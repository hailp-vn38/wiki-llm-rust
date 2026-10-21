use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use llm_wiki::{
    slice_lines_with_budget, CollectionScope, GetOptions, LlmToolSpec, LogAction, WikiEngine,
};
use orchestrator_agent_rs::{
    HostTool, ToolContext, ToolDescriptor, ToolEffect, ToolError, ToolOutcome,
};
use serde_json::{json, Value};

use crate::ytdlp::{clip_transcript, fetch_playlist, fetch_transcript, slugify};

/// Giới hạn ký tự cho bản xem trước ngắn khi `source_collector` lưu transcript vào `sources/`
/// (tránh làm đầy context window của `source_collector` khi tải nhiều video liên tiếp).
const COLLECTOR_PREVIEW_CHARS: usize = 600;

/// Cấu hình cửa sổ đọc thích ứng (Adaptive Read Budget) cho `wiki_get` theo `context_limit_tokens` của Model:
/// - **Model nhỏ (`<= 16K` tokens)**: Chia nhỏ `110 dòng / 5.200 ký tự` để không tràn context.
/// - **Model trung bình (`16K..=64K` tokens)**: `220 dòng / 16.000 ký tự`.
/// - **Model lớn (`> 64K` tokens, ví dụ Nemotron 3 Ultra 200K)**: `400 dòng / 36.000 ký tự` (~9K tokens),
///   đủ đọc trọn vẹn 95% video/tài liệu chỉ trong **1 lượt `wiki_get` duy nhất** (tiết kiệm 3–4 vòng gọi LLM lặp lại),
///   nhưng vẫn giữ trần an toàn 36K ký tự để tránh hiện tượng "Lost in the Middle" với sách/podcast quá dài.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AdaptiveReadBudget {
    pub(crate) default_lines: usize,
    pub(crate) max_window_chars: usize,
    pub(crate) compact_index_max_chars: usize,
}

impl AdaptiveReadBudget {
    pub(crate) fn for_context_limit(context_limit_tokens: usize) -> Self {
        if context_limit_tokens <= 16_000 {
            Self {
                default_lines: 110,
                max_window_chars: 5_200,
                compact_index_max_chars: 3_200,
            }
        } else if context_limit_tokens <= 64_000 {
            Self {
                default_lines: 220,
                max_window_chars: 16_000,
                compact_index_max_chars: 6_000,
            }
        } else {
            Self {
                default_lines: 400,
                max_window_chars: 36_000,
                compact_index_max_chars: 12_000,
            }
        }
    }
}

/// Tự động sửa lỗi thẻ đóng `</mermaid>` thành ` ``` ` khi một số model (như Nemotron) sinh nhầm cú pháp XML cho Mermaid.
fn sanitize_mermaid_fences(md: &str) -> String {
    if !md.contains("</mermaid>") {
        return md.to_string();
    }
    md.lines()
        .map(|line| {
            if line.trim().eq_ignore_ascii_case("</mermaid>") {
                "```"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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

fn str_arg<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| tool_err("INVALID_ARGUMENT", format!("Thiếu hoặc rỗng: {key}")))
}

/// Chuẩn hóa đường dẫn thư mục chủ đề do Agent truyền vào (ví dụ: `"algo-trading/mql5-dca"`).
fn sanitize_topic_folder(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_matches('/')
        .strip_prefix("sources/")
        .unwrap_or(raw.trim().trim_matches('/'));
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|s| !s.trim().is_empty() && *s != "." && *s != "..")
        .take(2)
        .map(|s| slugify(s, 36))
        .collect();
    if segments.is_empty() {
        "general".to_string()
    } else {
        segments.join("/")
    }
}

// ---------------------------------------------------------------------------
// Trạng thái dùng chung cho YouTube Tools & Native WikiEngine Tools
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct PlaylistVideoEntry {
    pub(crate) index: usize,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) stem: String,
    /// Đường dẫn tương đối thực tế bên trong `sources/` sau khi `source_collector` phân loại theo chủ đề
    /// (ví dụ: `Some("algo-trading/mql5-dca/01-....md")`).
    pub(crate) saved_source_scoped_path: Option<String>,
}

