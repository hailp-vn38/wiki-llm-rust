use crate::error::WikiResult;
use crate::fs::path_guard::resolve_safe_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Write `content` to `relative_path` inside `base_dir`, automatically creating all intermediate
/// nested parent folders (Requirement #6: e.g. `"docs/flow/lib.md"` creates `docs/` and `docs/flow/`).
///
/// Returns `(clean_relative_path, full_disk_path)`.
pub fn write_nested_file(
    base_dir: &Path,
    relative_path: &str,
    content: &str,
) -> WikiResult<(String, PathBuf)> {
    let (clean_rel, full_path) = resolve_safe_path(base_dir, relative_path)?;

    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&full_path, content)?;
    Ok((clean_rel, full_path))
}

/// Move or rename a file within `base_dir`, automatically creating any nested target folders
/// and cleaning up empty parent directories left behind.
pub fn move_nested_file(
    base_dir: &Path,
    from_rel_path: &str,
    to_rel_path: &str,
) -> WikiResult<(String, PathBuf)> {
    let (_, from_full) = resolve_safe_path(base_dir, from_rel_path)?;
    let (clean_to_rel, to_full) = resolve_safe_path(base_dir, to_rel_path)?;

    if let Some(parent) = to_full.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::rename(&from_full, &to_full)?;
    cleanup_empty_parents(base_dir, from_full.parent());
    Ok((clean_to_rel, to_full))
}

/// Delete a file inside `base_dir` and prune any now-empty intermediate parent folders up to `base_dir`.
pub fn delete_nested_file(base_dir: &Path, relative_path: &str) -> WikiResult<bool> {
    let (_, full_path) = resolve_safe_path(base_dir, relative_path)?;
    if !full_path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&full_path)?;
    cleanup_empty_parents(base_dir, full_path.parent());
    Ok(true)
}

