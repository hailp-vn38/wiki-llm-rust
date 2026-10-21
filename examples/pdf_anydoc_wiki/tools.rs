use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub(crate) use llm_wiki::AdaptiveReadBudget;
use llm_wiki::{
    clip_transcript, sanitize_topic_folder, slugify, LlmToolSpec, WikiEngine,
    WriteTopicSourceRequest,
};
use orchestrator_agent_rs::{
    HostTool, ToolContext, ToolDescriptor, ToolEffect, ToolError, ToolOutcome,
};
use serde_json::{json, Value};

use crate::pdf_doc::{convert_document_with_anydoc, ConvertedPdfDoc};

/// Giới hạn ký tự cho bản xem trước khi `source_collector` kiểm tra hoặc lưu PDF Markdown vào `sources/`.
const COLLECTOR_PREVIEW_CHARS: usize = 900;

pub(crate) fn tool_err(code: &str, message: impl Into<String>) -> ToolError {
    ToolError {
        code: code.into(),
        message: message.into(),
    }
}

fn outcome(ctx: &ToolContext, content: String, metadata: Value) -> ToolOutcome {
    ToolOutcome {
        call_id: ctx.scope().tool_call_id.clone(),
        content,
        metadata,
    }
}

pub(crate) struct SharedPdfEnv {
    pub(crate) engine: Arc<WikiEngine>,
    pub(crate) work_dir: PathBuf,
    pub(crate) input_paths: Vec<PathBuf>,
    pub(crate) max_chars: Option<usize>,
    pub(crate) docs: Mutex<Vec<ConvertedPdfDoc>>,
}

impl SharedPdfEnv {
    pub(crate) fn ensure_converted(&self) -> std::result::Result<Vec<ConvertedPdfDoc>, ToolError> {
        {
            let guard = self.docs.lock().unwrap();
            if !guard.is_empty() {
                return Ok(guard.clone());
            }
        }

        let mut converted = Vec::new();
        for (i, path) in self.input_paths.iter().enumerate() {
            let doc = convert_document_with_anydoc(i + 1, path, self.max_chars)
                .map_err(|e| tool_err("ANYDOC_CONVERT_ERROR", e.to_string()))?;

            // Lưu bản Markdown thô xuất từ `anydoc` ra thư mục tạm `work_dir` để tiện đối chiếu
            let snapshot_path = self.work_dir.join(format!("{}.anydoc.md", doc.file_stem));
            let _ = std::fs::write(&snapshot_path, &doc.markdown);
            converted.push(doc);
        }

        let mut guard = self.docs.lock().unwrap();
        if guard.is_empty() {
            *guard = converted.clone();
        }
        Ok(guard.clone())
    }
}

// ---------------------------------------------------------------------------
// 1. Tool `anydoc_inspect_pdf`: Chuyển đổi PDF -> Markdown bằng `anydoc` & xem Outline
// ---------------------------------------------------------------------------

pub(crate) struct AnydocInspectPdfTool {
    pub(crate) env: Arc<SharedPdfEnv>,
}

