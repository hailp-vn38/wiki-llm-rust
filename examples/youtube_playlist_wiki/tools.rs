use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub(crate) use llm_wiki::AdaptiveReadBudget;
use llm_wiki::{
    sanitize_topic_folder, slugify, LlmToolSpec, WikiEngine, WriteTopicSourceRequest,
};
use orchestrator_agent_rs::{
    HostTool, ToolContext, ToolDescriptor, ToolEffect, ToolError, ToolOutcome,
};
use serde_json::{json, Value};

use crate::ytdlp::{fetch_playlist, fetch_transcript};

/// Giới hạn ký tự cho bản xem trước ngắn khi `source_collector` lưu transcript vào `sources/`
/// (tránh làm đầy context window của `source_collector` khi tải nhiều video liên tiếp).
const COLLECTOR_PREVIEW_CHARS: usize = 600;

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

        let mut tags = Vec::new();
        if let Some(arr) = args.get("tags").and_then(Value::as_array) {
            for item in arr.iter().filter_map(Value::as_str) {
                let clean_tag = item.trim().to_string();
                if !clean_tag.is_empty() && !tags.contains(&clean_tag) {
                    tags.push(clean_tag);
                }
            }
        }

        ctx.progress(&format!(
            "Đang tải transcript bằng yt-dlp cho video `{video_id}` -> `sources/{topic_folder}/{stem}.md`..."
        ));
        let work = self.env.work_dir.clone();
        let langs = self.env.langs.clone();
        let vid = video_id.clone();
        let transcript =
            tokio::task::spawn_blocking(move || fetch_transcript(&work, &vid, &langs))
                .await
                .map_err(|e| tool_err("INTERNAL", e.to_string()))??;

        let youtube_url = format!("https://www.youtube.com/watch?v={video_id}");
        let written = self
            .env
            .engine
            .write_topic_source(WriteTopicSourceRequest {
                topic_folder: topic_folder.clone(),
                file_stem: Some(stem),
                title: title.clone(),
                raw_body: transcript,
                body_heading: Some("Raw Transcript".to_string()),
                tags,
                extra_frontmatter: vec![
                    ("video_id".to_string(), json!(video_id)),
                    ("youtube_url".to_string(), json!(youtube_url)),
                    ("playlist_url".to_string(), json!(self.env.playlist_url)),
                    ("playlist_index".to_string(), json!(idx)),
                ],
                header_bullets: vec![
                    ("YouTube URL".to_string(), format!("<{youtube_url}>")),
                    (
                        "Playlist URL".to_string(),
                        format!("<{}>", self.env.playlist_url),
                    ),
                    ("Video ID".to_string(), format!("`{video_id}`")),
                    ("Thứ tự trong Playlist".to_string(), format!("#{idx}")),
                ],
                preview_chars: Some(COLLECTOR_PREVIEW_CHARS),
            })
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
            entry.saved_source_scoped_path = Some(written.info.scoped_path.clone());
        }

        ctx.progress(&format!(
            "Đã lưu Raw Source theo chủ đề `{}` ({} ký tự, {} dòng, docid: {})",
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
