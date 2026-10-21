use crate::markdown::frontmatter::{parse_frontmatter, ParsedFrontmatter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Heading information extracted from Markdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadingInfo {
    pub level: u8,
    pub text: String,
    pub line_number: usize,
}

/// Kind of link extracted from a Markdown document for Obsidian Graph View.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractedLinkType {
    /// Standard Obsidian WikiLink: `[[target]]` or `[[target#heading|alias]]`
    WikiLink,
    /// Obsidian Embed: `![[target]]`
    Embed,
    /// Standard Markdown internal link: `[text](path/to/note.md)`
    Markdown,
}

impl ExtractedLinkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WikiLink => "wikilink",
            Self::Embed => "embed",
            Self::Markdown => "markdown",
        }
    }
}

/// A link extracted from a Markdown document (either in frontmatter properties or body).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedLink {
    /// Original inner string (e.g. `"concepts/bm25-search#Scoring|BM25"`).
    pub raw_target: String,
    /// Path/stem portion before `#` and `|` (e.g. `"concepts/bm25-search"`).
    pub target_path_spec: String,
    /// Optional heading anchor after `#` (e.g. `Some("Scoring")`).
    pub target_heading: Option<String>,
    /// Optional display alias after `|` (e.g. `Some("BM25")`).
    pub display_alias: Option<String>,
    /// Link type (`WikiLink`, `Embed`, `Markdown`).
    pub link_type: ExtractedLinkType,
    /// 1-indexed line number in the full file.
    pub line_number: usize,
}

/// Full parsed representation of a Markdown file.
#[derive(Debug, Clone)]
pub struct ParsedMarkdownDoc {
    pub frontmatter: ParsedFrontmatter,
    pub title: String,
    pub headings: Vec<HeadingInfo>,
    pub tags: Vec<String>,
    pub links: Vec<ExtractedLink>,
    pub line_count: usize,
}

/// Parse a Markdown document to extract frontmatter, title, headings, tags, and Obsidian graph links.
pub fn parse_markdown_document(full_content: &str, fallback_file_stem: &str) -> ParsedMarkdownDoc {
    let fm = parse_frontmatter(full_content);
    let normalized = full_content.replace("\r\n", "\n");
    let line_count = normalized.lines().count().max(1);

    let mut headings = Vec::new();
    let mut links = Vec::new();
    let mut tags_set: BTreeSet<String> = fm.tags.iter().cloned().collect();

    // 1. Extract [[WikiLinks]] from YAML frontmatter lines (Obsidian Properties support [[WikiLinks]])
    if let Some(raw_yaml) = &fm.raw_yaml {
        for (idx, line) in raw_yaml.lines().enumerate() {
            let line_no = idx + 2; // +1 for 1-indexed, +1 for opening `---`
            extract_wikilinks_from_line(line, line_no, &mut links);
        }
    }

    // 2. Scan body lines while tracking fenced code blocks (` ``` ` or `~~~`)
    let mut in_code_fence = false;
    let mut active_fence_marker = "";

    for (idx, line) in fm.body.lines().enumerate() {
        let line_no = fm.frontmatter_lines + idx + 1;
        let trimmed = line.trim_start();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            if !in_code_fence {
                in_code_fence = true;
                active_fence_marker = marker;
            } else if trimmed.starts_with(active_fence_marker) {
                in_code_fence = false;
                active_fence_marker = "";
            }
            continue;
        }

        if in_code_fence {
            continue;
        }

        // Check if line is an ATX Heading (`# Heading` .. `###### Heading`)
        if let Some((level, heading_text)) = parse_atx_heading(trimmed) {
            headings.push(HeadingInfo {
                level,
                text: heading_text,
                line_number: line_no,
            });
        }

        // Strip inline code spans (`...`) before extracting links and #tags
        let line_without_inline_code = strip_inline_code(line);
        extract_wikilinks_from_line(&line_without_inline_code, line_no, &mut links);
        extract_markdown_links_from_line(&line_without_inline_code, line_no, &mut links);
        extract_inline_tags_from_line(&line_without_inline_code, &mut tags_set);
    }

    // Determine document title: frontmatter `title` -> first H1 -> first heading -> fallback_file_stem
    let title = fm
        .title
        .clone()
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.level == 1)
                .map(|h| h.text.clone())
        })
        .or_else(|| headings.first().map(|h| h.text.clone()))
        .unwrap_or_else(|| fallback_file_stem.to_string());

    let mut final_fm = fm;
    let tags: Vec<String> = tags_set.into_iter().collect();
    if !tags.is_empty() && !final_fm.qmd_metadata.contains_key("tags") {
        final_fm.qmd_metadata.insert(
            "tags".to_string(),
            crate::markdown::frontmatter::TypedMetaValue::StringArray(tags.clone()),
        );
    }

    ParsedMarkdownDoc {
        frontmatter: final_fm,
        title,
        headings,
        tags,
        links,
        line_count,
    }
}

fn parse_atx_heading(trimmed_line: &str) -> Option<(u8, String)> {
    let hashes = trimmed_line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed_line[hashes..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            let text = rest.trim().trim_end_matches('#').trim();
            if !text.is_empty() {
                return Some((hashes as u8, text.to_string()));
            }
        }
    }
    None
}

