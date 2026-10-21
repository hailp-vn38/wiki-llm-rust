use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use llm_wiki::{TreeOptions, TreeScope};
use orchestrator_agent_rs::{CallerContext, FeedItem, Orchestrator, RunStatus};
use serde_json::Value;

use crate::tools::SharedPdfEnv;
use crate::Result;

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

fn explain_error_code(code: &str) -> &'static str {
    match code {
        "CHILD_FAILED" => "subagent con không hoàn tất (do lỗi Provider rate-limit/timeout hoặc vượt ngưỡng token)",
        "PROVIDER_UNAVAILABLE" => "LLM Provider tạm thời lỗi hoặc chạm giới hạn Rate-Limit (HTTP 429/5xx)",
        "PROVIDER_TIMEOUT" => "LLM Provider phản hồi quá thời gian chờ",
        "CONTEXT_LIMIT" => "vượt quá giới hạn Context Window (token limit) của model",
        "WIKI_TOOL_ERROR" => "tham số hoặc đường dẫn tài liệu truyền vào Wiki tool chưa khớp",
        _ => "",
    }
}

fn print_event(data: &Value, secret: &str) {
    let payload = &data["payload"];
    let session = data["session_id"].as_str().unwrap_or("");
    let tag = if session == data["root_session_id"].as_str().unwrap_or("") {
        "root-dispatcher".to_string()
    } else {
        format!("subagent:{}", session.chars().take(8).collect::<String>())
    };
    let tool = payload["tool_name"].as_str().unwrap_or("?");
    let line = match data["event_type"].as_str().unwrap_or("") {
        "tool.started" => format!("[{tag}] ▶ {tool}"),
        "tool.progress" => format!("[{tag}] … {}", payload["message"].as_str().unwrap_or("")),
        "tool.completed" => format!("[{tag}] ✔ {tool}"),
        "tool.failed" | "tool.interrupted" => {
            let code = payload["error_code"]
                .as_str()
                .or_else(|| payload["status"].as_str())
                .unwrap_or("UNKNOWN");
            let detail = payload["error"]
                .as_str()
                .or_else(|| payload["error"]["message"].as_str())
                .or_else(|| payload["message"].as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| explain_error_code(code));
            if detail.is_empty() {
                format!("[{tag}] ✘ {tool} ({code})")
            } else {
                format!("[{tag}] ✘ {tool} ({code}: {detail})")
            }
        }
        "compaction.started" => {
            format!("[{tag}] ⟳ compaction.started (đang tóm tắt lịch sử context để giải phóng token...)")
        }
        "compaction.completed" => {
            format!("[{tag}] ✔ compaction.completed (đã nén lịch sử hội thoại thành bản tóm tắt)")
        }
        "run.failed" => {
            let code = payload["error_code"].as_str().unwrap_or("UNKNOWN");
            let hint = explain_error_code(code);
            if hint.is_empty() {
                format!("[{tag}] ⚠ run.failed ({code})")
            } else {
                format!("[{tag}] ⚠ run.failed ({code}: {hint})")
            }
        }
        _ => return,
    };
    if !secret.is_empty() {
        println!("{}", line.replace(secret, "[redacted]"));
    } else {
        println!("{line}");
    }
}

fn build_initial_pdf_prompt(env: &SharedPdfEnv) -> String {
    let files_desc = env
        .input_paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "{}. `{}`",
                i + 1,
                p.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("document.pdf")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Hãy dùng `anydoc_inspect_pdf` và `anydoc_save_to_sources` để chuyển đổi các tài liệu PDF sau sang Markdown, phân loại vào thư mục chủ đề `sources/<domain>/<topic>/` và biên dịch sang `wiki/`:\n{files_desc}"
    )
}

fn build_next_autoflow_prompt(env: &SharedPdfEnv) -> Result<Option<String>> {
    let initial = build_initial_pdf_prompt(env);
    let next = env.engine.build_next_autoflow_prompt(Some(&initial))?;
    if let Some(ref p) = next {
        if p.starts_with("[AUTO-FLOW: NEW SOURCES DETECTED]") {
            println!(
                "\n[Auto-Flow Watcher] Phát hiện bản Markdown từ PDF mới trong `sources/` chưa được Ingest vào `wiki/` -> Điều phối subagent `wiki_ingestor`..."
            );
        } else if p.starts_with("[AUTO-FLOW: WIKI LINT]") {
            println!(
                "\n[Auto-Flow Watcher] Điều phối subagent `wiki_curator` để tổng hợp Synthesis theo chủ đề & chạy `wiki_lint`..."
            );
        }
    }
    Ok(next)
}