pub(crate) struct SharedEnv {
    pub(crate) engine: Arc<WikiEngine>,
    pub(crate) work_dir: PathBuf,
    pub(crate) playlist_url: String,
    pub(crate) langs: String,
    pub(crate) limit: usize,
    pub(crate) videos: Mutex<Vec<PlaylistVideoEntry>>,
}

// ---------------------------------------------------------------------------
// 1. Tool `yt_list_playlist`: Lấy danh sách video từ YouTube Playlist
// ---------------------------------------------------------------------------

pub(crate) struct YtListPlaylistTool {
    pub(crate) env: Arc<SharedEnv>,
}

#[async_trait::async_trait]
impl HostTool for YtListPlaylistTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "yt_list_playlist".into(),
            description: "List all videos in a YouTube playlist URL using yt-dlp. Returns `index`, `video_id`, `file_stem`, and `title` so the agent can analyze the playlist theme and choose a topic folder (`<domain>/<topic>`) inside `sources/`.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "YouTube playlist http(s) URL"
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
        let url = str_arg(&args, "url")?.to_string();
        ctx.progress("Đang lấy danh sách video từ playlist bằng yt-dlp...");
        let mut raw_list = tokio::task::spawn_blocking(move || fetch_playlist(&url))
            .await
            .map_err(|e| tool_err("INTERNAL", e.to_string()))??;
        raw_list.truncate(self.env.limit);

        let entries: Vec<PlaylistVideoEntry> = raw_list
            .into_iter()
            .enumerate()
            .map(|(i, (id, title))| {
                let stem = format!("{:02}-{}", i + 1, slugify(&title, 36));
                PlaylistVideoEntry {
                    index: i + 1,
                    id,
                    title,
                    stem,
                    saved_source_scoped_path: None,
                }
            })
            .collect();

        let mut text = String::new();
        for v in &entries {
            text.push_str(&format!(
                "{}. video_id={} file_stem={} title={}\n",
                v.index, v.id, v.stem, v.title
            ));
        }

        ctx.progress(&format!("Tìm thấy {} video trong playlist", entries.len()));
        *self.env.videos.lock().unwrap() = entries;
        Ok(outcome(&ctx, text, json!({})))
    }
}

// ---------------------------------------------------------------------------
// 2. Tool `yt_fetch_transcript`: Agent tự gọi để tải transcript & lưu vào `sources/<topic_folder>/`
// ---------------------------------------------------------------------------

pub(crate) struct YtFetchTranscriptTool {
    pub(crate) env: Arc<SharedEnv>,
}

