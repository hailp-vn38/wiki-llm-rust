use crate::config::ChunkConfig;
use serde::{Deserialize, Serialize};

/// A single smartly segmented Markdown chunk produced by the QMD chunking algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownChunk {
    /// 0-indexed chunk sequence number within the document.
    pub seq: usize,
    /// Character offset in the original document body.
    pub char_pos: usize,
    /// 1-indexed start line in the full file.
    pub start_line: usize,
    /// 1-indexed end line in the full file.
    pub end_line: usize,
    /// Enclosing heading hierarchy (e.g. `"Architecture > Smart Chunking"`).
    pub heading_context: String,
    /// Raw chunk text.
    pub chunk_text: String,
    /// QMD EmbeddingGemma formatted text: `"title: {title} | text: {chunk_text}"`.
    pub formatted_text: String,
    /// Estimated token count of the chunk.
    pub token_estimate: usize,
}

#[derive(Debug, Clone)]
struct LineMeta {
    text: String,
    char_offset: usize,
    file_line_no: usize,
    token_est: usize,
    cum_tokens_before: usize,
    break_base_score: f32,
    heading_update: Option<(u8, String)>,
}

/// Perform QMD Smart Markdown Chunking:
/// - Target ~900 tokens per chunk with 15% overlap
/// - Break point scoring:
///   - `# ` (H1): 100, `## ` (H2): 90, `### ` (H3): 80, `#### ` (H4): 70, `##### ` (H5): 60, `###### ` (H6): 50
///   - Code fence boundary (` ``` ` or `~~~`): 80 (with Code Fence Protection: lines inside code blocks get score 0)
///   - Horizontal rule (`---`, `***`, `___`): 60
///   - Blank line: 20
///   - List item (`- `, `* `, `1. `): 5
///   - Line break: 1
/// - Squared distance decay inside the 200-token window before the target cutoff:
///   `final_score = base_score * (1.0 - (distance / window)^2 * 0.7)`
pub fn smart_chunk_markdown(
    body: &str,
    doc_title: &str,
    frontmatter_line_offset: usize,
    config: &ChunkConfig,
) -> Vec<MarkdownChunk> {
    let normalized = body.replace("\r\n", "\n");
    if normalized.trim().is_empty() {
        return Vec::new();
    }

    let chars_per_token = config.chars_per_token.max(1);
    let target_tokens = config.target_tokens.max(50);
    let window_tokens = config.window_tokens.min(target_tokens / 2).max(10);
    let overlap_tokens = ((target_tokens as f32) * config.overlap_ratio.clamp(0.0, 0.5)) as usize;

    let raw_lines: Vec<&str> = normalized.split('\n').collect();
    let mut lines_meta = Vec::with_capacity(raw_lines.len());

    let mut char_offset = 0usize;
    let mut cum_tokens = 0usize;
    let mut in_code_fence = false;
    let mut fence_marker = "";

    for (idx, &line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        let mut heading_update = None;

        // Score the break point BEFORE this line
        let break_base_score = if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            if !in_code_fence {
                in_code_fence = true;
                fence_marker = marker;
                80.0
            } else if trimmed.starts_with(fence_marker) {
                in_code_fence = false;
                fence_marker = "";
                80.0
            } else {
                0.0
            }
        } else if in_code_fence {
            // Code Fence Protection: ignore break points inside code blocks
            0.0
        } else if let Some((lvl, h_text)) = detect_heading(trimmed) {
            heading_update = Some((lvl, h_text));
            match lvl {
                1 => 100.0,
                2 => 90.0,
                3 => 80.0,
                4 => 70.0,
                5 => 60.0,
                _ => 50.0,
            }
        } else if is_horizontal_rule(trimmed) {
            60.0
        } else if trimmed.is_empty() {
            20.0
        } else if is_list_item(trimmed) {
            5.0
        } else {
            1.0
        };

        let line_chars = line.chars().count() + 1; // +1 for '\n'
        let token_est = line_chars.div_ceil(chars_per_token).max(1);

        lines_meta.push(LineMeta {
            text: line.to_string(),
            char_offset,
            file_line_no: frontmatter_line_offset + idx + 1,
            token_est,
            cum_tokens_before: cum_tokens,
            break_base_score,
            heading_update,
        });

        char_offset += line.len() + 1;
        cum_tokens += token_est;
    }

    let total_lines = lines_meta.len();
    let mut chunks = Vec::new();
    let mut start_idx = 0usize;
    let mut seq = 0usize;

    while start_idx < total_lines {
        let start_tokens = lines_meta[start_idx].cum_tokens_before;
        let cutoff_tokens = start_tokens + target_tokens;

        // Find the line index where cumulative tokens reach `cutoff_tokens`
        let mut target_idx = start_idx;
        while target_idx < total_lines
            && (lines_meta[target_idx].cum_tokens_before + lines_meta[target_idx].token_est)
                <= cutoff_tokens
        {
            target_idx += 1;
        }

        // If the remaining lines fit completely within target_tokens, emit final chunk
        if target_idx >= total_lines {
            if let Some(chunk) = build_chunk(&lines_meta, start_idx, total_lines, seq, doc_title) {
                chunks.push(chunk);
            }
            break;
        }

        let end_idx = if target_idx <= start_idx + 1 {
            (start_idx + 1).min(total_lines)
        } else {
            // Search window [cutoff_tokens - window_tokens, cutoff_tokens]
            let window_start_tokens = cutoff_tokens.saturating_sub(window_tokens);
            let mut best_cut_idx = None;
            let mut best_score = -1.0f32;

            for (candidate_idx, candidate_line) in lines_meta
                .iter()
                .enumerate()
                .take(target_idx + 1)
                .skip(start_idx + 1)
            {
                let base_score = candidate_line.break_base_score;
                if base_score <= 0.0 {
                    continue;
                }
                let pos_tokens = candidate_line.cum_tokens_before;
                if pos_tokens < window_start_tokens {
                    continue;
                }

                let distance = (cutoff_tokens.saturating_sub(pos_tokens)) as f32;
                let window_f = (window_tokens as f32).max(1.0);
                let ratio = (distance / window_f).clamp(0.0, 1.0);
                let final_score = base_score * (1.0 - ratio * ratio * 0.7);

                if final_score >= best_score {
                    best_score = final_score;
                    best_cut_idx = Some(candidate_idx);
                }
            }

            // Fallback if we were inside a giant code block with no positive scores in window
            best_cut_idx.unwrap_or(target_idx)
        };

        if let Some(chunk) = build_chunk(&lines_meta, start_idx, end_idx, seq, doc_title) {
            chunks.push(chunk);
            seq += 1;
        }

        if end_idx >= total_lines {
            break;
        }

        // Compute next `start_idx` with 15% overlap, ensuring strict forward progress
        let end_cum_tokens = lines_meta[end_idx].cum_tokens_before;
        let desired_overlap_start = end_cum_tokens.saturating_sub(overlap_tokens);
        let mut next_start = end_idx;
        while next_start > start_idx + 1
            && lines_meta[next_start - 1].cum_tokens_before >= desired_overlap_start
            && lines_meta[next_start - 1].break_base_score > 0.0
        {
            next_start -= 1;
        }
        start_idx = next_start.max(start_idx + 1);
    }

    chunks
}

