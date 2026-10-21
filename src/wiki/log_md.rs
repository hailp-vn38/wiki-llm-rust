use crate::error::WikiResult;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Type of chronological operation logged in `wiki/log.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogAction {
    Init,
    Ingest,
    Query,
    Lint,
    Update,
    Custom,
}

impl LogAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Ingest => "ingest",
            Self::Query => "query",
            Self::Lint => "lint",
            Self::Update => "update",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "init" => Self::Init,
            "ingest" => Self::Ingest,
            "query" => Self::Query,
            "lint" => Self::Lint,
            "update" => Self::Update,
            _ => Self::Custom,
        }
    }
}

/// Parsed chronological entry from `wiki/log.md`.
/// Matches Karpathy's format: `## [YYYY-MM-DD HH:MM] <action> | <title>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub action: String,
    pub title: String,
    pub details: Vec<String>,
}

/// Initial header for `wiki/log.md`.
pub const DEFAULT_LOG_MD_HEADER: &str = "---\ntitle: \"Wiki Activity Log\"\ncategory: \"system\"\ntags:\n  - \"wiki/system\"\n  - \"wiki/log\"\n---\n\n# Wiki Evolution Log\n\n";

/// Append an entry to `wiki/log.md` in Karpathy's parseable format:
/// `## [YYYY-MM-DD HH:MM] <action> | <title>`
pub fn append_entry_to_log_file(
    log_path: &Path,
    action: LogAction,
    title: &str,
    details: &[String],
) -> WikiResult<String> {
    let mut current = if log_path.exists() {
        std::fs::read_to_string(log_path)?
    } else {
        DEFAULT_LOG_MD_HEADER.to_string()
    };

    if !current.ends_with('\n') {
        current.push('\n');
    }

    let ts = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let mut block = format!("## [{ts}] {} | {}\n", action.as_str(), title.trim());
    for d in details {
        block.push_str(&format!("- {}\n", d.trim()));
    }
    block.push('\n');

    current.push_str(&block);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(log_path, &current)?;
    Ok(current)
}

/// Parse the most recent `limit` entries from `wiki/log.md` (most recent first).
pub fn parse_log_entries(log_content: &str, limit: usize) -> Vec<LogEntry> {
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut current_entry: Option<LogEntry> = None;

    for line in log_content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("## [") {
            if let Some(prev) = current_entry.take() {
                entries.push(prev);
            }
            if let Some((ts, after_bracket)) = rest.split_once(']') {
                let (action_str, title_str) = match after_bracket.split_once('|') {
                    Some((a, t)) => (a.trim(), t.trim()),
                    None => ("log", after_bracket.trim()),
                };
                current_entry = Some(LogEntry {
                    timestamp: ts.trim().to_string(),
                    action: action_str.to_string(),
                    title: title_str.to_string(),
                    details: Vec::new(),
                });
            }
        } else if let Some(ref mut entry) = current_entry {
            if let Some(bullet) = trimmed.strip_prefix("- ") {
                entry.details.push(bullet.trim().to_string());
            } else if !trimmed.is_empty() {
                entry.details.push(trimmed.to_string());
            }
        }
    }

    if let Some(last) = current_entry {
        entries.push(last);
    }

    entries.reverse();
    if limit > 0 && entries.len() > limit {
        entries.truncate(limit);
    }
    entries
}