pub(crate) async fn run_orchestrator_turn(
    orch: &Orchestrator,
    env: &SharedPdfEnv,
    secret: &str,
    timeout: Duration,
) -> Result<()> {
    let caller = CallerContext::new("wiki-maintainer", "pdf-anydoc-wiki");
    let mut session_id = orch.create_session(&caller, "root").await?;
    let mut sub = orch.subscribe(&caller, &session_id).await?;

    let mut current_prompt = build_initial_pdf_prompt(env);
    let mut retries = 0usize;
    let max_retries = 5usize;
    let mut auto_flow_rounds = 0usize;

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
                        let is_recoverable = result
                            .error
                            .as_ref()
                            .is_some_and(|e| {
                                matches!(
                                    e.code.as_str(),
                                    "PROVIDER_UNAVAILABLE" | "PROVIDER_TIMEOUT" | "CONTEXT_LIMIT" | "CHILD_FAILED"
                                )
                            });
                        if is_recoverable && retries < max_retries {
                            retries += 1;
                            let backoff_secs = match retries {
                                1 => 10,
                                2 => 20,
                                3 => 35,
                                4 => 50,
                                _ => 60,
                            };
                            println!(
                                "\n[retry {retries}/{max_retries}] Turn dừng do {:?}; đợi {backoff_secs}s rồi tạo session điều phối mới với danh sách công việc còn lại...",
                                result.error
                            );
                            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;

                            session_id = orch.create_session(&caller, "root").await?;
                            sub = orch.subscribe(&caller, &session_id).await?;
                            if let Some(next_prompt) = build_next_autoflow_prompt(env)? {
                                current_prompt = next_prompt;
                                break;
                            } else {
                                return Ok(());
                            }
                        }
                        return Err(io::Error::other(format!("Turn kết thúc với lỗi: {:?}", result.error)).into());
                    }

                    retries = 0;

                    println!(
                        "\n=== [Kết quả Turn từ `root` ({})] ===\n{}",
                        result.status.as_str(),
                        result.output.unwrap_or_default()
                    );

                    if auto_flow_rounds < 8 {
                        if let Some(next_prompt) = build_next_autoflow_prompt(env)? {
                            auto_flow_rounds += 1;
                            session_id = orch.create_session(&caller, "root").await?;
                            sub = orch.subscribe(&caller, &session_id).await?;
                            current_prompt = next_prompt;
                            break;
                        }
                    }

                    return Ok(());
                },
                _ = &mut deadline => {
                    orch.cancel(&caller, &session_id).await?;
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "Quá thời gian xử lý PDF; đã cancel").into());
                }
            }
        }
    }
}

pub(crate) fn verify_vault_on_disk(env: &SharedPdfEnv) -> Result<()> {
    let docs = env.docs.lock().unwrap().clone();
    if docs.is_empty() {
        return Err(io::Error::other("Subagent chưa gọi anydoc_inspect_pdf").into());
    }

    for d in &docs {
        let scoped = d.saved_source_scoped_path.as_deref().ok_or_else(|| {
            io::Error::other(format!(
                "Tài liệu PDF `{}` chưa được lưu vào `sources/` theo chủ đề",
                d.file_name
            ))
        })?;
        let src_file = env.engine.config().sources_dir().join(scoped);
        let wiki_file = env
            .engine
            .config()
            .wiki_dir()
            .join("sources")
            .join(scoped);
        if !src_file.exists() {
            return Err(
                io::Error::other(format!("Thiếu Raw Source: {}", src_file.display())).into(),
            );
        }
        if !wiki_file.exists() {
            return Err(io::Error::other(format!(
                "Thiếu trang Wiki của tài liệu PDF: {}",
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

    println!("\n=== [Xác minh Vault sau khi xử lý PDF qua `anydoc` -> Agent -> `WikiEngine`] ===");
    println!(
        "- Vault Root (Obsidian Vault): {}",
        env.engine.config().data_root.display()
    );
    println!(
        "- Tầng 1 (`sources/`): {} file Raw Source Markdown (chuyển đổi từ PDF bằng `anydoc`, phân loại theo `<domain>/<topic>`)",
        status.sources_count
    );
    println!("- Tầng 2 (`wiki/`): {} trang Wiki", status.wiki_count);
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
