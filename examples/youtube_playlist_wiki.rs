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

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llm_wiki::{
    CollectionScope, LlmToolSpec, LogAction, TreeOptions, TreeScope, WikiEngine,
};
use orchestrator_agent_rs::{
    AgentDefinition, AgentMode, CallerContext, FeedItem, HostTool, LlmBackend, LlmConfig,
    LlmProviderConfig, Orchestrator, OrchestratorConfig, ProviderAuth, RunStatus, SecretString,
    ToolContext, ToolDescriptor, ToolEffect, ToolError, ToolOutcome,
};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Giới hạn ký tự transcript trả về cho context của LLM (bản gốc đầy đủ luôn lưu trong `sources/`).
/// Gateway `gh/gpt-4o-mini` có giới hạn ~8,000 input tokens (~30,000 ký tự tổng cộng cho toàn bộ session),
/// nên ta giữ 9,500 ký tự giàu thông tin nhất (đầu + giữa + cuối) cho prompt, còn file `sources/youtube/*.md` lưu trọn vẹn 100%.
const MAX_TRANSCRIPT_CHARS: usize = 9_500;

const YOUTUBE_WIKI_SCHEMA_MD: &str = r#"# LLM Wiki Schema — YouTube Playlist Knowledge Base

> Based on Andrej Karpathy's **LLM Wiki** pattern (`docs/karpathy-llm-wiki.md`).
> Obsidian is the IDE; the LLM is the programmer; the Wiki is the codebase.

## 1. Three-Layer Architecture
1. **Raw Sources (`sources/youtube/*.md`)**:
   - Immutable raw video transcripts fetched via `yt_fetch_transcript` (`yt-dlp`).
   - Contains YAML frontmatter (`title`, `video_id`, `youtube_url`, `playlist_url`, `tags`) and full transcript text.
   - Never modified after creation.
2. **The Wiki (`wiki/`)**:
   - `wiki/sources/youtube/<slug>.md`: Deep, comprehensive technical wiki pages (strictly preserving the exact filename from `sources/youtube/<slug>.md`).
   - `wiki/entities/<slug>.md`: Canonical entity pages (people, platforms, languages, indicators, brokers/datasets) referenced across videos.
   - `wiki/concepts/<slug>.md`: Canonical concept pages (strategies, algorithms, architectural patterns, risk principles) synthesized across videos.
   - `wiki/syntheses/<slug>.md`: Cross-video comparative synthesis, evolution analysis, comparison table, and Mermaid knowledge graph.
   - `wiki/index.md`: Content-oriented master catalog of all wiki pages.
   - `wiki/log.md`: Chronological append-only operation log (`init`, `ingest`, `query`, `lint`, `update`).
3. **The Schema (`SCHEMA.md`)**:
   - Defines conventions for frontmatter, canonical taxonomy (no duplicate/fragmented slugs), Obsidian `[[WikiLinks]]`, contradiction flagging (`> [!WARNING] Contradiction`), and workflows.
"#;

fn tool_err(code: &str, message: impl Into<String>) -> ToolError {
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
// Tiện ích xử lý chuỗi & WebVTT
// ---------------------------------------------------------------------------

fn fold_vietnamese(c: char) -> char {
    const GROUPS: [(&str, char); 7] = [
        ("àáảãạăằắẳẵặâầấẩẫậÀÁẢÃẠĂẰẮẲẴẶÂẦẤẨẪẬ", 'a'),
        ("èéẻẽẹêềếểễệÈÉẺẼẸÊỀẾỂỄỆ", 'e'),
        ("ìíỉĩịÌÍỈĨỊ", 'i'),
        ("òóỏõọôồốổỗộơờớởỡợÒÓỎÕỌÔỒỐỔỖỘƠỜỚỞỠỢ", 'o'),
        ("ùúủũụưừứửữựÙÚỦŨỤƯỪỨỬỮỰ", 'u'),
        ("ỳýỷỹỵỲÝỶỸỴ", 'y'),
        ("đĐ", 'd'),
    ];
    GROUPS
        .iter()
        .find(|(set, _)| set.contains(c))
        .map_or(c, |(_, r)| *r)
}

fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars().map(fold_vietnamese) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.chars().take(max).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

/// Chuyển WebVTT sang văn bản thuần: bỏ header, timestamp, thẻ `<c>` và dòng lặp của auto-sub.
fn vtt_to_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line == "WEBVTT"
            || line.contains("-->")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.starts_with("NOTE")
            || line.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let mut text = String::new();
        let mut in_tag = false;
        for c in line.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        let text = text
            .replace("&amp;", "&")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&#39;", "'")
            .replace("&quot;", "\"")
            .trim()
            .to_string();
        if !text.is_empty() && out.last() != Some(&text) {
            out.push(text);
        }
    }
    out.join(" ")
}

