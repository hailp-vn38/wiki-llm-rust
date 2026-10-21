//! App Example: `PDF -> anydoc (GFM Markdown) -> Multi-Subagent (orchestrator-agent) -> Karpathy LLM Wiki`
//!
//! Luồng thực thi tự động:
//! 1. **Chuyển đổi tài liệu PDF sang Markdown bằng `anydoc` (`anydoc::to_markdown_bytes`)**:
//!    - Nhận diện định dạng thật từ chữ ký byte (`Format::from_bytes`), trích xuất văn bản & cấu trúc
//!      đề mục sang GitHub-Flavored Markdown, chuẩn hóa độ dài dòng để đọc phân trang (`wiki_get`).
//! 2. **Thu thập & Phân loại theo chủ đề (`source_collector`)**:
//!    - Subagent `source_collector` gọi `anydoc_inspect_pdf` để xem cấu trúc đề mục + bản xem trước Markdown,
//!      xác định `<domain>/<topic>` + bộ tag 3 tầng, rồi gọi `anydoc_save_to_sources` lưu vào
//!      `sources/<domain>/<topic>/<file_stem>.md`.
//! 3. **Tự động Ingest & Tổng hợp Đa chủ đề (`wiki_ingestor` & `wiki_curator`)**:
//!    - Luồng Reactive Auto-Flow (`WikiEngine::build_next_autoflow_prompt`) tự động ưu tiên Compact Index
//!      theo chủ đề đang xử lý, gọi `wiki_ingestor` biên dịch sang `wiki/sources/<domain>/<topic>/<file_stem>.md`
//!      (giữ nguyên tên file gốc) kèm `wiki/entities/...`, `wiki/concepts/...`, rồi gọi `wiki_curator`
//!      tạo bài tổng hợp `wiki/syntheses/<topic>-synthesis.md` và kiểm toán `wiki_lint` (`is_healthy: true`).

mod pdf_doc;
mod runner;
mod tools;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llm_wiki::prompts::WikiTemplateId;
use llm_wiki::{
    is_read_only_wiki_tool, WikiEngine, ROOT_DISPATCHER_PROMPT, SOURCE_COLLECTOR_PROMPT,
    WIKI_CURATOR_PROMPT, WIKI_INGESTOR_PROMPT,
};
use orchestrator_agent_rs::{
    AgentDefinition, AgentMode, LlmBackend, LlmConfig, LlmProviderConfig, Orchestrator,
    OrchestratorConfig, ProviderAuth, SecretString, ToolEffect,
};