#[async_trait::async_trait]
impl HostTool for YtFetchTranscriptTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "yt_fetch_transcript".into(),
            description: "Download the subtitles/transcript of a YouTube video using yt-dlp and save the Raw Source Markdown file into `sources/<topic_folder>/<file_stem>.md` organized by topic with hierarchical tags.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["video_id", "topic_folder"],
                "properties": {
                    "video_id": {
                        "type": "string",
                        "description": "YouTube video ID from `yt_list_playlist`"
                    },
                    "topic_folder": {
                        "type": "string",
                        "description": "Topic folder path inside `sources/` in `kebab-case` (max 2 levels `<domain>/<topic>`, e.g. 'algo-trading/mql5-dca' or 'ai-engineering/rag')."
                    },
                    "title": {
                        "type": "string",
                        "description": "Video title from `yt_list_playlist`"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hierarchical topic tags in `kebab-case` (e.g. ['topic/algo-trading/mql5-dca', 'mql5', 'mt5'])"
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
        let video_id = str_arg(&args, "video_id")?.to_string();
        let raw_topic_folder = args
            .get("topic_folder")
            .and_then(Value::as_str)
            .unwrap_or("general");
        let topic_folder = sanitize_topic_folder(raw_topic_folder);

        let mut folder_parts = topic_folder.split('/');
        let domain = folder_parts.next().unwrap_or("general").to_string();
        let topic = folder_parts.next().unwrap_or(&domain).to_string();

        let known_entry = self
            .env
            .videos
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.id == video_id)
            .cloned();

        let (stem, title, idx) = if let Some(entry) = known_entry {
            (entry.stem, entry.title, entry.index)
        } else {
            let custom_title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&video_id)
                .to_string();
            let custom_stem = slugify(&custom_title, 36);
            (custom_stem, custom_title, 0)
        };

        let source_scoped = format!("{topic_folder}/{stem}.md");

        let mut tags = vec![
            "wiki/source".to_string(),
            format!("topic/{topic_folder}"),
        ];
        if let Some(arr) = args.get("tags").and_then(Value::as_array) {
            for item in arr.iter().filter_map(Value::as_str) {
                let clean_tag = item.trim().to_string();
                if !clean_tag.is_empty() && !tags.contains(&clean_tag) {
                    tags.push(clean_tag);
                }
            }
        }
        let yaml_tags = tags
            .iter()
            .map(|t| format!("  - \"{t}\""))
            .collect::<Vec<_>>()
            .join("\n");
        let qmd_yaml_tags = tags
            .iter()
            .map(|t| format!("      - \"{t}\""))
            .collect::<Vec<_>>()
            .join("\n");

        ctx.progress(&format!(
            "Đang tải transcript bằng yt-dlp cho video `{video_id}` -> `sources/{source_scoped}`..."
        ));
        let work = self.env.work_dir.clone();
        let langs = self.env.langs.clone();
        let vid = video_id.clone();
        let transcript =
            tokio::task::spawn_blocking(move || fetch_transcript(&work, &vid, &langs))
                .await
                .map_err(|e| tool_err("INTERNAL", e.to_string()))??;

        let char_count = transcript.chars().count();
        let line_count = transcript.lines().count().max(1);
        let youtube_url = format!("https://www.youtube.com/watch?v={video_id}");
        let escaped_title = title.replace('"', "\\\"");

        // Ghi file Raw Source Markdown bất biến vào `sources/<topic_folder>/<stem>.md`
        let raw_source_md = format!(
            "---\ntitle: \"{escaped_title}\"\ncategory: \"raw_source\"\ndomain: \"{domain}\"\ntopic: \"{topic}\"\nvideo_id: \"{video_id}\"\nyoutube_url: \"{youtube_url}\"\nplaylist_url: \"{}\"\nplaylist_index: {idx}\ntags:\n{yaml_tags}\nqmd:\n  metadata:\n    category: \"raw_source\"\n    domain: \"{domain}\"\n    topic: \"{topic}\"\n    video_id: \"{video_id}\"\n    playlist_index: {idx}\n    tags:\n{qmd_yaml_tags}\n---\n\n# {title}\n\n- **Chủ đề (Topic Folder):** `{topic_folder}`\n- **YouTube URL:** <{youtube_url}>\n- **Playlist URL:** <{}>\n- **Video ID:** `{video_id}`\n- **Thứ tự trong Playlist:** #{idx}\n- **Độ dài Transcript:** {char_count} ký tự ({line_count} dòng)\n\n## Raw Transcript\n\n{transcript}\n",
            self.env.playlist_url, self.env.playlist_url
        );

        let source_info = self
            .env
            .engine
            .write_source_file(&source_scoped, &raw_source_md)
            .map_err(|e| tool_err("WIKI_SOURCE_WRITE_FAILED", e.to_string()))?;

        // Lưu lại đường dẫn thực tế của video trong `SharedEnv`
        if let Some(entry) = self
            .env
            .videos
            .lock()
            .unwrap()
            .iter_mut()
            .find(|v| v.id == video_id)
        {
            entry.saved_source_scoped_path = Some(source_info.scoped_path.clone());
        }

        ctx.progress(&format!(
            "Đã lưu Raw Source theo chủ đề `{}` ({char_count} ký tự, {} dòng, docid: {})",
            source_info.rel_path, source_info.line_count, source_info.docid
        ));

        let short_preview = clip_transcript(&transcript, COLLECTOR_PREVIEW_CHARS);
        let content = format!(
            "RAW_SOURCE_SAVED: {}\nSOURCE_SCOPED_PATH: {}\nEXPECTED_WIKI_PATH: wiki/sources/{}\nTOPIC_FOLDER: {}\nDOCID: {}\nVIDEO_ID: {}\nTITLE: {}\nTRANSCRIPT_LENGTH_CHARS: {}\nTRANSCRIPT_LINES: {}\n\n=== BEGIN SHORT PREVIEW ===\n{}\n=== END SHORT PREVIEW ===",
            source_info.rel_path,
            source_info.scoped_path,
            source_info.scoped_path,
            topic_folder,
            source_info.docid,
            video_id,
            title,
            char_count,
            source_info.line_count,
            short_preview
        );

        Ok(outcome(
            &ctx,
            content,
            json!({
                "source_rel_path": source_info.scoped_path,
                "vault_rel_path": source_info.rel_path,
                "topic_folder": topic_folder,
                "docid": source_info.docid,
                "chars": char_count,
                "lines": source_info.line_count
            }),
        ))
    }
}