fn clip_transcript(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let part = max / 3;
    let mid = chars.len() / 2 - part / 2;
    let seg = |a: usize| chars[a..a + part].iter().collect::<String>();
    format!(
        "{}\n[...lược bớt để vừa context window; bản đầy đủ nằm trong sources/...]\n{}\n[...lược bớt...]\n{}",
        seg(0),
        seg(mid),
        seg(chars.len() - part)
    )
}

// ---------------------------------------------------------------------------
// Tích hợp yt-dlp (chạy trong spawn_blocking)
// ---------------------------------------------------------------------------

fn http_url(url: &str) -> std::result::Result<(), ToolError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(tool_err("INVALID_URL", "Chỉ hỗ trợ URL http(s)"))
    }
}

fn ytdlp_unavailable(e: io::Error) -> ToolError {
    tool_err(
        "YTDLP_UNAVAILABLE",
        format!("Không chạy được yt-dlp (cài bằng `brew install yt-dlp`): {e}"),
    )
}

fn stderr_tail(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let tail: Vec<char> = s.chars().rev().take(600).collect();
    tail.into_iter().rev().collect()
}

fn fetch_playlist(url: &str) -> std::result::Result<Vec<(String, String)>, ToolError> {
    http_url(url)?;
    let out = std::process::Command::new("yt-dlp")
        .args(["--flat-playlist", "--print", "%(id)s\t%(title)s", "--"])
        .arg(url)
        .output()
        .map_err(ytdlp_unavailable)?;
    if !out.status.success() {
        return Err(tool_err("YTDLP_FAILED", stderr_tail(&out.stderr)));
    }
    let list: Vec<(String, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (id, title) = l.trim().split_once('\t')?;
            (!id.trim().is_empty()).then(|| (id.trim().to_string(), title.trim().to_string()))
        })
        .collect();
    if list.is_empty() {
        return Err(tool_err("EMPTY_PLAYLIST", "Playlist không có video nào"));
    }
    Ok(list)
}

