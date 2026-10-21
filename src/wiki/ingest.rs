use crate::config::{WikiConfig, WikiSourceLayout};
use crate::error::{WikiError, WikiResult};
use crate::fs::path_guard::ensure_markdown_extension;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Request to ingest a source document from `sources/` into `wiki/` (Requirement #8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSourceRequest {
    /// Relative path of the source file inside `sources/` (e.g. `"docs/flow/lib.md"` or `"sources/docs/flow/lib.md"`).
    /// By default, the generated wiki file strictly preserves the source's filename (`"lib.md"`).
    pub source_rel_path: String,
    /// Optional existing wiki page path (inside `wiki/`) to update instead of creating a new file.
    /// Useful when a new source document only supplements an already-existing wiki page
    /// (e.g. `Some("sources/docs/flow/lib.md")` or `Some("entities/rust.md")`).
    #[serde(default, deserialize_with = "deserialize_non_empty_opt_string")]
    pub target_wiki_path: Option<String>,
    /// Optional raw content if you want `ingest_source_to_wiki` to also create/save the source file on disk first.
    #[serde(default, deserialize_with = "deserialize_non_empty_opt_string")]
    pub raw_source_content: Option<String>,
    /// Human-readable title for the wiki summary page.
    pub title: String,
    /// Concise one-line summary registered in `wiki/index.md`.
    pub one_line_summary: String,
    /// Markdown body of the wiki page (should include `[[WikiLinks]]`).
    pub wiki_markdown_body: String,
    /// Obsidian tags to attach in frontmatter.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Related entity page names (e.g. `["rust", "sqlite"]`).
    #[serde(default)]
    pub related_entities: Vec<String>,
    /// Related concept page names (e.g. `["bm25-search", "smart-chunking"]`).
    #[serde(default)]
    pub related_concepts: Vec<String>,
}

fn deserialize_non_empty_opt_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.and_then(|s| {
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }))
}

/// Response returned after ingesting a source into the wiki.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSourceResponse {
    /// Vault-relative path of the raw source file (e.g. `"sources/docs/flow/lib.md"`).
    pub source_rel_path: String,
    /// Scoped path of the wiki page inside `wiki/` (e.g. `"sources/docs/flow/lib.md"`).
    pub wiki_scoped_path: String,
    /// Vault-relative path of the created/updated wiki page (e.g. `"wiki/sources/docs/flow/lib.md"`).
    pub wiki_rel_path: String,
    /// Preserved file name (e.g. `"lib.md"` — identical to the source's file name, Requirement #8).
    pub file_name: String,
    /// Short QMD document ID of the created wiki page (e.g. `"#e8b210"`).
    pub wiki_docid: String,
}

/// Strip leading `"sources/"` prefix if the caller passed a vault-relative path instead of a scoped path,
/// and ensure `.md` extension.
pub fn normalize_source_scoped_path(
    raw_source_path: &str,
    sources_dir_name: &str,
) -> WikiResult<String> {
    let clean = ensure_markdown_extension(raw_source_path)?;
    let prefix = format!("{sources_dir_name}/");
    let stripped = clean.strip_prefix(&prefix).unwrap_or(&clean);
    if stripped.is_empty() {
        return Err(WikiError::InvalidFileName(raw_source_path.to_string()));
    }
    Ok(stripped.to_string())
}

/// Map a scoped path inside `sources/` (e.g., `"docs/flow/lib.md"`) to its corresponding
/// scoped path inside `wiki/` (e.g., `"sources/docs/flow/lib.md"`).
///
/// **Requirement #8**: The resulting wiki file ALWAYS keeps the exact same `file_name` (`"lib.md"`)
/// as the file in `sources/`, never an auto-generated ID or hash.
pub fn map_source_to_wiki_scoped_path(
    source_scoped_path: &str,
    config: &WikiConfig,
) -> WikiResult<(String, String)> {
    let clean_source = normalize_source_scoped_path(source_scoped_path, &config.sources_dir_name)?;
    let path = Path::new(&clean_source);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| WikiError::InvalidFileName(source_scoped_path.to_string()))?
        .to_string();

    let wiki_scoped = match config.wiki_source_layout {
        WikiSourceLayout::MirrorUnderWikiSources => format!("sources/{clean_source}"),
        WikiSourceLayout::MirrorDirectlyInWiki => clean_source,
        WikiSourceLayout::FlatInWikiSources => format!("sources/{file_name}"),
    };

    Ok((wiki_scoped, file_name))
}