// ---------------------------------------------------------------------------
// 3. Bridge Adapter: Biến các LLM Tools nguyên bản của `WikiEngine` thành `HostTool`
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
        let name = self.spec.name.as_str();
        if name == "wiki_lint" {
            let _ = self.engine.embed_pending(false);
            let _ = self.engine.rebuild_or_sync_index_md();
        }

        // Cơ chế đọc phân trang thích ứng (`AdaptiveReadBudget` + `doc_theo_dong` của `javis-os`):
        // - Với model có `context_limit_tokens` lớn (200K), tự động mở rộng cửa sổ lên 400 dòng / 36.000 ký tự
        //   để đọc trọn vẹn 1 tài liệu/video dài chỉ trong 1 lượt `wiki_get` duy nhất (giảm 70% token cộng dồn do không phải gửi lại lịch sử nhiều vòng).
        // - Với file cực dài (> 36.000 ký tự), tự động phân trang sạch ở ranh giới dòng `\n` kèm chỉ dẫn `from_line` tiếp theo.
        if name == "wiki_get" {
            let file = str_arg(&args, "file")?;
            let from_line = args
                .get("from_line")
                .or_else(|| args.get("fromLine"))
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let max_lines = args
                .get("max_lines")
                .or_else(|| args.get("maxLines"))
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let line_numbers = args
                .get("line_numbers")
                .or_else(|| args.get("lineNumbers"))
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let clean_file = file.trim().trim_start_matches('/');
            if (clean_file.eq_ignore_ascii_case("wiki/index.md")
                || clean_file.eq_ignore_ascii_case("index.md"))
                && from_line.is_none()
            {
                let full_idx = self
                    .engine
                    .read_index_md()
                    .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?;
                let body = if full_idx.chars().count() <= self.read_budget.compact_index_max_chars {
                    full_idx
                } else {
                    self.engine
                        .read_compact_index_md(self.read_budget.compact_index_max_chars)
                        .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?
                };
                return Ok(outcome(
                    &ctx,
                    format!("FILE: wiki/index.md\n\n{body}"),
                    json!({ "rel_path": "wiki/index.md" }),
                ));
            }

            // Khi đọc file trong `sources/...`, phần `# Title` và bullet metadata đã chứa đầy đủ thông tin,
            // nên lược bỏ 30 dòng YAML frontmatter + `qmd.metadata` trùng lặp ở đầu file để tiết kiệm token.
            let include_frontmatter = !clean_file.starts_with("sources/");
            let doc = self
                .engine
                .get_document(
                    file,
                    GetOptions {
                        from_line: None,
                        max_lines: None,
                        line_numbers: false,
                        include_frontmatter,
                    },
                )
                .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?;

            let effective_max_lines = Some(max_lines.unwrap_or(self.read_budget.default_lines));
            let bounded = slice_lines_with_budget(
                &doc.content,
                from_line,
                effective_max_lines,
                Some(self.read_budget.max_window_chars),
                line_numbers,
            );

            return Ok(outcome(
                &ctx,
                format!(
                    "FILE: {} (docid: {}, total_lines: {})\n\n{}",
                    doc.info.rel_path, doc.info.docid, doc.info.line_count, bounded
                ),
                json!({
                    "rel_path": doc.info.rel_path,
                    "docid": doc.info.docid,
                    "line_count": doc.info.line_count,
                }),
            ));
        }

        let mut args = args;
        if let Some(obj) = args.as_object_mut() {
            for field in [
                "wiki_markdown_body",
                "content",
                "append_text",
                "replacement_text",
                "markdown_body",
            ] {
                if let Some(Value::String(s)) = obj.get_mut(field) {
                    if s.contains("</mermaid>") {
                        *s = sanitize_mermaid_fences(s);
                    }
                }
            }
        }

        let res = self
            .engine
            .execute_llm_tool(name, args)
            .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?;

        if name == "wiki_tree" {
            let ascii = res.get("ascii_tree").and_then(Value::as_str).unwrap_or("");
            return Ok(outcome(
                &ctx,
                ascii.to_string(),
                json!({ "total_files": res.get("total_files") }),
            ));
        } else if name == "wiki_lint" {
            let is_healthy = res
                .get("is_healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let broken_arr = res
                .get("broken_links")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let orphans_arr = res
                .get("orphan_wiki_pages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let uningested_arr = res
                .get("uningested_sources")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let broken = broken_arr.len();
            let orphans = orphans_arr.len();
            let _ = self.engine.append_log(
                LogAction::Lint,
                "Subagent executed wiki_lint health-check",
                &[format!(
                    "is_healthy={is_healthy}, broken_links={broken}, orphan_wiki_pages={orphans}"
                )],
            );
            ctx.progress(&format!(
                "Wiki Lint: is_healthy={is_healthy}, broken_links={broken}, orphans={orphans}"
            ));
            let compact = json!({
                "is_healthy": is_healthy,
                "uningested_sources": uningested_arr,
                "broken_links": broken_arr,
                "orphan_wiki_pages": orphans_arr,
            });
            let pretty = serde_json::to_string_pretty(&compact).unwrap_or_default();
            return Ok(outcome(&ctx, pretty, compact));
        } else if name == "wiki_ingest_source" {
            if let Some(p) = res.get("wiki_rel_path").and_then(Value::as_str) {
                ctx.progress(&format!("Đã ingest trang Wiki: `{p}`"));
                let docid = res.get("wiki_docid").and_then(Value::as_str).unwrap_or("");
                return Ok(outcome(
                    &ctx,
                    format!("Ingested `{p}` (docid: {docid})"),
                    res,
                ));
            }
        } else if name == "wiki_write_file" || name == "wiki_patch_file" {
            if let Some(p) = res.get("rel_path").and_then(Value::as_str) {
                ctx.progress(&format!("Đã cập nhật file Wiki: `{p}`"));
                let docid = res.get("docid").and_then(Value::as_str).unwrap_or("");
                return Ok(outcome(&ctx, format!("Updated `{p}` (docid: {docid})"), res));
            }
        } else if name == "wiki_file_synthesis" {
            if let Some(p) = res.get("rel_path").and_then(Value::as_str) {
                ctx.progress(&format!("Đã lưu trang Synthesis: `{p}`"));
                let docid = res.get("docid").and_then(Value::as_str).unwrap_or("");
                return Ok(outcome(
                    &ctx,
                    format!("Filed synthesis `{p}` (docid: {docid})"),
                    res,
                ));
            }
        }

        let pretty = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
        Ok(outcome(&ctx, pretty, res))
    }
}

