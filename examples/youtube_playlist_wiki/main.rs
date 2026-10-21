//! App Example (Multi-Subagent + Reactive Auto-Ingest Flow + Universal Skills):
//!
//! Luồng thực thi hoàn toàn tự động theo 3 nguyên tắc:
//! 1. **Thu thập Source theo chủ đề (`source_collector`)**:
//!    - Khi nhận yêu cầu *"Hãy lấy thông tin chi tiết trong playlist và lưu vào source theo chủ đề"*,
//!      Subagent `source_collector` lấy thông tin + transcript từng video và tự động phân loại
//!      vào thư mục chủ đề `sources/<domain>/<topic>/<slug>.md` kèm bộ tag phân cấp.
//! 2. **Tự động kích hoạt Ingest khi có tài liệu mới trong `sources/` (`wiki_ingestor` & `wiki_curator`)**:
//!    - Ngay khi có file mới xuất hiện trong `sources/`, luồng Auto-Flow tự động kích hoạt
//!      Subagent `wiki_ingestor` (dùng skill `wiki-taxonomy` + `wiki-ingest`) để biên dịch sang `wiki/`
//!      và Subagent `wiki_curator` (dùng skill `wiki-query` + `wiki-lint`) để tổng hợp & kiểm toán.
//!    - Không dùng bất kỳ prompt cố định theo bài toán cụ thể nào.
//! 3. **Root Agent chỉ điều phối (`allowed_tools = ["subagent"]`)**:
//!    - Root Agent (`root`) không có bất kỳ tool xử lý trực tiếp nào, toàn bộ công việc
//!      do 3 Subagent chuyên biệt (`source_collector`, `wiki_ingestor`, `wiki_curator`) thực hiện.

mod prompts;
mod runner;
mod tools;
mod ytdlp;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llm_wiki::prompts::WikiTemplateId;
use llm_wiki::{is_read_only_wiki_tool, WikiEngine};
use orchestrator_agent_rs::{
    AgentDefinition, AgentMode, LlmBackend, LlmConfig, LlmProviderConfig, Orchestrator,
    OrchestratorConfig, ProviderAuth, SecretString, ToolEffect,
};

