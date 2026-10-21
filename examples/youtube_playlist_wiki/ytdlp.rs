use std::io;
use std::path::{Path, PathBuf};

use orchestrator_agent_rs::ToolError;

use crate::tools::tool_err;

// ---------------------------------------------------------------------------
// Tiện ích xử lý chuỗi & WebVTT
// ---------------------------------------------------------------------------

pub(crate) fn fold_vietnamese(c: char) -> char {
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

pub(crate) fn slugify(s: &str, max: usize) -> String {
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
pub(crate) fn vtt_to_text(vtt: &str) -> String {
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
    // Gom các cue phụ đề thành các dòng văn bản dài ~120–140 ký tự (giống chuẩn đọc theo dòng `doc_theo_dong` của javis-os)
    // để agent có thể đọc phân trang bằng `from_line` / `max_lines` thay vì dồn toàn bộ transcript vào 1 dòng khổng lồ.
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    for cue in out {
        if current_line.is_empty() {
            current_line.push_str(&cue);
        } else if current_line.chars().count() + 1 + cue.chars().count() <= 130 {
            current_line.push(' ');
            current_line.push_str(&cue);
        } else {
            lines.push(std::mem::take(&mut current_line));
            current_line.push_str(&cue);
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    lines.join("\n")
}

pub(crate) fn clip_transcript(text: &str, max: usize) -> String {
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

pub(crate) fn fetch_playlist(url: &str) -> std::result::Result<Vec<(String, String)>, ToolError> {
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

pub(crate) fn fetch_transcript(
    work: &Path,
    id: &str,
    langs: &str,
) -> std::result::Result<String, ToolError> {
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