#[async_trait::async_trait]
impl HostTool for AnydocInspectPdfTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "anydoc_inspect_pdf".into(),
            description: "Convert the input PDF/document file(s) to GitHub-Flavored Markdown using `anydoc` and return each document's `doc_index`, `file_name`, `file_stem`, `inferred_title`, `outline_headings`, character/line counts, and a Markdown preview so you can classify its `<domain>/<topic>` folder and hierarchical tags.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "doc_index": {
                        "type": "integer",
                        "description": "Optional 1-based document index to inspect (omit to inspect all input PDF files)."
                    }
                }
            }),
            effect: ToolEffect::ReadOnly,
        }
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        args: Value,
    ) -> std::result::Result<ToolOutcome, ToolError> {
        let filter_idx = args
            .get("doc_index")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        ctx.progress("Đang trích xuất Markdown từ PDF bằng anydoc...");
        let env = self.env.clone();
        let docs = tokio::task::spawn_blocking(move || env.ensure_converted())
            .await
            .map_err(|e| tool_err("INTERNAL", e.to_string()))??;

        let mut out = String::new();
        for doc in &docs {
            if let Some(target_idx) = filter_idx {
                if doc.index != target_idx {
                    continue;
                }
            }
            let headings_str = if doc.outline_headings.is_empty() {
                "_(Không có heading cấp 1–3)_".to_string()
            } else {
                doc.outline_headings.join("\n  ")
            };
            let preview = clip_transcript(&doc.markdown, COLLECTOR_PREVIEW_CHARS);
            out.push_str(&format!(
                "=== DOCUMENT #{idx} ===\n\
                 - source_path     : {spath}\n\
                 - file_name       : {fname}\n\
                 - file_stem       : {stem}\n\
                 - detected_format : {fmt}\n\
                 - inferred_title  : {title}\n\
                 - char_count      : {chars}\n\
                 - line_count      : {lines}\n\
                 - outline_headings:\n  {headings}\n\
                 --- MARKDOWN PREVIEW ---\n\
                 {preview}\n\
                 --- END PREVIEW ---\n\n",
                idx = doc.index,
                spath = doc.source_path.display(),
                fname = doc.file_name,
                stem = doc.file_stem,
                fmt = doc.format_name,
                title = doc.inferred_title,
                chars = doc.char_count,
                lines = doc.line_count,
                headings = headings_str,
                preview = preview
            ));
        }

        if out.is_empty() {
            return Err(tool_err(
                "NOT_FOUND",
                format!("Không tìm thấy tài liệu với doc_index={filter_idx:?}"),
            ));
        }

        ctx.progress(&format!(
            "Đã chuyển đổi {} tài liệu sang Markdown bằng anydoc",
            docs.len()
        ));
        Ok(outcome(&ctx, out, json!({ "count": docs.len() })))
    }
}

// ---------------------------------------------------------------------------
// 2. Tool `anydoc_save_to_sources`: Lưu bản Markdown từ `anydoc` vào `sources/<domain>/<topic>/<file_stem>.md`
// ---------------------------------------------------------------------------

pub(crate) struct AnydocSaveToSourcesTool {
    pub(crate) env: Arc<SharedPdfEnv>,
}