fn strip_inline_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for ch in line.chars() {
        if ch == '`' {
            in_code = !in_code;
            out.push(' ');
        } else if in_code {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse all `[[...]]` and `![[...]]` occurrences on a single line.
pub fn extract_wikilinks_from_line(line: &str, line_number: usize, out: &mut Vec<ExtractedLink>) {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 3 < len {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let is_embed = i > 0 && bytes[i - 1] == b'!';
            let content_start = i + 2;
            let mut close_pos = None;
            let mut j = content_start;
            while j + 1 < len {
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    close_pos = Some(j);
                    break;
                }
                j += 1;
            }

            if let Some(end) = close_pos {
                if let Some(inner) = line.get(content_start..end) {
                    if let Some(extracted) = parse_wikilink_inner(
                        inner,
                        if is_embed {
                            ExtractedLinkType::Embed
                        } else {
                            ExtractedLinkType::WikiLink
                        },
                        line_number,
                    ) {
                        out.push(extracted);
                    }
                }
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
}

fn parse_wikilink_inner(
    inner: &str,
    link_type: ExtractedLinkType,
    line_number: usize,
) -> Option<ExtractedLink> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Handle escaped pipe `\|` inside tables vs standard `|` alias separator
    let normalized_inner = trimmed.replace("\\|", "|");
    let (target_and_heading, display_alias) = match normalized_inner.split_once('|') {
        Some((left, right)) => {
            let alias = right.trim();
            (
                left.trim(),
                if alias.is_empty() {
                    None
                } else {
                    Some(alias.to_string())
                },
            )
        }
        None => (normalized_inner.as_str(), None),
    };

    let (path_spec, target_heading) = match target_and_heading.split_once('#') {
        Some((p, h)) => {
            let heading = h.trim();
            (
                p.trim(),
                if heading.is_empty() {
                    None
                } else {
                    Some(heading.to_string())
                },
            )
        }
        None => (target_and_heading.trim(), None),
    };

    // Ignore same-file heading anchors like `[[#Section]]` for cross-file graph edges
    if path_spec.is_empty() {
        return None;
    }

    Some(ExtractedLink {
        raw_target: trimmed.to_string(),
        target_path_spec: path_spec.replace('\\', "/"),
        target_heading,
        display_alias,
        link_type,
        line_number,
    })
}

fn extract_markdown_links_from_line(line: &str, line_number: usize, out: &mut Vec<ExtractedLink>) {
    // Match standard `[label](target)` where it's not a `[[wikilink]]`
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let n = chars.len();
    let mut idx = 0;

    while idx < n {
        let (byte_pos, ch) = chars[idx];
        if ch == '[' {
            // Skip if part of `[[`
            if idx + 1 < n && chars[idx + 1].1 == '[' {
                idx += 2;
                continue;
            }
            if idx > 0 && chars[idx - 1].1 == '[' {
                idx += 1;
                continue;
            }

            // Find closing `]` followed immediately by `(`
            let mut close_bracket = None;
            let mut k = idx + 1;
            while k < n {
                if chars[k].1 == ']' {
                    close_bracket = Some(k);
                    break;
                }
                k += 1;
            }

            if let Some(cb_idx) = close_bracket {
                if cb_idx + 1 < n && chars[cb_idx + 1].1 == '(' {
                    let open_paren_idx = cb_idx + 1;
                    let mut close_paren = None;
                    let mut m = open_paren_idx + 1;
                    while m < n {
                        if chars[m].1 == ')' {
                            close_paren = Some(m);
                            break;
                        }
                        m += 1;
                    }

                    if let Some(cp_idx) = close_paren {
                        let label_start = byte_pos + 1;
                        let label_end = chars[cb_idx].0;
                        let url_start = chars[open_paren_idx].0 + 1;
                        let url_end = chars[cp_idx].0;

                        let label = line[label_start..label_end].trim();
                        let raw_url = line[url_start..url_end].trim();
                        // Strip optional title in `[label](path.md "title")`
                        let url_part = raw_url.split_whitespace().next().unwrap_or("");

                        if !url_part.is_empty()
                            && !url_part.starts_with("http://")
                            && !url_part.starts_with("https://")
                            && !url_part.starts_with("mailto:")
                            && !url_part.starts_with('#')
                        {
                            let (path_spec, heading) = match url_part.split_once('#') {
                                Some((p, h)) => (
                                    p.trim(),
                                    if h.is_empty() {
                                        None
                                    } else {
                                        Some(h.to_string())
                                    },
                                ),
                                None => (url_part, None),
                            };
                            if !path_spec.is_empty() {
                                out.push(ExtractedLink {
                                    raw_target: url_part.to_string(),
                                    target_path_spec: path_spec.replace('\\', "/"),
                                    target_heading: heading,
                                    display_alias: if label.is_empty() {
                                        None
                                    } else {
                                        Some(label.to_string())
                                    },
                                    link_type: ExtractedLinkType::Markdown,
                                    line_number,
                                });
                            }
                        }
                        idx = cp_idx + 1;
                        continue;
                    }
                }
            }
        }
        idx += 1;
    }
}

fn extract_inline_tags_from_line(line: &str, tags_set: &mut BTreeSet<String>) {
    // Strip `[[...]]` first so `[[page#heading]]` doesn't get parsed as `#heading` tag
    let mut scrubbed = String::with_capacity(line.len());
    let mut skip_depth: usize = 0;
    for ch in line.chars() {
        if ch == '[' || ch == '(' {
            skip_depth += 1;
            scrubbed.push(' ');
        } else if ch == ']' || ch == ')' {
            skip_depth = skip_depth.saturating_sub(1);
            scrubbed.push(' ');
        } else if skip_depth > 0 {
            scrubbed.push(' ');
        } else {
            scrubbed.push(ch);
        }
    }

    for token in scrubbed.split_whitespace() {
        if let Some(tag) = token.strip_prefix('#') {
            let clean = tag.trim_end_matches(|c: char| {
                matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'')
            });
            // Valid Obsidian tag: must contain at least one non-digit letter and only alphanumeric, `_`, `-`, `/`
            if !clean.is_empty()
                && clean.chars().any(|c| c.is_alphabetic())
                && clean
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '/')
            {
                tags_set.insert(clean.to_string());
            }
        }
    }
}