use prompts::{
    ROOT_DISPATCHER_PROMPT, SOURCE_COLLECTOR_PROMPT, WIKI_CURATOR_PROMPT, WIKI_INGESTOR_PROMPT,
};
use runner::{resolve_llm_credentials, run_orchestrator_turn, verify_vault_on_disk};
use tools::{
    AdaptiveReadBudget, SharedEnv, WikiEngineBridgeTool, YtFetchTranscriptTool, YtListPlaylistTool,
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut url: Option<String> = None;
    let mut langs = "en,vi".to_string();
    let mut limit = usize::MAX;
    let mut model_override: Option<String> = None;
    let mut env_file = std::env::var_os("LIVE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join(".env.live"));
    let mut output_root = manifest_dir.join("target/youtube-wiki-vault");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" | "--env-file" | "--vault-dir" | "--output-dir" | "--limit" | "--model" => {
                let v = args
                    .next()
                    .ok_or_else(|| io::Error::other(format!("Thiếu giá trị cho {arg}")))?;
                match arg.as_str() {
                    "--lang" => langs = v,
                    "--env-file" => env_file = PathBuf::from(v),
                    "--limit" => limit = v.parse()?,
                    "--model" => model_override = Some(v),
                    _ => output_root = PathBuf::from(v),
                }
            }
            "--help" | "-h" => {
                println!(
                    "Cách dùng:\n  cargo run --example youtube_playlist_wiki -- <PLAYLIST_URL> [--model <MODEL>] [--lang en,vi] [--limit N] [--env-file .env.live] [--vault-dir target/youtube-wiki-vault]"
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
    let (endpoint, env_model, api_key) = resolve_llm_credentials(&env_file)?;
    let model = model_override.unwrap_or(env_model);

    let run_id = &uuid::Uuid::new_v4().to_string()[..8];
    let run_dir = output_root.join(format!("run-{run_id}"));
    let vault_dir = run_dir.join("vault");
    let work_dir = run_dir.join("work");
    let store_dir = run_dir.join("orchestrator-store");
    let prompts_dir = run_dir.join("prompts");
    let skills_dir = manifest_dir.join("skills");
    std::fs::create_dir_all(&prompts_dir)?;
    std::fs::create_dir_all(&work_dir)?;

    // 1. Khởi tạo `WikiEngine` với chuẩn Universal Template (`WikiTemplateId::Research`)
    let engine = Arc::new(WikiEngine::open(&vault_dir)?);
    engine.apply_template(WikiTemplateId::Research)?;

    println!("=== Khởi tạo Multi-Subagent LLM Wiki (Topic Source Collection + Reactive Auto-Ingest) ===");
    println!("- Playlist URL : {playlist_url}");
    println!("- Model        : {model}");
    println!("- Skills Dir   : {}", skills_dir.display());
    println!("- Vault Path   : {}", vault_dir.display());

    // 2. Ghi file prompt cho Root Dispatcher (`root`) và 3 Subagent chuyên biệt
    let root_prompt_path = prompts_dir.join("root.md");
    let collector_prompt_path = prompts_dir.join("source_collector.md");
    let ingestor_prompt_path = prompts_dir.join("wiki_ingestor.md");
    let curator_prompt_path = prompts_dir.join("wiki_curator.md");
    std::fs::write(&root_prompt_path, ROOT_DISPATCHER_PROMPT)?;
    std::fs::write(&collector_prompt_path, SOURCE_COLLECTOR_PROMPT)?;
    std::fs::write(&ingestor_prompt_path, WIKI_INGESTOR_PROMPT)?;
    std::fs::write(&curator_prompt_path, WIKI_CURATOR_PROMPT)?;

    let shared_env = Arc::new(SharedEnv {
        engine: engine.clone(),
        work_dir,
        playlist_url: playlist_url.clone(),
        langs,
        limit,
        videos: Mutex::new(Vec::new()),
    });

    let context_limit_tokens: usize = std::env::var("WIKI_CONTEXT_LIMIT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let max_output_tokens: u32 = std::env::var("WIKI_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16_384);

    // 3. Khởi tạo `Orchestrator`:
    //    - `root` (Primary): CHỈ có quyền gọi tool `subagent` (`with_allowed_tools(["subagent"])`).
    //    - `source_collector` (Subagent): Chuyên lấy chi tiết playlist & lưu vào `sources/<domain>/<topic>/` theo chủ đề.
    //    - `wiki_ingestor` (Subagent): Chuyên Ingest file mới từ `sources/` vào `wiki/` theo skill `wiki-taxonomy` + `wiki-ingest`.
    //    - `wiki_curator` (Subagent): Chuyên tổng hợp `wiki/syntheses/` & chạy `wiki_lint` theo skill `wiki-query` + `wiki-lint`.
    let mut orch_cfg = OrchestratorConfig::new(
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
            AgentDefinition::new("root", "live", &model)
                .with_mode(AgentMode::Primary)
                .with_prompt_path(&root_prompt_path)
                .with_context_limit_tokens(context_limit_tokens)
                .with_max_output_tokens(max_output_tokens)
                .with_allowed_tools(["subagent"]),
            AgentDefinition::new("source_collector", "live", &model)
                .with_mode(AgentMode::Subagent)
                .with_description(
                    "Inspects external sources (e.g. YouTube playlist), extracts detailed transcripts/metadata, classifies topic hierarchy (<domain>/<topic>), and saves raw Markdown files into sources/<domain>/<topic>/<slug>.md",
                )
                .with_prompt_path(&collector_prompt_path)
                .with_context_limit_tokens(context_limit_tokens)
                .with_max_output_tokens(max_output_tokens)
                .with_allowed_skills(["wiki-taxonomy"])
                .with_allowed_tools([
                    "skill",
                    "yt_list_playlist",
                    "yt_fetch_transcript",
                    "wiki_tree",
                    "wiki_get",
                    "wiki_write_file",
                    "wiki_patch_file",
                ]),
            AgentDefinition::new("wiki_ingestor", "live", &model)
                .with_mode(AgentMode::Subagent)
                .with_description(
                    "Ingests a newly added source file from sources/<path>.md into wiki/ (wiki/sources/<path>.md, wiki/entities/..., wiki/concepts/...) using universal wiki-taxonomy and wiki-ingest skills",
                )
                .with_prompt_path(&ingestor_prompt_path)
                .with_context_limit_tokens(context_limit_tokens)
                .with_max_output_tokens(max_output_tokens)
                .with_allowed_skills(["wiki-taxonomy", "wiki-ingest"])
                .with_allowed_tools([
                    "skill",
                    "wiki_tree",
                    "wiki_get",
                    "wiki_multi_get",
                    "wiki_query",
                    "wiki_metadata",
                    "wiki_ingest_source",
                    "wiki_write_file",
                    "wiki_patch_file",
                ]),
            AgentDefinition::new("wiki_curator", "live", &model)
                .with_mode(AgentMode::Subagent)
                .with_description(
                    "Synthesizes cross-source insights into wiki/syntheses/ and runs wiki_lint repairs until is_healthy is true using universal wiki-query and wiki-lint skills",
                )
                .with_prompt_path(&curator_prompt_path)
                .with_context_limit_tokens(context_limit_tokens)
                .with_max_output_tokens(max_output_tokens)
                .with_allowed_skills(["wiki-taxonomy", "wiki-query", "wiki-lint"])
                .with_allowed_tools([
                    "skill",
                    "wiki_tree",
                    "wiki_get",
                    "wiki_multi_get",
                    "wiki_query",
                    "wiki_file_synthesis",
                    "wiki_write_file",
                    "wiki_patch_file",
                    "wiki_delete_file",
                    "wiki_lint",
                ]),
        ],
        &store_dir,
    )
    .with_skills_dir(&skills_dir)
    .with_project_instructions(engine.config().schema_path());

    // Tinh chỉnh Auto-Compaction & trần Tool Output kết hợp cơ chế đọc phân trang `doc_theo_dong` của `javis-os`:
    // - `context_limit_tokens = 200_000` (khớp với context window 200K–256K của `oc/nemotron-3-ultra-free`,
    //   có thể ghi đè qua biến môi trường `WIKI_CONTEXT_LIMIT_TOKENS` nếu dùng model nhỏ hơn).
    // - `compaction_threshold_percent = 85`: Chỉ nén khi chạm 85% ngân sách context (170K tokens).
    // - `compaction_keep_recent = 6`: Giữ nguyên 6 lượt trao đổi gần nhất nếu phiên kéo dài.
    // - `tool_output_cap_bytes = 24 * 1024`: Trần bảo vệ an toàn cho một tool output đơn lẻ.
    let read_budget = AdaptiveReadBudget::for_context_limit(context_limit_tokens);
    orch_cfg.runtime.context_limit_tokens = context_limit_tokens;
    orch_cfg.runtime.compaction_threshold_percent = 85;
    orch_cfg.runtime.compaction_keep_recent = 6;
    orch_cfg.runtime.tool_output_cap_bytes = (read_budget.max_window_chars * 3 / 2).max(24 * 1024);

    // 4. Đăng ký YouTube tools + toàn bộ 13 native tools của `WikiEngine`
    let mut builder = Orchestrator::builder(orch_cfg)
        .register_tool(Arc::new(YtListPlaylistTool {
            env: shared_env.clone(),
        }))
        .register_tool(Arc::new(YtFetchTranscriptTool {
            env: shared_env.clone(),
        }));

    for spec in engine.llm_tool_definitions() {
        let effect = if is_read_only_wiki_tool(&spec.name) {
            ToolEffect::ReadOnly
        } else {
            ToolEffect::Mutating
        };
        builder = builder.register_tool(Arc::new(WikiEngineBridgeTool {
            engine: engine.clone(),
            spec,
            effect,
            read_budget,
        }));
    }

    let orch = builder.build().await?;
    let turn_res = run_orchestrator_turn(
        &orch,
        &shared_env,
        &playlist_url,
        &api_key,
        Duration::from_secs(4800),
    )
    .await;
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
    fn test_long_vtt_wraps_into_multiple_lines_and_paginates_with_budget() {
        let mut long_vtt = String::from("WEBVTT\n\n");
        for i in 1..=25 {
            long_vtt.push_str(&format!(
                "00:00:{i:02}.000 --> 00:00:{:02}.000\nĐây là câu phụ đề thứ {i} dùng để kiểm tra chia dòng tự động cho transcript dài.\n\n",
                i + 1
            ));
        }
        let cleaned = vtt_to_text(&long_vtt);
        let line_count = cleaned.lines().count();
        assert!(
            line_count >= 10,
            "Long VTT must be wrapped into multiple lines for line-based pagination, got {line_count} lines"
        );

        let page1 = llm_wiki::slice_lines_with_budget(&cleaned, Some(1), Some(5), Some(1_000), true);
        assert!(page1.contains("Đã đọc dòng 1-5 trên tổng"));
        assert!(page1.contains("from_line=6"));

        let page_last =
            llm_wiki::slice_lines_with_budget(&cleaned, Some(6), Some(50), Some(10_000), true);
        assert!(page_last.contains("Đã đọc đến cuối file: dòng 6-"));
    }

    #[test]
    fn test_universal_skills_directory_has_valid_frontmatter() {
        let skills_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        for skill_id in [
            "wiki-taxonomy",
            "wiki-ingest",
            "wiki-query",
            "wiki-lint",
        ] {
            let p = skills_root.join(skill_id).join("SKILL.md");
            assert!(p.is_file(), "Missing skill file: {}", p.display());
            let content = std::fs::read_to_string(&p).unwrap();
            assert!(content.starts_with("---\nname: "));
            assert!(content.contains("\ndescription: "));
        }
    }
}
