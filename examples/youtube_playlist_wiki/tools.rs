use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use llm_wiki::{CollectionScope, LlmToolSpec, LogAction, WikiEngine};
use orchestrator_agent_rs::{
    HostTool, ToolContext, ToolDescriptor, ToolEffect, ToolError, ToolOutcome,
};
use serde_json::{json, Value};

use crate::ytdlp::{clip_transcript, fetch_playlist, fetch_transcript, slugify};

/// Giới hạn ký tự transcript trả về cho context của LLM (bản gốc đầy đủ luôn lưu trong `sources/`).
const MAX_TRANSCRIPT_CHARS: usize = 9_500;

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
}

impl PlaylistVideoEntry {
    pub(crate) fn source_scoped_path(&self) -> String {
        format!("youtube/{}.md", self.stem)
    }

    pub(crate) fn wiki_rel_path(&self) -> String {
        format!("wiki/sources/youtube/{}.md", self.stem)
    }
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
            description: "List all videos in a YouTube playlist URL using yt-dlp. Returns one line per video with `index`, `video_id`, `source_rel_path` (inside `sources/`), `wiki_rel_path` (inside `wiki/`), and `title`.".into(),
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
                }
            })
            .collect();

        let mut text = String::new();
        for v in &entries {
            text.push_str(&format!(
                "{}. video_id={} source_rel_path={} wiki_rel_path={} title={}\n",
                v.index,
                v.id,
                v.source_scoped_path(),
                v.wiki_rel_path(),
                v.title
            ));
        }

        ctx.progress(&format!("Tìm thấy {} video trong playlist", entries.len()));
        *self.env.videos.lock().unwrap() = entries;
        Ok(outcome(&ctx, text, json!({})))
    }
}

// ---------------------------------------------------------------------------
// 2. Tool `yt_fetch_transcript`: Agent tự gọi để tải transcript & tạo Raw Source
// ---------------------------------------------------------------------------

pub(crate) struct YtFetchTranscriptTool {
    pub(crate) env: Arc<SharedEnv>,
}

#[async_trait::async_trait]
impl HostTool for YtFetchTranscriptTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "yt_fetch_transcript".into(),
            description: "Download the subtitles/transcript of a YouTube video using yt-dlp, save the immutable Raw Source Markdown file into `sources/<source_rel_path>`, and return the full transcript text for the agent to analyze.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["video_id"],
                "properties": {
                    "video_id": {
                        "type": "string",
                        "description": "YouTube video ID from `yt_list_playlist`"
                    },
                    "source_rel_path": {
                        "type": "string",
                        "description": "Relative path inside `sources/` (e.g. 'youtube/01-....md' from `yt_list_playlist`)"
                    },
                    "title": {
                        "type": "string",
                        "description": "Video title from `yt_list_playlist`"
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
        let known_entry = self
            .env
            .videos
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.id == video_id)
            .cloned();

        let (source_scoped, title, idx) = if let Some(entry) = known_entry {
            (entry.source_scoped_path(), entry.title, entry.index)
        } else {
            let custom_rel = args
                .get("source_rel_path")
                .and_then(Value::as_str)
                .map(|s| s.trim().trim_start_matches("sources/").to_string())
                .unwrap_or_else(|| format!("youtube/{video_id}.md"));
            let custom_title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&video_id)
                .to_string();
            (custom_rel, custom_title, 0)
        };

        ctx.progress(&format!(
            "Đang tải transcript bằng yt-dlp cho video `{video_id}` ({title})..."
        ));
        let work = self.env.work_dir.clone();
        let langs = self.env.langs.clone();
        let vid = video_id.clone();
        let transcript =
            tokio::task::spawn_blocking(move || fetch_transcript(&work, &vid, &langs))
                .await
                .map_err(|e| tool_err("INTERNAL", e.to_string()))??;

        let char_count = transcript.chars().count();
        let youtube_url = format!("https://www.youtube.com/watch?v={video_id}");
        let escaped_title = title.replace('"', "\\\"");

        // Ghi file Raw Source Markdown bất biến vào `sources/youtube/<slug>.md`
        let raw_source_md = format!(
            "---\ntitle: \"{escaped_title}\"\ncategory: \"raw_source\"\nvideo_id: \"{video_id}\"\nyoutube_url: \"{youtube_url}\"\nplaylist_url: \"{}\"\nplaylist_index: {idx}\ntags:\n  - \"raw\"\n  - \"youtube\"\n  - \"transcript\"\nqmd:\n  metadata:\n    category: \"raw_source\"\n    video_id: \"{video_id}\"\n    playlist_index: {idx}\n    tags:\n      - \"raw\"\n      - \"youtube\"\n---\n\n# {title}\n\n- **YouTube URL:** <{youtube_url}>\n- **Playlist URL:** <{}>\n- **Video ID:** `{video_id}`\n- **Thứ tự trong Playlist:** #{idx}\n- **Độ dài Transcript:** {char_count} ký tự\n\n## Raw Transcript\n\n{transcript}\n",
            self.env.playlist_url, self.env.playlist_url
        );

        let source_info = self
            .env
            .engine
            .write_source_file(&source_scoped, &raw_source_md)
            .map_err(|e| tool_err("WIKI_SOURCE_WRITE_FAILED", e.to_string()))?;

        ctx.progress(&format!(
            "Đã lưu Raw Source `{}` ({char_count} ký tự, docid: {})",
            source_info.rel_path, source_info.docid
        ));

        let clipped = clip_transcript(&transcript, MAX_TRANSCRIPT_CHARS);
        let content = format!(
            "RAW_SOURCE_SAVED: {}\nSOURCE_SCOPED_PATH: {}\nEXPECTED_WIKI_PATH: wiki/sources/{}\nDOCID: {}\nVIDEO_ID: {}\nTITLE: {}\nTRANSCRIPT_LENGTH_CHARS: {}\n\n=== BEGIN FULL TRANSCRIPT ===\n{}\n=== END FULL TRANSCRIPT ===",
            source_info.rel_path,
            source_info.scoped_path,
            source_info.scoped_path,
            source_info.docid,
            video_id,
            title,
            char_count,
            clipped
        );

        Ok(outcome(
            &ctx,
            content,
            json!({
                "source_rel_path": source_info.scoped_path,
                "vault_rel_path": source_info.rel_path,
                "docid": source_info.docid,
                "chars": char_count
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

        let res = self
            .engine
            .execute_llm_tool(name, args)
            .map_err(|e| tool_err("WIKI_TOOL_ERROR", e.to_string()))?;

        if name == "wiki_get" {
            let rel_path = res
                .get("info")
                .and_then(|i| i.get("rel_path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let content = res.get("content").and_then(Value::as_str).unwrap_or("");
            return Ok(outcome(&ctx, format!("FILE: {rel_path}\n\n{content}"), res));
        } else if name == "wiki_tree" {
            let ascii = res.get("ascii_tree").and_then(Value::as_str).unwrap_or("");
            return Ok(outcome(&ctx, ascii.to_string(), res));
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
                "Agent executed wiki_lint health-check",
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

/// Tool bổ sung `wiki_delete_file` để Agent có thể xóa trang bị trùng lặp hoặc dư thừa khi dọn dẹp Wiki.
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

