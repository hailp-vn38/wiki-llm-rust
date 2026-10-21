use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use llm_wiki::{TreeOptions, TreeScope};
use orchestrator_agent_rs::{CallerContext, FeedItem, Orchestrator, RunStatus};
use serde_json::Value;

use crate::tools::SharedEnv;
use crate::Result;

// ---------------------------------------------------------------------------
// Đọc cấu hình `.env.live` & chạy Orchestrator
// ---------------------------------------------------------------------------

fn read_env_file(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().into(), value.into()))
        })
        .collect()
}

pub(crate) fn resolve_llm_credentials(env_file: &Path) -> Result<(String, String, String)> {
    let mut map = read_env_file(env_file);
    if map.is_empty() {
        let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("orchestrator_agent_rs/.env.live"));
        if let Some(sib) = sibling.filter(|p| p.exists()) {
            map = read_env_file(&sib);
        }
    }

    let get_key = |keys: &[&str]| -> Result<String> {
        for k in keys {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return Ok(v.trim().to_string());
                }
            }
            if let Some(v) = map.get(*k) {
                if !v.trim().is_empty() {
                    return Ok(v.trim().to_string());
                }
            }
        }
        Err(io::Error::other(format!(
            "Thiếu biến môi trường {:?} (hãy cấu hình trong .env.live)",
            keys
        ))
        .into())
    };

    let endpoint = get_key(&["G0_OPENAI_BASE_URL", "OPENAI_BASE_URL"])?;
    let model = get_key(&["G0_OPENAI_MODEL", "OPENAI_MODEL"])?;
    let api_key = get_key(&["G0_OPENAI_API_KEY", "OPENAI_API_KEY"])?;
    Ok((endpoint, model, api_key))
}

fn print_event(data: &Value, secret: &str) {
    let payload = &data["payload"];
    let session = data["session_id"].as_str().unwrap_or("");
    let tag = if session == data["root_session_id"].as_str().unwrap_or("") {
        "coordinator".to_string()
    } else {
        format!("ingestor:{}", session.chars().take(8).collect::<String>())
    };
    let tool = payload["tool_name"].as_str().unwrap_or("?");
    let line = match data["event_type"].as_str().unwrap_or("") {
        "tool.started" => format!("[{tag}] ▶ {tool}"),
        "tool.progress" => format!("[{tag}] … {}", payload["message"].as_str().unwrap_or("")),
        "tool.completed" => format!("[{tag}] ✔ {tool}"),
        "tool.failed" => format!(
            "[{tag}] ✘ {tool} ({}: {})",
            payload["error_code"].as_str().unwrap_or(""),
            payload["error"]
                .as_str()
                .or_else(|| payload["message"].as_str())
                .unwrap_or("")
        ),
        _ => return,
    };
    if !secret.is_empty() {
        println!("{}", line.replace(secret, "[redacted]"));
    } else {
        println!("{line}");
    }
}

