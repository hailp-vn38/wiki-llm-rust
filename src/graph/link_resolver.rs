use crate::db::index_store::get_document_info_by_id;
use crate::error::WikiResult;
use crate::types::DocumentInfo;
use rusqlite::{params, Connection};
use std::path::Path;

#[derive(Debug, Clone)]
struct DocCandidate {
    id: i64,
    collection: String,
    rel_path: String,
    scoped_path: String,
    file_name_lower: String,
    file_stem_lower: String,
    aliases_lower: Vec<String>,
    parent_dir: String,
}

/// Resolve all entries in `graph_links` against the current `documents` table using Obsidian's
/// link resolution rules.
pub fn resolve_all_links_in_db(conn: &Connection) -> WikiResult<usize> {
    let candidates = load_all_doc_candidates(conn)?;
    let mut stmt = conn.prepare("SELECT id, source_rel_path, target_path_spec FROM graph_links")?;
    let links: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut resolved_count = 0usize;
    for (link_id, source_rel_path, target_spec) in links {
        if let Some(matched) =
            resolve_target_against_candidates(&target_spec, Some(&source_rel_path), &candidates)
        {
            conn.execute(
                "UPDATE graph_links SET resolved_doc_id = ?1, resolved_rel_path = ?2 WHERE id = ?3",
                params![matched.id, matched.rel_path, link_id],
            )?;
            resolved_count += 1;
        } else {
            conn.execute(
                "UPDATE graph_links SET resolved_doc_id = NULL, resolved_rel_path = NULL WHERE id = ?1",
                params![link_id],
            )?;
        }
    }

    Ok(resolved_count)
}

/// Resolve a single `[[WikiLink]]` target string (optionally from the context of `from_rel_path`)
/// to its matching `DocumentInfo` if it exists in the vault.
pub fn resolve_single_wikilink(
    conn: &Connection,
    wikilink_target: &str,
    from_rel_path: Option<&str>,
) -> WikiResult<Option<DocumentInfo>> {
    let clean = wikilink_target
        .trim()
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(wikilink_target.trim());
    let before_alias = clean.split('|').next().unwrap_or(clean);
    let target_spec = before_alias
        .split('#')
        .next()
        .unwrap_or(before_alias)
        .trim();
    if target_spec.is_empty() {
        return Ok(None);
    }

    let candidates = load_all_doc_candidates(conn)?;
    if let Some(matched) =
        resolve_target_against_candidates(target_spec, from_rel_path, &candidates)
    {
        let info = get_document_info_by_id(conn, matched.id)?;
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

fn load_all_doc_candidates(conn: &Connection) -> WikiResult<Vec<DocCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection, rel_path, scoped_path, file_name, file_stem, aliases_json
         FROM documents",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let collection: String = row.get(1)?;
        let rel_path: String = row.get(2)?;
        let scoped_path: String = row.get(3)?;
        let file_name: String = row.get(4)?;
        let file_stem: String = row.get(5)?;
        let aliases_json: String = row.get(6)?;
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
        let parent_dir = Path::new(&rel_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .replace('\\', "/");

        Ok(DocCandidate {
            id,
            collection,
            rel_path,
            scoped_path,
            file_name_lower: file_name.to_ascii_lowercase(),
            file_stem_lower: file_stem.to_ascii_lowercase(),
            aliases_lower: aliases
                .into_iter()
                .map(|a| a.to_ascii_lowercase())
                .collect(),
            parent_dir,
        })
    })?;

    let mut list = Vec::new();
    for r in rows {
        list.push(r?);
    }
    Ok(list)
}

fn resolve_target_against_candidates<'a>(
    raw_spec: &str,
    from_rel_path: Option<&str>,
    candidates: &'a [DocCandidate],
) -> Option<&'a DocCandidate> {
    let spec = raw_spec.trim().replace('\\', "/");
    let spec = spec.trim_start_matches("./").trim_start_matches('/');
    if spec.is_empty() {
        return None;
    }

    let spec_lower = spec.to_ascii_lowercase();
    let spec_with_md = if spec_lower.ends_with(".md") {
        spec_lower.clone()
    } else {
        format!("{spec_lower}.md")
    };

    let source_parent = from_rel_path
        .and_then(|p| Path::new(p).parent())
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('\\', "/");

    // 1. If target contains `/`, resolve by path match (`rel_path`, `scoped_path`, or path suffix)
    if spec.contains('/') {
        let mut path_matches: Vec<&DocCandidate> = candidates
            .iter()
            .filter(|c| {
                let rel_l = c.rel_path.to_ascii_lowercase();
                let scoped_l = c.scoped_path.to_ascii_lowercase();
                rel_l == spec_lower
                    || rel_l == spec_with_md
                    || scoped_l == spec_lower
                    || scoped_l == spec_with_md
                    || rel_l.ends_with(&format!("/{spec_with_md}"))
                    || scoped_l.ends_with(&format!("/{spec_with_md}"))
            })
            .collect();

        if !path_matches.is_empty() {
            path_matches.sort_by(|a, b| {
                // Exact `rel_path` match first, then `wiki` collection priority, then shortest path
                let a_exact = a.rel_path.eq_ignore_ascii_case(&spec_with_md);
                let b_exact = b.rel_path.eq_ignore_ascii_case(&spec_with_md);
                b_exact
                    .cmp(&a_exact)
                    .then_with(|| (a.collection != "wiki").cmp(&(b.collection != "wiki")))
                    .then_with(|| a.rel_path.len().cmp(&b.rel_path.len()))
            });
            return path_matches.first().copied();
        }
    }

    // 2. Match by `file_stem`, `file_name`, or `aliases` (Obsidian short wikilink `[[name]]`)
    let stem_query = spec_lower.strip_suffix(".md").unwrap_or(&spec_lower);
    let mut name_matches: Vec<&DocCandidate> = candidates
        .iter()
        .filter(|c| {
            c.file_stem_lower == stem_query
                || c.file_name_lower == spec_lower
                || c.aliases_lower
                    .iter()
                    .any(|a| a == stem_query || a == &spec_lower)
        })
        .collect();

    if name_matches.is_empty() {
        return None;
    }

    name_matches.sort_by(|a, b| {
        let a_same_dir = !source_parent.is_empty() && a.parent_dir == source_parent;
        let b_same_dir = !source_parent.is_empty() && b.parent_dir == source_parent;
        let a_exact_stem = a.file_stem_lower == stem_query;
        let b_exact_stem = b.file_stem_lower == stem_query;

        b_same_dir
            .cmp(&a_same_dir)
            .then_with(|| b_exact_stem.cmp(&a_exact_stem))
            .then_with(|| (a.collection != "wiki").cmp(&(b.collection != "wiki")))
            .then_with(|| a.rel_path.len().cmp(&b.rel_path.len()))
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });

    name_matches.first().copied()
}
