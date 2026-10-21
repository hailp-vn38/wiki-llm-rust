//! App Example (100% Agent-Driven + Skills):
//! Live YouTube Playlist -> Raw Source Markdown -> Karpathy LLM Wiki
//! sử dụng `orchestrator-agent` (`orchestrator_agent_rs`) + `llm-wiki` (`WikiEngine`).
//!
//! Điểm cốt lõi của thiết kế nâng cấp:
//! 1. **100% do Agent tự xử lý toàn bộ Flow (Host KHÔNG làm thay)**:
//!    - Host chỉ đăng ký các tool nguyên bản:
//!      - `yt_list_playlist`: Lấy danh sách video từ URL playlist bằng `yt-dlp`.
//!      - `yt_fetch_transcript`: Agent tự gọi để tải phụ đề `.vtt` bằng `yt-dlp`, tạo file
//!        Raw Source bất biến tại `sources/youtube/<slug>.md` và nhận toàn bộ nội dung transcript.
//!      - Bộ tool nguyên bản của `WikiEngine` (`wiki_tree`, `wiki_get`, `wiki_multi_get`,
//!        `wiki_query`, `wiki_ingest_source`, `wiki_write_file`, `wiki_patch_file`,
//!        `wiki_delete_file`, `wiki_file_synthesis`, `wiki_lint`, `wiki_status`, `wiki_log`).
//! 2. **Tích hợp hệ thống Skills (`skills/<id>/SKILL.md`) của `orchestrator-agent`**:
//!    - `karpathy-wiki-taxonomy`: Quy hoạch bộ khung Taxonomy chuẩn (Canonical Entities & Concepts)
//!      chống phân mảnh/trùng lặp tên file, phân tách rõ `entities/` và `concepts/`.
//!    - `deep-video-wiki-ingest`: Hướng dẫn Agent khai thác sâu toàn bộ transcript (thông số kỹ thuật,
//!      logic code/thuật toán, kịch bản backtest, mâu thuẫn `[!WARNING] Contradiction`, sơ đồ Mermaid)
//!      và **tự động liên kết tới các trang video đã có sẵn trong Wiki**.
//!    - `karpathy-wiki-synthesis-lint`: Hướng dẫn Agent tổng hợp bảng đối chiếu xuyên suốt Playlist,
//!      lưu vào `wiki/syntheses/`, và tự chạy `wiki_lint` + sửa lỗi cho tới khi `is_healthy = true`.

mod prompts;
mod runner;
mod tools;
mod ytdlp;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llm_wiki::{CollectionScope, WikiEngine};
use orchestrator_agent_rs::{
    AgentDefinition, AgentMode, LlmBackend, LlmConfig, LlmProviderConfig, Orchestrator,
    OrchestratorConfig, ProviderAuth, SecretString, ToolEffect,
};