#[async_trait::async_trait]
impl HostTool for AnydocSaveToSourcesTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "anydoc_save_to_sources".into(),
            description: "Save the `anydoc`-converted Markdown of a PDF document into `sources/<topic_folder>/<file_stem>.md` organized by `<domain>/<topic>` with 3-facet hierarchical tags and QMD metadata.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["topic_folder"],
                "properties": {
                    "doc_index": {
                        "type": "integer",
                        "description": "1-based `doc_index` from `anydoc_inspect_pdf` (defaults to 1 if only 1 PDF is loaded)."
                    },
                    "file_stem": {
                        "type": "string",
                        "description": "Optional explicit file_stem (defaults to the PDF's `file_stem` from `anydoc_inspect_pdf`)."
                    },
                    "topic_folder": {
                        "type": "string",
                        "description": "Topic folder path inside `sources/` in `kebab-case` (`<domain>/<topic>`, e.g. 'quantitative-finance/deep-learning' or 'ai-engineering/hybrid-search')."
                    },
                    "title": {
                        "type": "string",
                        "description": "Human-readable document title."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hierarchical topic and domain tags in `kebab-case` (e.g. ['topic/ai-engineering/hybrid-search', 'pdf', 'bm25', 'rrf'])."
                    }
                }
            }),
            effect: ToolEffect::Mutating,
        }
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        args: Value,
    ) -> std::result::Result<ToolOutcome, ToolError> {
        let env = self.env.clone();
        let docs = tokio::task::spawn_blocking(move || env.ensure_converted())
            .await
            .map_err(|e| tool_err("INTERNAL", e.to_string()))??;

        let requested_idx = args
            .get("doc_index")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let requested_stem = args
            .get("file_stem")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let doc = docs
            .iter()
            .find(|d| {
                if let Some(idx) = requested_idx {
                    d.index == idx
                } else if let Some(st) = requested_stem {
                    d.file_stem == slugify(st, 48) || d.file_name == st
                } else {
                    d.index == 1
                }
            })
            .cloned()
            .ok_or_else(|| {
                tool_err(
                    "INVALID_ARGUMENT",
                    format!(
                        "Không tìm thấy PDF khớp với doc_index={requested_idx:?} / file_stem={requested_stem:?}"
                    ),
                )
            })?;

        let raw_topic = args
            .get("topic_folder")
            .and_then(Value::as_str)
            .unwrap_or("general/pdf-docs");
        let topic_folder = sanitize_topic_folder(raw_topic);

        let title = args
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&doc.inferred_title)
            .to_string();

        let stem = requested_stem
            .map(|s| slugify(s, 48))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| doc.file_stem.clone());

        let mut tags = vec!["pdf".to_string(), "anydoc".to_string()];
        if let Some(arr) = args.get("tags").and_then(Value::as_array) {
            for item in arr.iter().filter_map(Value::as_str) {
                let clean = item.trim().to_string();
                if !clean.is_empty() && !tags.contains(&clean) {
                    tags.push(clean);
                }
            }
        }

        let mut extra_frontmatter = vec![
            ("source_format".to_string(), json!(doc.format_name)),
            ("converter".to_string(), json!("anydoc")),
            ("original_file".to_string(), json!(doc.file_name)),
        ];
        let mut header_bullets = vec![
            ("File gốc".to_string(), format!("`{}`", doc.file_name)),
            (
                "Bộ chuyển đổi".to_string(),
                format!("`anydoc` (`{}` -> GFM Markdown)", doc.format_name),
            ),
        ];
        if let Some(total_pages) = doc.total_pdf_pages {
            extra_frontmatter.push(("total_pdf_pages".to_string(), json!(total_pages)));
            header_bullets.push((
                "Số trang PDF".to_string(),
                format!(
                    "{}/{} trang có text (bỏ qua trang thuần ảnh: {:?})",
                    total_pages.saturating_sub(doc.skipped_ocr_pages.len() as u32),
                    total_pages,
                    doc.skipped_ocr_pages
                ),
            ));
        }

        let written = self
            .env
            .engine
            .write_topic_source(WriteTopicSourceRequest {
                topic_folder: topic_folder.clone(),
                file_stem: Some(stem),
                title,
                raw_body: doc.markdown.clone(),
                body_heading: Some("Extracted Document Markdown (via anydoc)".to_string()),
                tags,
                extra_frontmatter,
                header_bullets,
                preview_chars: Some(COLLECTOR_PREVIEW_CHARS),
            })
            .map_err(|e| tool_err("WIKI_SOURCE_WRITE_FAILED", e.to_string()))?;

        if let Some(entry) = self
            .env
            .docs
            .lock()
            .unwrap()
            .iter_mut()
            .find(|d| d.index == doc.index)
        {
            entry.saved_source_scoped_path = Some(written.info.scoped_path.clone());
        }

        ctx.progress(&format!(
            "Đã lưu PDF Markdown vào `{}` ({} ký tự, {} dòng, docid: {})",
            written.info.rel_path,
            written.body_char_count,
            written.info.line_count,
            written.info.docid
        ));

        Ok(outcome(
            &ctx,
            written.agent_output,
            json!({
                "source_rel_path": written.info.scoped_path,
                "vault_rel_path": written.info.rel_path,
                "topic_folder": written.topic_folder,
                "docid": written.info.docid,
                "chars": written.body_char_count,
                "lines": written.info.line_count
            }),
        ))
    }
}

// ---------------------------------------------------------------------------
// 3. Bridge Adapter: Biến 13 LLM Tools nguyên bản của `WikiEngine` thành `HostTool`
// ---------------------------------------------------------------------------

pub(crate) struct WikiEngineBridgeTool {
    pub(crate) engine: Arc<WikiEngine>,
    pub(crate) spec: LlmToolSpec,
    pub(crate) effect: ToolEffect,
    pub(crate) read_budget: AdaptiveReadBudget,
}

#[async_trait::async_trait]
impl HostTool for WikiEngineBridgeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.spec.name.clone(),
            description: self.spec.description.clone(),
            input_schema: self.spec.parameters.clone(),
            effect: self.effect,
        }
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        args: Value,
    ) -> std::result::Result<ToolOutcome, ToolError> {
        let out = self
            .engine
            .execute_llm_tool_with_budget(&self.spec.name, args, self.read_budget)
            .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?;

        if let Some(ref msg) = out.progress_message {
            ctx.progress(msg);
        }

        Ok(outcome(&ctx, out.content, out.metadata))
    }
}
