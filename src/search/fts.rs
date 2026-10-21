use crate::db::filter_sql::resolve_matching_document_ids;
use crate::db::index_store::{get_document_info_by_id, resolve_path_context_for_doc};
use crate::error::WikiResult;
use crate::search::{SearchOptions, SearchResultItem};
use crate::types::CollectionScope;
use rusqlite::{params, Connection};
use std::collections::HashSet;

fn run_fts_query(conn: &Connection, fts_q: &str, limit: i64) -> WikiResult<Vec<(i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT rowid, bm25(documents_fts, 0.0, 0.0, 0.0, 5.0, 3.0, 1.0, 2.0) AS raw_bm25
         FROM documents_fts
         WHERE documents_fts MATCH ?1
         ORDER BY raw_bm25 ASC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts_q, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Perform BM25 Full-Text Search using SQLite FTS5 (`documents_fts`).
pub fn search_fts_bm25(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> WikiResult<Vec<SearchResultItem>> {
    let fts_query = build_safe_fts5_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let allowed_doc_ids: Option<HashSet<i64>> = match &opts.filter {
        Some(f) => Some(resolve_matching_document_ids(conn, f)?),
        None => None,
    };

    let allowed_collections: HashSet<&str> = if opts.collections.is_empty() {
        [
            CollectionScope::Wiki.as_str(),
            CollectionScope::Sources.as_str(),
        ]
        .into_iter()
        .collect()
    } else {
        opts.collections
            .iter()
            .map(CollectionScope::as_str)
            .collect()
    };

    let overfetch_limit = (opts.limit.max(10) * 5).max(50) as i64;

    let rows: Vec<(i64, f64)> = match run_fts_query(conn, &fts_query, overfetch_limit) {
        Ok(list) if !list.is_empty() => list,
        _ => {
            let fallback_q = build_fallback_or_fts5_query(query);
            if fallback_q.is_empty() {
                Vec::new()
            } else {
                run_fts_query(conn, &fallback_q, overfetch_limit).unwrap_or_default()
            }
        }
    };

    let mut results = Vec::new();
    for (doc_id, raw_bm25) in rows {
        if let Some(ref allowed) = allowed_doc_ids {
            if !allowed.contains(&doc_id) {
                continue;
            }
        }

        let info = get_document_info_by_id(conn, doc_id)?;
        if !allowed_collections.contains(info.collection.as_str()) {
            continue;
        }

        let abs_bm25 = raw_bm25.abs() as f32;
        // Normalize BM25 score (0..25+) into (0.0..1.0) using smooth saturation `x / (x + 2.5)`
        let normalized_score = if abs_bm25 <= 0.0 {
            0.0
        } else {
            (abs_bm25 / (abs_bm25 + 2.5)).clamp(0.0, 1.0)
        };

        if normalized_score < opts.min_score {
            continue;
        }

        let raw_body: String = conn.query_row(
            "SELECT raw_body FROM documents WHERE id = ?1",
            params![doc_id],
            |r| r.get(0),
        )?;

        let (snippet, match_line) = extract_best_snippet(&raw_body, query, 3);
        let context = resolve_path_context_for_doc(conn, info.collection, &info.scoped_path)?;
        let virtual_uri = format!("qmd://{}/{}", info.collection.as_str(), info.scoped_path);

        results.push(SearchResultItem {
            docid: info.docid.clone(),
            rel_path: info.rel_path.clone(),
            virtual_uri,
            title: info.title.clone(),
            collection: info.collection,
            score: normalized_score,
            raw_backend_score: abs_bm25,
            match_line,
            snippet,
            context,
            metadata: info.metadata,
            explain: None,
        });

        if results.len() >= opts.limit {
            break;
        }
    }

    Ok(results)
}

/// Sanitize a raw user query string into a valid SQLite FTS5 query string.
/// Preserves `"quoted phrases"`, `-negated` terms, and converts bare words into safe tokens.
pub fn build_safe_fts5_query(raw_query: &str) -> String {
    let mut clauses = Vec::new();
    let mut chars = raw_query.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        // Check for `-` negation prefix
        let is_negated = if ch == '-' {
            chars.next();
            true
        } else {
            false
        };

        if let Some(&'"') = chars.peek() {
            chars.next(); // consume opening quote
            let mut phrase = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                if c.is_alphanumeric() || c.is_whitespace() || c == '_' || c == '-' {
                    phrase.push(c);
                } else {
                    phrase.push(' ');
                }
            }
            let clean_phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
            if !clean_phrase.is_empty() {
                if is_negated {
                    clauses.push(format!("NOT \"{clean_phrase}\""));
                } else {
                    clauses.push(format!("\"{clean_phrase}\""));
                }
            }
        } else {
            let mut token = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                chars.next();
                if c.is_alphanumeric() || c == '_' {
                    token.push(c);
                } else if !token.is_empty() {
                    break;
                }
            }
            let clean_token = token.trim();
            if !clean_token.is_empty() {
                // Ignore pure FTS keywords typed in lowercase/uppercase
                let upper = clean_token.to_ascii_uppercase();
                if matches!(upper.as_str(), "AND" | "OR" | "NOT" | "NEAR") {
                    continue;
                }
                if is_negated {
                    clauses.push(format!("NOT \"{clean_token}\""));
                } else {
                    clauses.push(format!("\"{clean_token}\"*"));
                }
            }
        }
    }

    // Put positive clauses before NOT clauses (FTS5 requires `A NOT B`)
    let (negatives, positives): (Vec<String>, Vec<String>) =
        clauses.into_iter().partition(|c| c.starts_with("NOT "));

    if positives.is_empty() {
        return String::new();
    }

    let mut combined = positives.join(" ");
    for neg in negatives {
        combined.push(' ');
        combined.push_str(&neg);
    }
    combined
}

fn build_fallback_or_fts5_query(raw_query: &str) -> String {
    let tokens: Vec<String> = raw_query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 2)
        .map(|s| format!("\"{s}\"*"))
        .collect();
    tokens.join(" OR ")
}

/// Extract a relevant snippet and 1-indexed line number around matching query terms.
pub fn extract_best_snippet(body: &str, query: &str, context_lines: usize) -> (String, usize) {
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return (String::new(), 1);
    }

    let query_terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let mut best_line_idx = 0usize;
    let mut best_hits = 0usize;

    if !query_terms.is_empty() {
        for (idx, line) in lines.iter().enumerate() {
            let line_lower = line.to_ascii_lowercase();
            let hits = query_terms
                .iter()
                .filter(|t| line_lower.contains(t.as_str()))
                .count();
            if hits > best_hits {
                best_hits = hits;
                best_line_idx = idx;
            }
        }
    }

    let start = best_line_idx.saturating_sub(context_lines / 2);
    let end = (start + context_lines.max(1)).min(lines.len());
    let snippet = lines[start..end].join("\n").trim().to_string();
    (snippet, best_line_idx + 1)
}