use prompts::{COORDINATOR_PROMPT, VIDEO_INGESTOR_PROMPT, YOUTUBE_WIKI_SCHEMA_MD};
use runner::{resolve_llm_credentials, run_orchestrator_turn, verify_vault_on_disk};
use tools::{
    SharedEnv, WikiDeleteFileTool, WikiEngineBridgeTool, YtFetchTranscriptTool, YtListPlaylistTool,
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut url: Option<String> = None;
    let mut langs = "en,vi".to_string();
    let mut limit = usize::MAX;
    let mut env_file = std::env::var_os("LIVE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join(".env.live"));
    let mut output_root = manifest_dir.join("target/youtube-wiki-vault");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" | "--env-file" | "--vault-dir" | "--output-dir" | "--limit" => {
                let v = args
                    .next()
                    .ok_or_else(|| io::Error::other(format!("Thiếu giá trị cho {arg}")))?;
                match arg.as_str() {
                    "--lang" => langs = v,
                    "--env-file" => env_file = PathBuf::from(v),
                    "--limit" => limit = v.parse()?,
                    _ => output_root = PathBuf::from(v),
                }
            }
            "--help" | "-h" => {
                println!(
                    "Cách dùng:\n  cargo run --example youtube_playlist_wiki -- <PLAYLIST_URL> [--lang en,vi] [--limit N] [--env-file .env.live] [--vault-dir target/youtube-wiki-vault]"
                );
                return Ok(());
            }
            s if !s.starts_with('-') && url.is_none() => url = Some(s.to_string()),
            other => {
                return Err(
                    io::Error::other(format!("Tham số không hợp lệ: {other} (dùng --help)")).into(),
                )
            }
        }
    }

    let playlist_url =
        url.ok_or_else(|| io::Error::other("Thiếu <PLAYLIST_URL>; dùng --help để xem hướng dẫn"))?;
    let (endpoint, model, api_key) = resolve_llm_credentials(&env_file)?;

    let run_id = &uuid::Uuid::new_v4().to_string()[..8];
    let run_dir = output_root.join(format!("run-{run_id}"));
    let vault_dir = run_dir.join("vault");
    let work_dir = run_dir.join("work");
    let store_dir = run_dir.join("orchestrator-store");
    let prompts_dir = run_dir.join("prompts");
    let skills_dir = manifest_dir.join("skills");
    std::fs::create_dir_all(&prompts_dir)?;
    std::fs::create_dir_all(&work_dir)?;

    // 1. Khởi tạo `WikiEngine`
    let engine = Arc::new(WikiEngine::open(&vault_dir)?);
    engine.set_schema_md(YOUTUBE_WIKI_SCHEMA_MD)?;
    engine.set_path_context(
        Some(CollectionScope::Sources),
        "/youtube",
        "Raw immutable YouTube video transcripts fetched via yt-dlp",
    )?;
    engine.set_path_context(
        Some(CollectionScope::Wiki),
        "/sources/youtube",
        "Deep technical LLM-compiled video wiki pages preserving raw source filenames",
    )?;
    engine.set_path_context(
        Some(CollectionScope::Wiki),
        "/entities",
        "Canonical entity pages (people, platforms, languages, indicators, datasets)",
    )?;
    engine.set_path_context(
        Some(CollectionScope::Wiki),
        "/concepts",
        "Canonical concept pages (strategies, algorithms, architectural patterns, risk principles)",
    )?;
    engine.set_path_context(
        Some(CollectionScope::Wiki),
        "/syntheses",
        "Cross-video comparative syntheses, tables, and knowledge maps",
    )?;

    println!("=== Khởi tạo Live YouTube Playlist LLM Wiki (100% Agent-Driven + Skills) ===");
    println!("- Playlist URL : {playlist_url}");
    println!("- Model        : {model}");
    println!("- Skills Dir   : {}", skills_dir.display());
    println!("- Vault Path   : {}", vault_dir.display());

    // 2. Ghi file prompt cho Root Agent (`coordinator`) và Subagent (`video_ingestor`)
    let coordinator_prompt_path = prompts_dir.join("coordinator.md");
    let ingestor_prompt_path = prompts_dir.join("video_ingestor.md");
    std::fs::write(&coordinator_prompt_path, COORDINATOR_PROMPT)?;
    std::fs::write(&ingestor_prompt_path, VIDEO_INGESTOR_PROMPT)?;

    let shared_env = Arc::new(SharedEnv {
        engine: engine.clone(),
        work_dir,
        playlist_url: playlist_url.clone(),
        langs,
        limit,
        videos: Mutex::new(Vec::new()),
    });

    // 3. Khởi tạo `Orchestrator` với `.with_skills_dir(...)` và `.with_project_instructions(SCHEMA.md)`
    let orch_cfg = OrchestratorConfig::new(
        LlmConfig {
            providers: vec![LlmProviderConfig {
                id: "live".into(),
                backend: LlmBackend::OpenAiCompatible,
                endpoint: Some(endpoint),
                auth: ProviderAuth::ApiKey(SecretString::new(api_key.clone())),
                request_timeout: Duration::from_secs(240),
            }],
        },
        vec![
            AgentDefinition::new("coordinator", "live", &model)
                .with_mode(AgentMode::Primary)
                .with_prompt_path(&coordinator_prompt_path)
                .with_context_limit_tokens(7_000)
                .with_allowed_skills([
                    "karpathy-wiki-taxonomy",
                    "deep-video-wiki-ingest",
                    "karpathy-wiki-synthesis-lint",
                ])
                .with_allowed_tools([
                    "skill",
                    "subagent",
                    "yt_list_playlist",
                    "wiki_get",
                    "wiki_write_file",
                    "wiki_patch_file",
                    "wiki_delete_file",
                    "wiki_file_synthesis",
                    "wiki_lint",
                ]),
            AgentDefinition::new("video_ingestor", "live", &model)
                .with_mode(AgentMode::Subagent)
                .with_description(
                    "Fetches one YouTube video's transcript via yt_fetch_transcript, writes a deep technical wiki page via wiki_ingest_source, and compounds canonical entities/concepts and cross-video links",
                )
                .with_prompt_path(&ingestor_prompt_path)
                .with_context_limit_tokens(7_000)
                .with_allowed_skills(["karpathy-wiki-taxonomy", "deep-video-wiki-ingest"])
                .with_allowed_tools([
                    "skill",
                    "yt_fetch_transcript",
                    "wiki_get",
                    "wiki_ingest_source",
                    "wiki_write_file",
                    "wiki_patch_file",
                ]),
        ],
        &store_dir,
    )
    .with_skills_dir(&skills_dir)
    .with_project_instructions(engine.config().schema_path());

    // 4. Đăng ký 2 YouTube tools + toàn bộ native tools của `WikiEngine`
    let mut builder = Orchestrator::builder(orch_cfg)
        .register_tool(Arc::new(YtListPlaylistTool {
            env: shared_env.clone(),
        }))
        .register_tool(Arc::new(YtFetchTranscriptTool {
            env: shared_env.clone(),
        }))
        .register_tool(Arc::new(WikiDeleteFileTool {
            engine: engine.clone(),
        }));

    for spec in engine.llm_tool_definitions() {
        let effect = match spec.name.as_str() {
            "wiki_tree" | "wiki_query" | "wiki_get" | "wiki_multi_get" | "wiki_metadata"
            | "wiki_status" | "wiki_graph_neighbors" => ToolEffect::ReadOnly,
            _ => ToolEffect::Mutating,
        };
        builder = builder.register_tool(Arc::new(WikiEngineBridgeTool {
            engine: engine.clone(),
            spec,
            effect,
        }));
    }

    let orch = builder.build().await?;
    let turn_res =
        run_orchestrator_turn(&orch, &playlist_url, &api_key, Duration::from_secs(2400)).await;
    let shutdown_res = orch.shutdown().await;
    turn_res?;
    shutdown_res?;

    // 5. Xác minh kết quả trên đĩa
    verify_vault_on_disk(&shared_env)?;
    println!("\n[Hoàn tất] Vault: {}", vault_dir.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ytdlp::{slugify, vtt_to_text};

    #[test]
    fn test_slugify_and_vtt_cleaning() {
        assert_eq!(
            slugify("Chiến lược DCA của Nick Shawn", 60),
            "chien-luoc-dca-cua-nick-shawn"
        );
        let sample_vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHello <c>world</c>\n\n00:00:03.000 --> 00:00:05.000\nHello world\nLine 2 &amp; more\n";
        assert_eq!(vtt_to_text(sample_vtt), "Hello world Line 2 & more");
    }

    #[test]
    fn test_skills_directory_has_valid_frontmatter() {
        let skills_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        for skill_id in [
            "karpathy-wiki-taxonomy",
            "deep-video-wiki-ingest",
            "karpathy-wiki-synthesis-lint",
        ] {
            let p = skills_root.join(skill_id).join("SKILL.md");
            assert!(p.is_file(), "Missing skill file: {}", p.display());
            let content = std::fs::read_to_string(&p).unwrap();
            assert!(content.starts_with("---\nname: "));
            assert!(content.contains("\ndescription: "));
        }
    }
}