fn fetch_transcript(work: &Path, id: &str, langs: &str) -> std::result::Result<String, ToolError> {
    let sub_langs = langs
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .flat_map(|l| [format!("{l}-orig"), l.to_string()])
        .collect::<Vec<_>>()
        .join(",");
    let dir = work.join(id);
    std::fs::create_dir_all(&dir).map_err(|e| tool_err("IO", e.to_string()))?;
    let output = std::process::Command::new("yt-dlp")
        .args([
            "--skip-download",
            "--no-playlist",
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            &sub_langs,
            "--sub-format",
            "vtt",
            "-o",
        ])
        .arg(dir.join("%(id)s.%(ext)s"))
        .arg("--")
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .output()
        .map_err(ytdlp_unavailable)?;
    let mut subs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| tool_err("IO", e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vtt"))
        .collect();
    if subs.is_empty() {
        return Err(if output.status.success() {
            tool_err(
                "NO_TRANSCRIPT",
                format!("Video {id} không có phụ đề cho: {sub_langs}"),
            )
        } else {
            tool_err("YTDLP_FAILED", stderr_tail(&output.stderr))
        });
    }
    let order: Vec<&str> = sub_langs.split(',').collect();
    subs.sort_by_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let rank = order
            .iter()
            .position(|l| name.contains(&format!(".{l}.")))
            .unwrap_or(usize::MAX);
        (rank, name)
    });
    let text = vtt_to_text(
        &std::fs::read_to_string(&subs[0]).map_err(|e| tool_err("IO", e.to_string()))?,
    );
    if text.is_empty() {
        return Err(tool_err("NO_TRANSCRIPT", "Phụ đề rỗng"));
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// Trạng thái dùng chung cho YouTube Tools & Native WikiEngine Tools
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct PlaylistVideoEntry {
    index: usize,
    id: String,
    title: String,
    stem: String,
}

impl PlaylistVideoEntry {
    fn source_scoped_path(&self) -> String {
        format!("youtube/{}.md", self.stem)
    }

    fn wiki_rel_path(&self) -> String {
        format!("wiki/sources/youtube/{}.md", self.stem)
    }
}

struct SharedEnv {
    engine: Arc<WikiEngine>,
    work_dir: PathBuf,
    playlist_url: String,
    langs: String,
    limit: usize,
    videos: Mutex<Vec<PlaylistVideoEntry>>,
}

// ---------------------------------------------------------------------------
// 1. Tool `yt_list_playlist`: Lấy danh sách video từ YouTube Playlist
// ---------------------------------------------------------------------------

struct YtListPlaylistTool {
    env: Arc<SharedEnv>,
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
                let stem = format!("{:02}-{}", i + 1, slugify(&title, 45));
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

struct YtFetchTranscriptTool {
    env: Arc<SharedEnv>,
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

struct WikiEngineBridgeTool {
    engine: Arc<WikiEngine>,
    spec: LlmToolSpec,
    effect: ToolEffect,
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
            let rel_path = res.get("info").and_then(|i| i.get("rel_path")).and_then(Value::as_str).unwrap_or("");
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
struct WikiDeleteFileTool {
    engine: Arc<WikiEngine>,
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

// ---------------------------------------------------------------------------
// Prompts cho Root Agent (`coordinator`) & Subagent (`video_ingestor`)
// ---------------------------------------------------------------------------

const COORDINATOR_PROMPT: &str = r#"You are the Root Wiki Architect Agent implementing Andrej Karpathy's "LLM Wiki" (`docs/karpathy-llm-wiki.md`).
You and your `video_ingestor` subagents own the ENTIRE workflow — the host does NOT summarize or create links for you.

Execute these steps in strict order:

1. **Load Taxonomy Skill & List Playlist**:
   - Call `skill` with `id="karpathy-wiki-taxonomy"`.
   - Call `yt_list_playlist` with the playlist URL from the user.

2. **Pre-Create the Canonical Taxonomy (Anti-Fragmentation)**:
   - Before spawning any subagents, call `wiki_write_file` (`scope="wiki"`) to create the initial canonical Entity and Concept pages so all subagents share the exact same slugs:
     - **4 Canonical Entities** in `entities/`:
       - `entities/nick-shawn.md` (`title: "Nick Shawn"`, `category: "entity"`)
       - `entities/metatrader-5.md` (`title: "MetaTrader 5 (MT5)"`, `category: "entity"`)
       - `entities/mql5.md` (`title: "MQL5 (MetaQuotes Language 5)"`, `category: "entity"`)
       - `entities/atr-indicator.md` (`title: "ATR Indicator (Average True Range)"`, `category: "entity"`)
     - **4 Canonical Concepts** in `concepts/`:
       - `concepts/dca-strategy.md` (`title: "Chiến lược DCA (Dollar Cost Averaging)"`, `category: "concept"`)
       - `concepts/martingale-grid.md` (`title: "Lưới Lệnh & Nhân Lot (Martingale Grid)"`, `category: "concept"`)
       - `concepts/multi-symbol-oop.md` (`title: "Kiến trúc OOP Đa Cặp Tiền (Multi-Symbol EA)"`, `category: "concept"`)
       - `concepts/risk-and-drawdown.md` (`title: "Quản trị Rủi ro, Drawdown & Kiểm thử Tick Data"`, `category: "concept"`)
   - Write a 15–20 line Vietnamese technical definition in each file with YAML frontmatter (`title`, `category`, `summary`, `tags`) and a `## Xuất hiện trong các Video` heading.

3. **Sequential Deep Video Ingestion via Subagent (`video_ingestor`)**:
   - Process the videos ONE AT A TIME sequentially using `subagent(agent="video_ingestor", description=..., prompt=...)`. Do NOT call multiple `subagent` tools in a single response! Wait for Video 1 to finish before calling `subagent` for Video 2, so Video 2 can see and link to Video 1 in `wiki/index.md`!
   - CRITICAL: Copy the `source_rel_path` and `wiki_rel_path` CHARACTER-FOR-CHARACTER from the `yt_list_playlist` output — NEVER abbreviate or truncate any filename!
   - In each subagent `prompt`, pass:
     - Exact `video_id`, `source_rel_path`, `wiki_rel_path`, and `title` from `yt_list_playlist`.
     - The exact canonical slugs: `related_entities: ["nick-shawn", "metatrader-5", "mql5", "atr-indicator"]` and `related_concepts: ["dca-strategy", "martingale-grid", "multi-symbol-oop", "risk-and-drawdown"]`.
     - The list of **already-ingested video wiki paths** (`wiki/sources/youtube/...`) so the subagent links directly to them using Obsidian `[[wiki/sources/youtube/<stem>|<Title>]]` syntax and patches them!

4. **Load Synthesis & Lint Skill (`karpathy-wiki-synthesis-lint`)**:
   - Call `skill` with `id="karpathy-wiki-synthesis-lint"`.
   - Call `wiki_file_synthesis` to create `syntheses/nick-shawn-dca-ea-synthesis.md` (70–100 lines in Vietnamese: executive thesis, architectural evolution across the 4 videos, a detailed Markdown comparison table of all 4 videos, links to all `[[wiki/sources/youtube/...]]`, `[[entities/...]]`, `[[concepts/...]]`, and a multi-line `flowchart LR` Mermaid diagram).
   - Call `wiki_patch_file` on `sources/youtube/01-...` (using the exact first video's `source_rel_path` prefixed with `sources/`) to append `- [[wiki/syntheses/nick-shawn-dca-ea-synthesis|Tổng hợp Chiến lược DCA của Nick Shawn]]` so the synthesis page has an inbound link.

5. **Run `wiki_lint` & Finish**:
   - Call `wiki_lint`.
   - If `broken_links`, `orphan_wiki_pages`, or `uningested_sources` is non-empty, fix them immediately (when replacing a broken `[[WikiLink]]`, include the full `[[raw_target]]` in `target_text`!) and re-run `wiki_lint` until `is_healthy: true`.
   - Finally, reply in Vietnamese with a comprehensive summary of the built Wiki.
"#;

const VIDEO_INGESTOR_PROMPT: &str = r#"You are the Deep Video Ingestor Subagent for Karpathy's LLM Wiki.
Execute these exact 4 steps for the single video assigned to you:

1. **Load Skill & Check Existing Wiki**:
   - Call `skill` with `id="deep-video-wiki-ingest"`.
   - Call `wiki_get` with `file="wiki/index.md"` to see what video pages (`wiki/sources/youtube/...`), `entities/`, and `concepts/` ALREADY exist in the wiki.

2. **Fetch Transcript & Create Raw Source**:
   - Call `yt_fetch_transcript` with `video_id`, `source_rel_path`, and `title`.
   - Note the exact `SOURCE_SCOPED_PATH` returned by `yt_fetch_transcript` (e.g. `youtube/03-...md`) and use that exact path for `wiki_ingest_source`!

3. **Ingest Comprehensive, Deep Technical Wiki Page (`wiki_ingest_source`)**:
   - Read the transcript carefully and call `wiki_ingest_source` ONCE (do NOT pass `target_wiki_path` or `raw_source_content` — omit them):
     - `source_rel_path`: exact `SOURCE_SCOPED_PATH` returned by `yt_fetch_transcript`
     - `title`: exact video title
     - `one_line_summary`: 1 concise Vietnamese sentence for `wiki/index.md`
     - `related_entities`: `["nick-shawn", "metatrader-5", "mql5", "atr-indicator"]`
     - `related_concepts`: `["dca-strategy", "martingale-grid", "multi-symbol-oop", "risk-and-drawdown"]`
     - `tags`: 4–6 tags
     - `wiki_markdown_body`: A comprehensive, detailed Vietnamese technical article (**80 to 120 lines**) extracting ALL concrete details from the transcript:
       - `## 1. Bối cảnh, Mục tiêu & Liên kết Chuỗi Video`: Explain the video's purpose in detail (6–8 sentences) and include inline `[[wiki/sources/youtube/<existing-stem>|<Title>]]` links ONLY to video pages that ALREADY exist in `wiki/index.md` (NEVER invent fake video filenames like `01-nguyen-tac-dca`!).
       - `## 2. Phân tích Chi tiết Quy tắc Chiến lược & Cấu trúc Code MQL5`: Write a thorough technical breakdown (25–40 lines) extracting every concrete parameter, variable, function, class, indicator setting, and bugfix from the transcript (e.g. `0.2%` / `2.0%` grid distance, `H1` / `D1` / `Period_D1` timeframe, `days_back = 50` vs `500`, `ATR(14)`, `iATR`, `CopyBuffer`, `PositionsTotal()`, `PositionGetDouble(POSITION_VOLUME)`, `CSymbolValidator`, `SymbolArray`, `StringSplit`, `Lots * 2`, `5` vs `2` open positions before doubling, `close_all` at `1%` profit). Include a 10–15 line `mql5` code block showing the exact MQL5 logic/bugfix discussed in the video, and inline `[[entities/<slug>|...]]` + `[[concepts/<slug>|...]]` links!
       - `## 3. Thông số Cấu hình & Bảng Kết quả Kiểm thử (Backtest / Thực nghiệm)`: Markdown table + detailed analysis of symbols (`AUDJPY`, `EURNZD`, 8-pair portfolio), timeframes, 10-year Tick Data (`2015–2025`), initial deposit (`$10,000`), how many times the account doubled (`100%` profit milestones) vs when it crashed, and drawdown metrics.
       - `## 4. Điểm Cốt lõi, Rủi ro & Phản biện (Key Takeaways & Caveats)`: 6–8 detailed technical takeaways + a `> [!WARNING] Contradiction / Caveat` callout block analyzing the discrepancy between the "99% winrate" claim and mechanical backtest reality.
       - `## 5. Sơ đồ Thuật toán & Luồng Thực thi`: Valid multi-line `flowchart TD` Mermaid diagram with 5–7 nodes and double-quoted node labels (`A["..."] --> B["..."]`).
       - `## 6. Liên kết & Tiến trình Tri thức trong Wiki`: Bullet list of Obsidian `[[WikiLinks]]` ONLY to existing pages in `wiki/index.md` (`[[wiki/sources/youtube/...]]`, `[[entities/...]]`, and `[[concepts/...]]`). NEVER invent links to pages that do not exist!

4. **Compound Entity & Concept Pages + Patch Existing Video Pages**:
   - Call `wiki_patch_file` (`scope="wiki"`) on each referenced canonical `entities/<slug>.md` and `concepts/<slug>.md` with `append_text="\n- [[wiki/sources/youtube/<this-stem>|<Title>]] (Nguồn gốc: [[sources/youtube/<this-stem>|<this-stem>.md]]) — <cập nhật chi tiết kỹ thuật từ video này>\n"`.
   - For each already-ingested video page in `wiki/sources/youtube/` that you saw in `wiki/index.md`: call `wiki_patch_file` (`scope="wiki"`, `rel_path="sources/youtube/<existing-stem>.md"`, `append_text="\n- ↔ [[wiki/sources/youtube/<this-stem>|<Title>]] — <mối liên hệ kỹ thuật giữa 2 video>\n"`).

5. Reply `DONE: <source_rel_path>` in 2–3 short sentences.
"#;

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

fn resolve_llm_credentials(env_file: &Path) -> Result<(String, String, String)> {
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

async fn run_orchestrator_turn(
    orch: &Orchestrator,
    playlist_url: &str,
    secret: &str,
    timeout: Duration,
) -> Result<()> {
    let caller = CallerContext::new("wiki-maintainer", "youtube-playlist-wiki");
    let session_id = orch.create_session(&caller, "coordinator").await?;
    let mut sub = orch.subscribe(&caller, &session_id).await?;
    let turn = orch
        .submit(
            &caller,
            &session_id,
            format!(
                "Build a deep, non-fragmented Karpathy LLM Wiki from this YouTube playlist.\nPlaylist URL: {playlist_url}"
            ),
        )
        .await?;

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
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
                println!(
                    "\n=== [Kết quả từ Agent `coordinator` ({})] ===\n{}",
                    result.status.as_str(),
                    result.output.unwrap_or_default()
                );
                if result.status != RunStatus::Completed {
                    return Err(io::Error::other(format!("Turn kết thúc với lỗi: {:?}", result.error)).into());
                }
                return Ok(());
            },
            _ = &mut deadline => {
                orch.cancel(&caller, &session_id).await?;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Quá thời gian xử lý playlist; đã cancel").into());
            }
        }
    }
}

/// Host chỉ đọc và xác minh kết quả trên đĩa sau khi Agent đã hoàn tất toàn bộ luồng.
fn verify_vault_on_disk(env: &SharedEnv) -> Result<()> {
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
                    "yt_fetch_transcript",
                    "wiki_tree",
                    "wiki_get",
                    "wiki_multi_get",
                    "wiki_query",
                    "wiki_ingest_source",
                    "wiki_write_file",
                    "wiki_patch_file",
                    "wiki_delete_file",
                    "wiki_file_synthesis",
                    "wiki_lint",
                    "wiki_status",
                    "wiki_log",
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
                    "wiki_tree",
                    "wiki_get",
                    "wiki_multi_get",
                    "wiki_query",
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