pub(crate) async fn run_orchestrator_turn(
    orch: &Orchestrator,
    playlist_url: &str,
    secret: &str,
    timeout: Duration,
) -> Result<()> {
    let caller = CallerContext::new("wiki-maintainer", "youtube-playlist-wiki");
    let session_id = orch.create_session(&caller, "coordinator").await?;
    let mut sub = orch.subscribe(&caller, &session_id).await?;
    let mut current_prompt = format!(
        "Build a deep, non-fragmented Karpathy LLM Wiki from this YouTube playlist.\nPlaylist URL: {playlist_url}"
    );
    let mut retries = 0usize;

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        let turn = orch
            .submit(&caller, &session_id, current_prompt.clone())
            .await?;
        let mut refresh = tokio::time::interval(orch.snapshot_refresh_interval());
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                item = sub.recv() => match item {
                    Some(FeedItem::Event(event)) => print_event(&event.to_json(), secret),
                    Some(FeedItem::ResyncRequired) | None => sub = orch.subscribe(&caller, &session_id).await?,
                },
                _ = refresh.tick() => {
                    let result = orch.result(&caller, &session_id, &turn.turn_id).await?;
                    if result.status == RunStatus::Running {
                        continue;
                    }
                    if result.status != RunStatus::Completed {
                        let is_provider_err = result
                            .error
                            .as_ref()
                            .is_some_and(|e| e.code == "PROVIDER_UNAVAILABLE" || e.code == "PROVIDER_TIMEOUT");
                        if is_provider_err && retries < 3 {
                            retries += 1;
                            println!(
                                "\n[retry {retries}/3] Gateway trả về {:?}; đợi 5s rồi tiếp tục trên cùng session...",
                                result.error
                            );
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            current_prompt = "Continue from where you left off. Check `wiki/index.md` via `wiki_get` and complete the remaining steps.".to_string();
                            break;
                        }
                        return Err(io::Error::other(format!("Turn kết thúc với lỗi: {:?}", result.error)).into());
                    }
                    println!(
                        "\n=== [Kết quả từ Agent `coordinator` ({})] ===\n{}",
                        result.status.as_str(),
                        result.output.unwrap_or_default()
                    );
                    return Ok(());
                },
                _ = &mut deadline => {
                    orch.cancel(&caller, &session_id).await?;
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "Quá thời gian xử lý playlist; đã cancel").into());
                }
            }
        }
    }
}

/// Host chỉ đọc và xác minh kết quả trên đĩa sau khi Agent đã hoàn tất toàn bộ luồng.
pub(crate) fn verify_vault_on_disk(env: &SharedEnv) -> Result<()> {
    let videos = env.videos.lock().unwrap().clone();
    if videos.is_empty() {
        return Err(io::Error::other("Agent chưa gọi yt_list_playlist").into());
    }

    for v in &videos {
        let src_file = env.engine.config().sources_dir().join(v.source_scoped_path());
        let wiki_file = env
            .engine
            .config()
            .wiki_dir()
            .join("sources")
            .join(v.source_scoped_path());
        if !src_file.exists() {
            return Err(
                io::Error::other(format!("Thiếu Raw Source: {}", src_file.display())).into(),
            );
        }
        if !wiki_file.exists() {
            return Err(io::Error::other(format!(
                "Thiếu trang Wiki của video: {}",
                wiki_file.display()
            ))
            .into());
        }
    }

    let _ = env.engine.embed_pending(false)?;
    let _ = env.engine.rebuild_or_sync_index_md()?;
    let lint = env.engine.lint_wiki()?;
    let status = env.engine.status()?;
    let graph = env.engine.get_graph(None)?;

    println!("\n=== [Xác minh Vault sau khi Agent hoàn tất (`WikiEngine`)] ===");
    println!(
        "- Vault Root (Obsidian Vault): {}",
        env.engine.config().data_root.display()
    );
    println!(
        "- Tầng 1 (`sources/`): {} file Raw Source Markdown",
        status.sources_count
    );
    println!(
        "- Tầng 2 (`wiki/`): {} trang Wiki",
        status.wiki_count
    );
    println!(
        "- Obsidian Graph View: {} nodes, {} edges ({} resolved, {} unresolved)",
        graph.nodes.len(),
        graph.edges.len(),
        status.resolved_graph_links,
        status.unresolved_graph_links
    );
    println!(
        "- Wiki Lint Report: is_healthy={}, uningested_sources={}, broken_links={}, orphan_wiki_pages={}, stale_wiki_pages={}",
        lint.is_healthy,
        lint.uningested_sources.len(),
        lint.broken_links.len(),
        lint.orphan_wiki_pages.len(),
        lint.stale_wiki_pages.len()
    );

    let tree = env
        .engine
        .get_tree(TreeOptions::for_scope(TreeScope::All))?;
    println!("\n=== [Cây thư mục Knowledge Base (`TreeScope::All`)] ===");
    print!("{}", tree.to_ascii_tree());

    Ok(())
}