fn build_chunk(
    lines_meta: &[LineMeta],
    start_idx: usize,
    end_idx: usize,
    seq: usize,
    doc_title: &str,
) -> Option<MarkdownChunk> {
    if start_idx >= end_idx || start_idx >= lines_meta.len() {
        return None;
    }

    let chunk_lines: Vec<&str> = lines_meta[start_idx..end_idx]
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    let chunk_text = chunk_lines.join("\n").trim().to_string();
    if chunk_text.is_empty() {
        return None;
    }

    // Reconstruct active heading breadcrumb up to `end_idx`
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    for line in &lines_meta[..end_idx] {
        if let Some((lvl, ref title)) = line.heading_update {
            while let Some((last_lvl, _)) = heading_stack.last() {
                if *last_lvl >= lvl {
                    heading_stack.pop();
                } else {
                    break;
                }
            }
            heading_stack.push((lvl, title.clone()));
        }
    }
    let heading_context = heading_stack
        .into_iter()
        .map(|(_, t)| t)
        .collect::<Vec<_>>()
        .join(" > ");

    let token_estimate: usize = lines_meta[start_idx..end_idx]
        .iter()
        .map(|l| l.token_est)
        .sum();

    let formatted_text = if heading_context.is_empty() {
        format!("title: {doc_title} | text: {chunk_text}")
    } else {
        format!("title: {doc_title} ({heading_context}) | text: {chunk_text}")
    };

    Some(MarkdownChunk {
        seq,
        char_pos: lines_meta[start_idx].char_offset,
        start_line: lines_meta[start_idx].file_line_no,
        end_line: lines_meta[end_idx - 1].file_line_no,
        heading_context,
        chunk_text,
        formatted_text,
        token_estimate,
    })
}

fn detect_heading(trimmed: &str) -> Option<(u8, String)> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            let text = rest.trim().trim_end_matches('#').trim();
            if !text.is_empty() {
                return Some((hashes as u8, text.to_string()));
            }
        }
    }
    None
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let chars: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.len() < 3 {
        return false;
    }
    let first = chars[0];
    (first == '-' || first == '*' || first == '_') && chars.iter().all(|&c| c == first)
}

fn is_list_item(trimmed: &str) -> bool {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        return rest.starts_with(". ") || rest.starts_with(") ");
    }
    false
}
