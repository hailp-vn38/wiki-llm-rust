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
