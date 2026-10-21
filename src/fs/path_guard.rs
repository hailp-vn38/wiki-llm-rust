use crate::error::{WikiError, WikiResult};
use std::path::{Component, Path, PathBuf};

/// Normalize a relative path to use forward slashes (`/`), strip leading slashes,
/// and reject any path traversal (`..`), root path, or Windows drive prefix.
pub fn sanitize_rel_path(raw_rel_path: &str) -> WikiResult<String> {
    let trimmed = raw_rel_path.trim();
    if trimmed.is_empty() {
        return Err(WikiError::InvalidFileName(raw_rel_path.to_string()));
    }

    let normalized = trimmed.replace('\\', "/");
    let stripped = normalized.trim_start_matches('/');
    if stripped.is_empty() {
        return Err(WikiError::PathTraversal(raw_rel_path.to_string()));
    }

    let path = Path::new(stripped);
    let mut clean_parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(os_str) => {
                let part = os_str
                    .to_str()
                    .ok_or_else(|| WikiError::InvalidFileName(raw_rel_path.to_string()))?;
                if part.is_empty() {
                    continue;
                }
                clean_parts.push(part);
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WikiError::PathTraversal(raw_rel_path.to_string()));
            }
        }
    }

    if clean_parts.is_empty() {
        return Err(WikiError::InvalidFileName(raw_rel_path.to_string()));
    }

    Ok(clean_parts.join("/"))
}

/// Ensure a relative markdown path ends with `.md` if it doesn't already have an extension.
pub fn ensure_markdown_extension(rel_path: &str) -> WikiResult<String> {
    let clean = sanitize_rel_path(rel_path)?;
    let p = Path::new(&clean);
    if p.extension().is_none() {
        Ok(format!("{clean}.md"))
    } else {
        Ok(clean)
    }
}

/// Resolve a safe full path inside `base_dir` from a sanitized relative path.
pub fn resolve_safe_path(base_dir: &Path, raw_rel_path: &str) -> WikiResult<(String, PathBuf)> {
    let clean_rel = sanitize_rel_path(raw_rel_path)?;
    let full = base_dir.join(&clean_rel);
    Ok((clean_rel, full))
}

/// Generate a deterministic 6-character hexadecimal QMD `docid` (prefixed with `#`, e.g., `"#a1b2c3"`)
/// from the vault-relative path (`rel_path`).
pub fn compute_docid(vault_rel_path: &str) -> String {
    let hash = blake3::hash(vault_rel_path.as_bytes()).to_hex();
    format!("#{}", &hash[..6])
}

/// Compute full BLAKE3 hex digest of file content.
pub fn compute_content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

/// Parse QMD `file[:from[:count]]` suffix notation, e.g. `"docs/flow/lib.md:10:25"` or `"#a1b2c3:50"`.
pub fn parse_target_with_line_range(input: &str) -> (&str, Option<usize>, Option<usize>) {
    let parts: Vec<&str> = input.split(':').collect();
    match parts.as_slice() {
        [base, from_str, count_str] => {
            if let (Ok(from), Ok(count)) = (from_str.parse::<usize>(), count_str.parse::<usize>()) {
                return (base, Some(from), Some(count));
            }
            (input, None, None)
        }
        [base, from_str] => {
            if let Ok(from) = from_str.parse::<usize>() {
                return (base, Some(from), None);
            }
            (input, None, None)
        }
        _ => (input, None, None),
    }
}