use pdf_doc::build_sample_technical_pdf;
use runner::{resolve_llm_credentials, run_orchestrator_turn, verify_vault_on_disk};
use tools::{
    AdaptiveReadBudget, AnydocInspectPdfTool, AnydocSaveToSourcesTool, SharedPdfEnv,
    WikiEngineBridgeTool,
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut input_paths: Vec<PathBuf> = Vec::new();
    let mut use_sample_pdf = false;
    let mut max_chars: Option<usize> = Some(32_000);
    let mut model_override: Option<String> = None;
    let mut env_file = std::env::var_os("LIVE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join(".env.live"));
    let mut output_root = manifest_dir.join("target/pdf-wiki-vault");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--env-file" | "--vault-dir" | "--output-dir" | "--model" | "--max-chars" => {
                let v = args
                    .next()
                    .ok_or_else(|| io::Error::other(format!("Thiếu giá trị cho {arg}")))?;
                match arg.as_str() {
                    "--env-file" => env_file = PathBuf::from(v),
                    "--model" => model_override = v.into(),
                    "--max-chars" => {
                        let parsed: usize = v.parse()?;
                        max_chars = if parsed == 0 { None } else { Some(parsed) };
                    }
                    _ => output_root = PathBuf::from(v),
                }
            }
            "--repair-vault" => {
                let v = args
                    .next()
                    .ok_or_else(|| io::Error::other("Thiếu giá trị cho --repair-vault"))?;
                let vault_path = PathBuf::from(v);
                repair_existing_vault(&vault_path)?;
                return Ok(());
            }
            "--sample-pdf" => {
                use_sample_pdf = true;
            }
            "--help" | "-h" => {
                println!(
                    "Cách dùng:\n  \
                     cargo run --example pdf_anydoc_wiki -- <PDF_PATH> [MORE_PDF_PATHS...] [--max-chars 60000] [--model <MODEL>] [--env-file .env.live] [--vault-dir target/pdf-wiki-vault]\n  \
                     cargo run --example pdf_anydoc_wiki -- --sample-pdf"
                );
                return Ok(());
            }
            s if !s.starts_with('-') => input_paths.push(PathBuf::from(s)),
            other => {
                return Err(
                    io::Error::other(format!("Tham số không hợp lệ: {other} (dùng --help)")).into(),
                )
            }
        }
    }

    if input_paths.is_empty() && !use_sample_pdf {
        return Err(io::Error::other(
            "Thiếu đường dẫn file PDF (hoặc truyền `--sample-pdf` để tự tạo file PDF mẫu chạy thử); dùng `--help` để xem hướng dẫn",
        )
        .into());
    }

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

    if use_sample_pdf {
        let sample_path = work_dir.join("hybrid-retrieval-rrf-architecture.pdf");
        std::fs::write(&sample_path, build_sample_technical_pdf())?;
        input_paths.push(sample_path);
    }

    // 1. Khởi tạo `WikiEngine` với chuẩn Universal Template (`WikiTemplateId::Research`)
    let engine = Arc::new(WikiEngine::open(&vault_dir)?);
    engine.apply_template(WikiTemplateId::Research)?;

    let shared_env = Arc::new(SharedPdfEnv {
        engine: engine.clone(),
        work_dir: work_dir.clone(),
        input_paths: input_paths.clone(),
        max_chars,
        docs: Mutex::new(Vec::new()),
    });

    // Chuyển đổi trước qua `anydoc` để xác minh file PDF hợp lệ và in thông tin tóm tắt
    let converted_docs = shared_env
        .ensure_converted()
        .map_err(|e| io::Error::other(format!("{}: {}", e.code, e.message)))?;

    println!("=== Khởi tạo Luồng `PDF -> anydoc (Markdown) -> Agent -> LLM Wiki` ===");
    println!("- Model        : {model}");
    println!("- Skills Dir   : {}", skills_dir.display());
    println!("- Vault Path   : {}", vault_dir.display());
    for doc in &converted_docs {
        println!(
            "- Tài liệu #{}: `{}` (format: `{}`, title: {:?}, {} ký tự, {} dòng, {} headings)",
            doc.index,
            doc.file_name,
            doc.format_name,
            doc.inferred_title,
            doc.char_count,
            doc.line_count,
            doc.outline_headings.len()
        );
    }

    // 2. Ghi file prompt cho Root Dispatcher (`root`) và 3 Subagent chuyên biệt
    let root_prompt_path = prompts_dir.join("root.md");
    let collector_prompt_path = prompts_dir.join("source_collector.md");
    let ingestor_prompt_path = prompts_dir.join("wiki_ingestor.md");
    let curator_prompt_path = prompts_dir.join("wiki_curator.md");
    std::fs::write(&root_prompt_path, ROOT_DISPATCHER_PROMPT)?;
    std::fs::write(&collector_prompt_path, SOURCE_COLLECTOR_PROMPT)?;
    std::fs::write(&ingestor_prompt_path, WIKI_INGESTOR_PROMPT)?;
    std::fs::write(&curator_prompt_path, WIKI_CURATOR_PROMPT)?;

    let context_limit_tokens: usize = std::env::var("WIKI_CONTEXT_LIMIT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let max_output_tokens: u32 = std::env::var("WIKI_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16_384);

    // 3. Khởi tạo `Orchestrator` với `root`, `source_collector`, `wiki_ingestor`, `wiki_curator`
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
                    "Inspects PDF/document files converted to Markdown by `anydoc`, classifies topic hierarchy (<domain>/<topic>), and saves raw Markdown files into sources/<domain>/<topic>/<file_stem>.md",
                )
                .with_prompt_path(&collector_prompt_path)
                .with_context_limit_tokens(context_limit_tokens)
                .with_max_output_tokens(max_output_tokens)
                .with_allowed_skills(["wiki-taxonomy"])
                .with_allowed_tools([
                    "skill",
                    "anydoc_inspect_pdf",
                    "anydoc_save_to_sources",
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
                    "Synthesizes cross-source insights per topic into wiki/syntheses/ and runs wiki_lint repairs until is_healthy is true using universal wiki-query and wiki-lint skills",
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

    let read_budget = AdaptiveReadBudget::for_context_limit(context_limit_tokens);
    orch_cfg.runtime.context_limit_tokens = context_limit_tokens;
    orch_cfg.runtime.compaction_threshold_percent = 85;
    orch_cfg.runtime.compaction_keep_recent = 6;
    orch_cfg.runtime.tool_output_cap_bytes = (read_budget.max_window_chars * 3 / 2).max(24 * 1024);

    // 4. Đăng ký 2 tools `anydoc` + 13 native tools của `WikiEngine`
    let mut builder = Orchestrator::builder(orch_cfg)
        .register_tool(Arc::new(AnydocInspectPdfTool {
            env: shared_env.clone(),
        }))
        .register_tool(Arc::new(AnydocSaveToSourcesTool {
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
        &api_key,
        Duration::from_secs(3600),
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

fn repair_existing_vault(vault_path: & std::path::Path) -> Result<()> {
    fn walk_md(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk_md(&path, out)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(path);
            }
        }
        Ok(())
    }

    let wiki_dir = vault_path.join("wiki");
    let mut md_files = Vec::new();
    walk_md(&wiki_dir, &mut md_files)?;
    md_files.sort();

    let mut sanitized_count = 0usize;
    let mut moved_count = 0usize;
    for file in &md_files {
        let raw = std::fs::read_to_string(file)?;
        let clean = llm_wiki::fs::file_ops::sanitize_mermaid_fences(&raw);
        if clean != raw {
            std::fs::write(file, &clean)?;
            sanitized_count += 1;
        }

        // Nếu file nằm phẳng trực tiếp trong `wiki/entities/<slug>.md`, `wiki/concepts/<slug>.md`, hoặc `wiki/syntheses/<slug>.md`,
        // di chuyển vào thư mục `<domain>` (hoặc `shared` cho công cụ đa ngành như `anydoc.md`).
        if let Ok(rel_to_wiki) = file.strip_prefix(&wiki_dir) {
            let rel_str = rel_to_wiki.to_string_lossy().replace('\\', "/");
            for cat in ["entities", "concepts", "syntheses"] {
                let prefix = format!("{cat}/");
                if let Some(rest) = rel_str.strip_prefix(&prefix) {
                    if !rest.contains('/') {
                        let domain = if cat == "entities" && rest == "anydoc.md" {
                            Some("shared".to_string())
                        } else {
                            llm_wiki::db::index_store::extract_domain_from_markdown_frontmatter(
                                &clean,
                            )
                        };
                        if let Some(dom) = domain {
                            let new_path = wiki_dir.join(cat).join(&dom).join(rest);
                            if let Some(parent) = new_path.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            std::fs::rename(file, &new_path)?;
                            moved_count += 1;
                        }
                    }
                    break;
                }
            }
        }
    }

    let engine = WikiEngine::open(vault_path)?;
    let _ = engine.rebuild_or_sync_index_md()?;
    let lint_report = engine.lint_wiki()?;
    let status = engine.status()?;
    println!(
        "[Repair Vault] Đã chuẩn hóa {} file, di chuyển {} file vào thư mục <domain>/ trong {} (sources={}, wiki_pages={}, resolved_links={}, unresolved_links={}).",
        sanitized_count,
        moved_count,
        vault_path.display(),
        status.sources_count,
        status.wiki_count,
        status.resolved_graph_links,
        status.unresolved_graph_links,
    );
    println!(
        "[Lint Report] is_healthy={}, broken_links={}, orphan_wiki_pages={}, uningested_sources={}",
        lint_report.is_healthy,
        lint_report.broken_links.len(),
        lint_report.orphan_wiki_pages.len(),
        lint_report.uningested_sources.len()
    );
    if !lint_report.broken_links.is_empty() {
        for bl in &lint_report.broken_links {
            println!("  - Broken link in `{}` -> `{}`", bl.source_rel_path, bl.raw_target);
        }
    }
    let tree = engine.get_tree(llm_wiki::TreeOptions::for_scope(llm_wiki::TreeScope::All))?;
    println!("\n=== [Cây thư mục Knowledge Base sau khi nâng cấp] ===");
    print!("{}", tree.to_ascii_tree());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf_doc::convert_document_with_anydoc;
    use llm_wiki::WriteTopicSourceRequest;
    use serde_json::json;

    #[test]
    fn test_anydoc_converts_pdf_to_markdown_and_saves_to_wiki_sources() {
        let temp_dir = std::env::temp_dir().join(format!(
            "llm_wiki_anydoc_pdf_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let pdf_path = temp_dir.join("hybrid-retrieval-rrf-architecture.pdf");
        std::fs::write(&pdf_path, build_sample_technical_pdf()).unwrap();

        let converted = convert_document_with_anydoc(1, &pdf_path, Some(50_000)).unwrap();
        assert_eq!(converted.format_name, "pdf");
        assert_eq!(converted.file_stem, "hybrid-retrieval-rrf-architecture");
        assert!(converted
            .markdown
            .contains("Reciprocal Rank Fusion"));
        assert!(converted.markdown.contains("SQLite FTS5"));

        let vault_dir = temp_dir.join("vault");
        let engine = WikiEngine::open(&vault_dir).unwrap();
        let saved = engine
            .write_topic_source(WriteTopicSourceRequest {
                topic_folder: "ai-engineering/hybrid-search".to_string(),
                file_stem: Some(converted.file_stem.clone()),
                title: converted.inferred_title.clone(),
                raw_body: converted.markdown.clone(),
                body_heading: Some("Extracted Document Markdown (via anydoc)".to_string()),
                tags: vec!["pdf".to_string(), "anydoc".to_string(), "rrf".to_string()],
                extra_frontmatter: vec![
                    ("source_format".to_string(), json!("pdf")),
                    ("converter".to_string(), json!("anydoc")),
                ],
                header_bullets: vec![(
                    "File gốc".to_string(),
                    format!("`{}`", converted.file_name),
                )],
                preview_chars: Some(400),
            })
            .unwrap();

        assert_eq!(
            saved.info.rel_path,
            "sources/ai-engineering/hybrid-search/hybrid-retrieval-rrf-architecture.md"
        );
        assert_eq!(
            saved.expected_wiki_path,
            "wiki/sources/ai-engineering/hybrid-search/hybrid-retrieval-rrf-architecture.md"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
