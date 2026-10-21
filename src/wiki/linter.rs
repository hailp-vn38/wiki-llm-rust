use crate::config::WikiConfig;
use crate::error::WikiResult;
use crate::wiki::ingest::map_source_to_wiki_scoped_path;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Health-check report produced by `WikiEngine::lint_wiki` (Karpathy's Lint operation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLintReport {
    /// Raw sources in `sources/` that do not have a corresponding summary or link in `wiki/` yet.
    pub uningested_sources: Vec<UningestedSourceItem>,
    /// Dangling/broken `[[WikiLinks]]` whose target page (or local asset) does not exist yet.
    pub broken_links: Vec<BrokenLinkItem>,
    /// Wiki pages (excluding `index.md` and `log.md`) that have zero inbound links (`[[...]]`).
    pub orphan_wiki_pages: Vec<String>,
    /// Mentions of existing entity/concept titles in wiki pages that are not wrapped in `[[WikiLinks]]`.
    pub unlinked_mentions: Vec<UnlinkedMentionItem>,
    /// Wiki pages whose underlying raw source in `sources/` was modified more recently than the wiki page (`stale claims`).
    pub stale_wiki_pages: Vec<StaleWikiPageItem>,
    /// Explicit contradiction or data-gap markers noted in wiki pages (`CONTRADICTION:`, `[!WARNING]`, `TODO:`, `GAP:`).
    pub flagged_contradictions_or_gaps: Vec<FlaggedClaimItem>,
    /// Wiki pages that lack YAML frontmatter.
    pub missing_frontmatter_pages: Vec<String>,
    /// Summary status: `true` if there are no broken links, orphans, stale pages, or uningested sources.
    pub is_healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleWikiPageItem {
    pub wiki_rel_path: String,
    pub source_rel_path: String,
    pub source_mtime_ms: i64,
    pub wiki_mtime_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedClaimItem {
    pub wiki_rel_path: String,
    pub line_number: usize,
    pub marker_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UningestedSourceItem {
    pub source_rel_path: String,
    pub expected_wiki_rel_path: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenLinkItem {
    pub source_rel_path: String,
    pub raw_target: String,
    pub target_path_spec: String,
    pub line_number: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnlinkedMentionItem {
    pub page_rel_path: String,
    pub target_entity_or_concept: String,
    pub target_rel_path: String,
}

/// Run a full health-check across `sources/` and `wiki/`.
pub fn run_wiki_lint(conn: &Connection, config: &WikiConfig) -> WikiResult<WikiLintReport> {
    // 1. Collect all indexed wiki `rel_path`s and `source_rel_path`s
    let mut wiki_stmt = conn.prepare(
        "SELECT rel_path, scoped_path, source_rel_path, frontmatter_json, raw_body, category, file_stem, title, mtime_ms
         FROM documents
         WHERE collection = 'wiki'
         ORDER BY rel_path ASC",
    )?;

    struct WikiDocRow {
        rel_path: String,
        scoped_path: String,
        source_rel_path: Option<String>,
        has_frontmatter: bool,
        raw_body: String,
        category: String,
        file_stem: String,
        title: String,
        mtime_ms: i64,
    }

    let wiki_docs: Vec<WikiDocRow> = wiki_stmt
        .query_map([], |r| {
            let fm: Option<String> = r.get(3)?;
            let cat: Option<String> = r.get(5)?;
            Ok(WikiDocRow {
                rel_path: r.get(0)?,
                scoped_path: r.get(1)?,
                source_rel_path: r.get(2)?,
                has_frontmatter: fm.is_some(),
                raw_body: r.get(4)?,
                category: cat.unwrap_or_default(),
                file_stem: r.get(6)?,
                title: r.get(7)?,
                mtime_ms: r.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let wiki_rel_set: HashSet<String> = wiki_docs.iter().map(|d| d.rel_path.clone()).collect();
    let mut covered_sources: HashSet<String> = wiki_docs
        .iter()
        .filter_map(|d| d.source_rel_path.clone())
        .collect();

    // Also mark any raw source that is linked from any non-system wiki page (e.g. via `sources:` frontmatter
    // or inline `[[sources/...]]` when updating an existing wiki page without creating a new file) as covered!
    let mut linked_sources_stmt = conn.prepare(
        "SELECT DISTINCT gl.resolved_rel_path
         FROM graph_links gl
         JOIN documents src ON src.id = gl.source_doc_id
         WHERE src.collection = 'wiki'
           AND COALESCE(src.category, '') != 'system'
           AND gl.resolved_rel_path IS NOT NULL",
    )?;
    let linked_from_wiki: Vec<String> = linked_sources_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    for p in linked_from_wiki {
        covered_sources.insert(p);
    }

    // 2. Check for un-ingested sources in `sources/` and build a lookup of source `mtime_ms`
    let mut src_stmt = conn.prepare(
        "SELECT rel_path, scoped_path, file_name, mtime_ms FROM documents WHERE collection = 'sources' ORDER BY rel_path ASC",
    )?;
    let src_rows: Vec<(String, String, String, i64)> = src_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut source_mtime_map = std::collections::HashMap::new();
    let mut uningested_sources = Vec::new();
    for (src_rel, src_scoped, file_name, src_mtime) in src_rows {
        source_mtime_map.insert(src_rel.clone(), src_mtime);
        let (expected_wiki_scoped, _) = map_source_to_wiki_scoped_path(&src_scoped, config)?;
        let expected_wiki_rel = format!("{}/{expected_wiki_scoped}", config.wiki_dir_name);
        if !covered_sources.contains(&src_rel) && !wiki_rel_set.contains(&expected_wiki_rel) {
            uningested_sources.push(UningestedSourceItem {
                source_rel_path: src_rel,
                expected_wiki_rel_path: expected_wiki_rel,
                file_name,
            });
        }
    }

    // 3. Check for broken/unresolved links (`resolved_doc_id IS NULL`), ignoring valid local asset files on disk
    let mut broken_stmt = conn.prepare(
        "SELECT source_rel_path, raw_target, target_path_spec, line_number
         FROM graph_links
         WHERE resolved_doc_id IS NULL
         ORDER BY source_rel_path ASC, line_number ASC",
    )?;
    let raw_broken: Vec<BrokenLinkItem> = broken_stmt
        .query_map([], |r| {
            Ok(BrokenLinkItem {
                source_rel_path: r.get(0)?,
                raw_target: r.get(1)?,
                target_path_spec: r.get(2)?,
                line_number: r.get::<_, i64>(3)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let broken_links: Vec<BrokenLinkItem> = raw_broken
        .into_iter()
        .filter(|item| !asset_exists_on_disk(config, &item.target_path_spec))
        .collect();

    // 4. Check for orphan wiki pages (no inbound links from non-system pages)
    let mut inbound_stmt = conn.prepare(
        "SELECT DISTINCT gl.resolved_rel_path
         FROM graph_links gl
         JOIN documents src ON src.id = gl.source_doc_id
         WHERE gl.resolved_rel_path IS NOT NULL
           AND gl.resolved_rel_path != gl.source_rel_path
           AND COALESCE(src.category, '') != 'system'",
    )?;
    let linked_targets: HashSet<String> = inbound_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut outbound_stmt = conn.prepare(
        "SELECT DISTINCT source_rel_path
         FROM graph_links
         WHERE resolved_rel_path IS NOT NULL AND resolved_rel_path != source_rel_path",
    )?;
    let pages_with_outbound: HashSet<String> = outbound_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut orphan_wiki_pages = Vec::new();
    let mut missing_frontmatter_pages = Vec::new();

    for doc in &wiki_docs {
        if doc.scoped_path == "index.md" || doc.scoped_path == "log.md" || doc.category == "system"
        {
            continue;
        }
        let has_inbound = linked_targets.contains(&doc.rel_path);
        let has_outbound = pages_with_outbound.contains(&doc.rel_path);

        let is_orphan = if doc.category == "source_summary" {
            // A source summary is an entry-point for a raw source; it is only orphaned if it neither
            // links to any wiki page nor is linked from any non-system page.
            !has_inbound && !has_outbound
        } else {
            // Entities, concepts, syntheses, and general wiki pages should have at least one inbound link
            // from another substantive page.
            !has_inbound
        };

        if is_orphan {
            orphan_wiki_pages.push(doc.rel_path.clone());
        }
        if !doc.has_frontmatter {
            missing_frontmatter_pages.push(doc.rel_path.clone());
        }
    }

    // 5. Check for unlinked mentions of Entity/Concept pages inside other wiki pages
    let entities_and_concepts: Vec<&WikiDocRow> = wiki_docs
        .iter()
        .filter(|d| d.category == "entity" || d.category == "concept")
        .collect();

    let mut outgoing_map_stmt =
        conn.prepare("SELECT source_rel_path, resolved_rel_path FROM graph_links WHERE resolved_rel_path IS NOT NULL")?;
    let existing_pairs: HashSet<(String, String)> = outgoing_map_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut unlinked_mentions = Vec::new();
    for doc in &wiki_docs {
        if doc.scoped_path == "index.md" || doc.scoped_path == "log.md" {
            continue;
        }
        let body_lower = doc.raw_body.to_ascii_lowercase();
        for target in &entities_and_concepts {
            if target.rel_path == doc.rel_path {
                continue;
            }
            if existing_pairs.contains(&(doc.rel_path.clone(), target.rel_path.clone())) {
                continue;
            }
            let stem_lower = target.file_stem.to_ascii_lowercase();
            let title_lower = target.title.to_ascii_lowercase();
            if (stem_lower.len() >= 3 && contains_whole_word(&body_lower, &stem_lower))
                || (title_lower.len() >= 3 && contains_whole_word(&body_lower, &title_lower))
            {
                unlinked_mentions.push(UnlinkedMentionItem {
                    page_rel_path: doc.rel_path.clone(),
                    target_entity_or_concept: target.file_stem.clone(),
                    target_rel_path: target.rel_path.clone(),
                });
            }
        }
    }

    // 6. Check for Stale Wiki Pages (where the underlying raw source was updated after the wiki page)
    // and Flagged Contradictions / Data Gaps
    let mut stale_wiki_pages = Vec::new();
    let mut flagged_contradictions_or_gaps = Vec::new();

    for doc in &wiki_docs {
        if doc.scoped_path == "index.md" || doc.scoped_path == "log.md" || doc.category == "system"
        {
            continue;
        }
        if let Some(ref src_rel) = doc.source_rel_path {
            if let Some(&src_mtime) = source_mtime_map.get(src_rel) {
                // Allow 100ms tolerance for rapid sequential writes in tests
                if src_mtime > doc.mtime_ms + 100 {
                    stale_wiki_pages.push(StaleWikiPageItem {
                        wiki_rel_path: doc.rel_path.clone(),
                        source_rel_path: src_rel.clone(),
                        source_mtime_ms: src_mtime,
                        wiki_mtime_ms: doc.mtime_ms,
                    });
                }
            }
        }

        for (idx, line) in doc.raw_body.lines().enumerate() {
            let upper = line.to_ascii_uppercase();
            if upper.contains("CONTRADICTION:")
                || upper.contains("[!WARNING]")
                || upper.contains("[!CAUTION]")
                || upper.contains("DATA GAP:")
                || upper.contains("TODO:")
            {
                flagged_contradictions_or_gaps.push(FlaggedClaimItem {
                    wiki_rel_path: doc.rel_path.clone(),
                    line_number: idx + 1,
                    marker_text: line.trim().to_string(),
                });
            }
        }
    }

    let is_healthy = uningested_sources.is_empty()
        && broken_links.is_empty()
        && orphan_wiki_pages.is_empty()
        && stale_wiki_pages.is_empty();

    Ok(WikiLintReport {
        uningested_sources,
        broken_links,
        orphan_wiki_pages,
        unlinked_mentions,
        stale_wiki_pages,
        flagged_contradictions_or_gaps,
        missing_frontmatter_pages,
        is_healthy,
    })
}

fn asset_exists_on_disk(config: &WikiConfig, target_spec: &str) -> bool {
    let clean = target_spec.trim().trim_start_matches('/');
    if clean.is_empty() || clean.contains("..") {
        return false;
    }
    config.data_root.join(clean).is_file()
        || config.sources_dir().join(clean).is_file()
        || config.sources_dir().join("assets").join(clean).is_file()
        || config.wiki_dir().join(clean).is_file()
}

fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    for (idx, _) in haystack.match_indices(needle) {
        let before_ok = idx == 0
            || !haystack[..idx]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric() || c == '_')
                .unwrap_or(false);
        let after_idx = idx + needle.len();
        let after_ok = after_idx >= haystack.len()
            || !haystack[after_idx..]
                .chars()
                .next()
                .map(|c| c.is_alphanumeric() || c == '_')
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}
