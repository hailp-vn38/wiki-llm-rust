use crate::error::WikiResult;
use crate::fs::path_guard::resolve_safe_path;
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