fn cleanup_empty_parents(stop_at: &Path, start_parent: Option<&Path>) {
    let mut current = start_parent;
    while let Some(dir) = current {
        if dir == stop_at || !dir.starts_with(stop_at) {
            break;
        }
        match std::fs::read_dir(dir) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    let _ = std::fs::remove_dir(dir);
                    current = dir.parent();
                } else {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// Slice text by 1-indexed `from_line` and `max_lines`, optionally adding `1: ` line numbers (QMD format).
pub fn slice_and_format_lines(
    text: &str,
    from_line: Option<usize>,
    max_lines: Option<usize>,
    line_numbers: bool,
) -> String {
    let all_lines: Vec<&str> = text.lines().collect();
    if all_lines.is_empty() {
        return String::new();
    }

    let start_idx = from_line.unwrap_or(1).saturating_sub(1);
    if start_idx >= all_lines.len() {
        return String::new();
    }

    let end_idx = match max_lines {
        Some(count) => (start_idx + count).min(all_lines.len()),
        None => all_lines.len(),
    };

    let selected = &all_lines[start_idx..end_idx];
    if !line_numbers {
        return selected.join("\n");
    }

    let width = (end_idx).to_string().len();
    selected
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let line_no = start_idx + idx + 1;
            format!("{line_no:>width$}: {line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Default line window for bounded LLM file reads (mirrors `javis-os` `DOC_DONG_MAC_DINH = 200`).
pub const DEFAULT_READ_WINDOW_LINES: usize = 200;

/// Default character budget for bounded LLM file reads (mirrors `javis-os` `DOC_KY_TU_TOI_DA = 12_000`).
pub const DEFAULT_READ_MAX_CHARS: usize = 12_000;

/// Line-bounded & character-bounded reader inspired by `javis-os` (`coding_files.doc_theo_dong`):
/// - Reads starting at 1-indexed `from_line` (defaults to `1`) for up to `max_lines` lines (defaults to `DEFAULT_READ_WINDOW_LINES`).
/// - Never cuts mid-line when reaching `max_chars` (always stops at a clean `\n` line boundary while guaranteeing at least 1 line).
/// - When `end_line < total_lines`, appends an explicit continuation footer telling the LLM the exact `from_line` to pass on the next `wiki_get` call.
pub fn slice_lines_with_budget(
    text: &str,
    from_line: Option<usize>,
    max_lines: Option<usize>,
    max_chars: Option<usize>,
    line_numbers: bool,
) -> String {
    let all_lines: Vec<&str> = text.lines().collect();
    let total_lines = all_lines.len();
    let total_chars = text.chars().count();
    if total_lines == 0 {
        return String::new();
    }

    let start_line = from_line.unwrap_or(1).max(1);
    let start_idx = start_line - 1;
    if start_idx >= total_lines {
        return format!(
            "[Vượt quá cuối file: from_line={start_line} > tổng {total_lines} dòng ({total_chars} ký tự)]"
        );
    }

    let requested_window = max_lines.unwrap_or(DEFAULT_READ_WINDOW_LINES).max(1);
    let max_end_idx = (start_idx + requested_window).min(total_lines);
    let char_budget = max_chars.unwrap_or(DEFAULT_READ_MAX_CHARS).max(120);

    let mut actual_end_idx = start_idx;
    let mut accumulated_chars = 0usize;
    for (i, line) in all_lines[start_idx..max_end_idx].iter().enumerate() {
        let line_chars = line.chars().count() + 1;
        // Always include at least 1 line; otherwise stop cleanly at a line boundary before exceeding char_budget
        if i > 0 && accumulated_chars + line_chars > char_budget {
            break;
        }
        accumulated_chars += line_chars;
        actual_end_idx = start_idx + i + 1;
    }

    let selected = &all_lines[start_idx..actual_end_idx];
    let mut out = if !line_numbers {
        selected.join("\n")
    } else {
        let width = actual_end_idx.to_string().len();
        selected
            .iter()
            .enumerate()
            .map(|(idx, line)| {
                let line_no = start_idx + idx + 1;
                format!("{line_no:>width$}: {line}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let end_line = actual_end_idx;
    if end_line < total_lines {
        let next_from = end_line + 1;
        let next_window = (end_line - start_idx).max(60).min(requested_window);
        out.push_str(&format!(
            "\n… [Đã đọc dòng {start_line}-{end_line} trên tổng {total_lines} dòng ({total_chars} ký tự). Đọc tiếp đoạn sau: gọi lại wiki_get với from_line={next_from}, max_lines={next_window}]"
        ));
    } else if start_line > 1 {
        out.push_str(&format!(
            "\n[Đã đọc đến cuối file: dòng {start_line}-{end_line} trên tổng {total_lines} dòng]"
        ));
    }

    out
}

/// Adaptive read window configuration for `wiki_get` based on the LLM's `context_limit_tokens`:
/// - **Small models (`<= 16K` tokens)**: `110 lines / 5,200 chars`, compact index `3,200 chars`.
/// - **Medium models (`16K..=64K` tokens)**: `220 lines / 16,000 chars`, compact index `6,000 chars`.
/// - **Large models (`> 64K` tokens, e.g. 200K)**: `400 lines / 36,000 chars` (~9K tokens), compact index `12,000 chars`
///   (reads 95% of documents in a single turn while preventing "Lost in the Middle" on book-length files).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveReadBudget {
    pub default_lines: usize,
    pub max_window_chars: usize,
    pub compact_index_max_chars: usize,
}

impl Default for AdaptiveReadBudget {
    fn default() -> Self {
        Self::for_context_limit(200_000)
    }
}

impl AdaptiveReadBudget {
    pub fn for_context_limit(context_limit_tokens: usize) -> Self {
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

/// Fold a Vietnamese diacritic character to its ASCII equivalent (`đ -> d`, `ă/â -> a`, etc.).
pub fn fold_vietnamese(c: char) -> char {
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

/// Convert an arbitrary title/string (including Vietnamese diacritics) into a clean `kebab-case` slug
/// truncated to `max` characters.
pub fn slugify(s: &str, max: usize) -> String {
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

/// Sanitize a topic folder path inside `sources/` into `<domain>/<topic>` in `kebab-case` (max 2 levels).
pub fn sanitize_topic_folder(raw: &str) -> String {
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

/// Automatically replace malformed `</mermaid>` closing tags with standard CommonMark ```` ``` ```` fences.
pub fn sanitize_mermaid_fences(md: &str) -> String {
    if !md.contains("</mermaid>") && !md.contains("</MERMAID>") {
        return md.to_string();
    }
    let mut out = md
        .lines()
        .map(|line| {
            if line.trim().eq_ignore_ascii_case("</mermaid>") {
                "```"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if md.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Convert raw WebVTT subtitle content into clean multi-line text (~130 chars per line) suitable for
/// line-bounded pagination (`slice_lines_with_budget`).
pub fn vtt_to_text(vtt: &str) -> String {
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

/// Produce a 3-part (head / middle / tail) preview of a long transcript or document capped at `max` characters.
pub fn clip_transcript(text: &str, max: usize) -> String {
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