/// Tool bổ sung `wiki_delete_file` để Subagent `wiki_curator` xóa trang bị trùng lặp hoặc dư thừa khi dọn dẹp Wiki.
pub(crate) struct WikiDeleteFileTool {
    pub(crate) engine: Arc<WikiEngine>,
}

#[async_trait::async_trait]
impl HostTool for WikiDeleteFileTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "wiki_delete_file".into(),
            description: "Delete a duplicate or obsolete Markdown page inside `wiki/` and refresh `wiki/index.md` + Obsidian Graph links.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["rel_path"],
                "properties": {
                    "rel_path": {
                        "type": "string",
                        "description": "Relative path inside `wiki/` to delete (e.g. 'entities/duplicate.md' or 'concepts/old.md')"
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
        let rel_path = str_arg(&args, "rel_path")?;
        let deleted = self
            .engine
            .delete_file(CollectionScope::Wiki, rel_path)
            .map_err(|e| tool_err("WIKI_DELETE_ERROR", e.to_string()))?;
        let _ = self.engine.rebuild_or_sync_index_md();
        ctx.progress(&format!("Đã xóa file Wiki `{rel_path}` (deleted={deleted})"));
        Ok(outcome(
            &ctx,
            format!("deleted={deleted} path={rel_path}"),
            json!({ "deleted": deleted, "rel_path": rel_path }),
        ))
    }
}