/// Format the full Markdown document for a source summary or updated wiki page in `wiki/`,
/// including Obsidian-compatible YAML frontmatter, QMD metadata, and `[[WikiLinks]]`.
/// Supports multiple contributing raw sources (`previous_sources`) when updating an existing wiki page.
pub fn render_source_wiki_markdown(
    req: &IngestSourceRequest,
    vault_source_rel_path: &str,
    source_file_name: &str,
    previous_source_rel_paths: &[String],
) -> String {
    let now = Utc::now().to_rfc3339();
    let mut tags = vec!["wiki/source".to_string()];
    for t in &req.tags {
        let clean_t = t.trim().trim_start_matches('#').to_string();
        if !clean_t.is_empty() && !tags.contains(&clean_t) {
            tags.push(clean_t);
        }
    }

    let tags_yaml = tags
        .iter()
        .map(|t| format!("  - \"{t}\""))
        .collect::<Vec<_>>()
        .join("\n");

    let qmd_tags_yaml = tags
        .iter()
        .map(|t| format!("      - \"{t}\""))
        .collect::<Vec<_>>()
        .join("\n");

    let mut all_source_paths: Vec<String> = Vec::new();
    for prev in previous_source_rel_paths {
        let clean = prev.trim().to_string();
        if !clean.is_empty() && !all_source_paths.contains(&clean) {
            all_source_paths.push(clean);
        }
    }
    if !all_source_paths.contains(&vault_source_rel_path.to_string()) {
        all_source_paths.push(vault_source_rel_path.to_string());
    }

    let sources_wikilink_yaml = all_source_paths
        .iter()
        .map(|p| {
            let fname = Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            format!("  - \"[[{p}|{fname}]]\"")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let sources_plain_yaml = all_source_paths
        .iter()
        .map(|p| format!("      - \"{p}\""))
        .collect::<Vec<_>>()
        .join("\n");

    let escaped_title = req.title.replace('"', "\\\"");
    let escaped_summary = req.one_line_summary.replace('"', "\\\"");
    let source_wikilink = format!("[[{vault_source_rel_path}|{source_file_name}]]");
    let source_count = all_source_paths.len();

    let mut out = format!(
        "---\ntitle: \"{escaped_title}\"\ncategory: \"source_summary\"\nsummary: \"{escaped_summary}\"\nsource_file: \"{source_wikilink}\"\nsource_count: {source_count}\nsources:\n{sources_wikilink_yaml}\ntags:\n{tags_yaml}\nupdated_at: \"{now}\"\nqmd:\n  metadata:\n    category: \"source_summary\"\n    source_file: \"{vault_source_rel_path}\"\n    source_count: {source_count}\n    sources:\n{sources_plain_yaml}\n    tags:\n{qmd_tags_yaml}\n---\n\n"
    );

    if !req.wiki_markdown_body.trim_start().starts_with("# ") {
        out.push_str(&format!("# {}\n\n", req.title));
    }

    let sources_inline_links = all_source_paths
        .iter()
        .map(|p| {
            let fname = Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            format!("[[{p}|{fname}]]")
        })
        .collect::<Vec<_>>()
        .join(", ");

    out.push_str(&format!(
        "> **Raw Sources:** {sources_inline_links}  \n> **Summary:** {}\n\n",
        req.one_line_summary.trim()
    ));

    out.push_str(req.wiki_markdown_body.trim());
    out.push('\n');

    // Append explicit Related Entities / Concepts links if not already mentioned in body
    let mut missing_entity_links = Vec::new();
    for ent in &req.related_entities {
        let clean = ent.trim().trim_matches(|c| c == '[' || c == ']');
        let already_in_body = req.wiki_markdown_body.contains(&format!("[[{clean}"))
            || req
                .wiki_markdown_body
                .contains(&format!("[[entities/{clean}"))
            || req
                .wiki_markdown_body
                .contains(&format!("[[wiki/entities/{clean}"));
        if !clean.is_empty() && !already_in_body {
            missing_entity_links.push(format!("[[entities/{clean}|{clean}]]"));
        }
    }

    let mut missing_concept_links = Vec::new();
    for con in &req.related_concepts {
        let clean = con.trim().trim_matches(|c| c == '[' || c == ']');
        let already_in_body = req.wiki_markdown_body.contains(&format!("[[{clean}"))
            || req
                .wiki_markdown_body
                .contains(&format!("[[concepts/{clean}"))
            || req
                .wiki_markdown_body
                .contains(&format!("[[wiki/concepts/{clean}"));
        if !clean.is_empty() && !already_in_body {
            missing_concept_links.push(format!("[[concepts/{clean}|{clean}]]"));
        }
    }

    if !missing_entity_links.is_empty() || !missing_concept_links.is_empty() {
        out.push_str("\n## Cross-References\n");
        if !missing_entity_links.is_empty() {
            out.push_str(&format!(
                "- **Entities:** {}\n",
                missing_entity_links.join(", ")
            ));
        }
        if !missing_concept_links.is_empty() {
            out.push_str(&format!(
                "- **Concepts:** {}\n",
                missing_concept_links.join(", ")
            ));
        }
    }

    out
}

/// Request to file a valuable query answer / synthesis back into `wiki/syntheses/`
/// (Karpathy's Query operation: "good answers can be filed back into the wiki as new pages").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSynthesisRequest {
    /// Relative path inside `wiki/` (e.g. `"syntheses/rag-vs-llm-wiki.md"` or just `"rag-vs-llm-wiki.md"`).
    pub rel_path: String,
    /// Original user question or exploration topic that produced this synthesis.
    pub query: String,
    /// Title of the synthesis page.
    pub title: String,
    /// One-line summary for `wiki/index.md`.
    pub one_line_summary: String,
    /// Markdown body (can be an analysis, comparison table, or Marp slide deck with `[[WikiLinks]]`).
    pub markdown_body: String,
    /// Tags to attach in YAML frontmatter.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Format a synthesis page with Obsidian & Dataview compatible frontmatter.
pub fn render_synthesis_markdown(req: &FileSynthesisRequest) -> String {
    let now = Utc::now().to_rfc3339();
    let mut tags = vec!["wiki/synthesis".to_string()];
    for t in &req.tags {
        let clean = t.trim().trim_start_matches('#').to_string();
        if !clean.is_empty() && !tags.contains(&clean) {
            tags.push(clean);
        }
    }
    let tags_yaml = tags
        .iter()
        .map(|t| format!("  - \"{t}\""))
        .collect::<Vec<_>>()
        .join("\n");
    let qmd_tags_yaml = tags
        .iter()
        .map(|t| format!("      - \"{t}\""))
        .collect::<Vec<_>>()
        .join("\n");

    let escaped_title = req.title.replace('"', "\\\"");
    let escaped_summary = req.one_line_summary.replace('"', "\\\"");
    let escaped_query = req.query.replace('"', "\\\"");

    let mut out = format!(
        "---\ntitle: \"{escaped_title}\"\ncategory: \"synthesis\"\nsummary: \"{escaped_summary}\"\nquery: \"{escaped_query}\"\ntags:\n{tags_yaml}\nupdated_at: \"{now}\"\nqmd:\n  metadata:\n    category: \"synthesis\"\n    tags:\n{qmd_tags_yaml}\n---\n\n"
    );

    if !req.markdown_body.trim_start().starts_with("# ") {
        out.push_str(&format!("# {}\n\n", req.title));
    }
    out.push_str(&format!(
        "> **Filed Query:** {}\n> **Summary:** {}\n\n",
        req.query.trim(),
        req.one_line_summary.trim()
    ));
    out.push_str(req.markdown_body.trim());
    out.push('\n');
    out
}
