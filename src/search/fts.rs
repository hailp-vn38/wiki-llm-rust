use crate::db::filter_sql::resolve_matching_document_ids;
use crate::db::index_store::{
    get_document_info_by_id, resolve_path_context_for_doc, resolve_wiki_links_for_doc,
};
use crate::error::WikiResult;
use crate::fs::file_ops::fold_vietnamese;
use crate::search::{SearchOptions, SearchResultItem};
use crate::types::{CollectionScope, DocumentInfo};
use rusqlite::{params, Connection};
use std::collections::{BTreeSet, HashSet};
use unicode_normalization::UnicodeNormalization;

const SYSTEM_CATALOG_SCORE_DAMPING: f32 = 0.60;
/// Shortest word that may match as a prefix.
const MIN_PREFIX_CHARS: usize = 4;
/// Share of the query's words a document must contain to count as a match.
const MIN_TERM_COVERAGE: f32 = 0.6;

fn normalize_query(text: &str) -> String {
    text.nfc().collect()
}

/// Fold a string to lowercase with Vietnamese diacritics converted to ASCII (`Đ/đ -> d`, `ă/â -> a`, etc.).
pub fn fold_vietnamese_str(s: &str) -> String {
    s.chars()
        .map(fold_vietnamese)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Check if `rel_path` is an auto-generated system catalog/log (`wiki/index.md` or `wiki/log.md`)
/// and the user's query is not explicitly asking for the index or log.
pub fn is_system_catalog_doc(rel_path: &str, query: &str) -> bool {
    if rel_path != "wiki/index.md" && rel_path != "wiki/log.md" {
        return false;
    }
    let q_lower = query.to_ascii_lowercase();
    !q_lower.contains("index") && !q_lower.contains("log") && !q_lower.contains("master")
}

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

/// A positive query word used to decide whether a candidate document really answers the query.
struct CoverageTerm {
    text: String,
    accented: bool,
    allow_prefix: bool,
}

/// Lowercased words of a document, kept both as written and with Vietnamese diacritics folded.
struct DocumentTerms {
    exact: BTreeSet<String>,
    folded: BTreeSet<String>,
}

fn split_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

fn coverage_terms(query: &str) -> Vec<CoverageTerm> {
    let mut terms = Vec::new();
    for clause in parse_query_clauses(query) {
        if clause.negated {
            continue;
        }
        for word in split_words(&clause.text) {
            let text = word.to_lowercase();
            let folded = fold_vietnamese_str(&text);
            let accented = folded != text;
            terms.push(CoverageTerm {
                text: if accented { text } else { folded },
                accented,
                allow_prefix: word.chars().count() >= MIN_PREFIX_CHARS,
            });
        }
    }
    terms
}

fn document_terms(conn: &Connection, doc_id: i64) -> WikiResult<DocumentTerms> {
    let text: String = conn.query_row(
        "SELECT title || ' ' || headings || ' ' || body || ' ' || tags
         FROM documents_fts WHERE rowid = ?1",
        params![doc_id],
        |r| r.get(0),
    )?;
    let normalized = normalize_query(&text).to_lowercase();
    let exact: BTreeSet<String> = split_words(&normalized).map(str::to_owned).collect();
    let folded = exact.iter().map(|w| fold_vietnamese_str(w)).collect();
    Ok(DocumentTerms { exact, folded })
}

fn term_is_covered(term: &CoverageTerm, doc: &DocumentTerms) -> bool {
    // An accented query word means what it says (`khí` is not `khi`); an unaccented one may be
    // a user typing without diacritics, so it matches the folded form.
    let words = if term.accented {
        &doc.exact
    } else {
        &doc.folded
    };
    if term.allow_prefix {
        words
            .range(term.text.clone()..)
            .next()
            .is_some_and(|w| w.starts_with(&term.text))
    } else {
        words.contains(&term.text)
    }
}

fn meets_term_coverage(terms: &[CoverageTerm], doc: &DocumentTerms) -> bool {
    if terms.is_empty() {
        return true;
    }
    let covered = terms.iter().filter(|t| term_is_covered(t, doc)).count();
    covered as f32 / terms.len() as f32 >= MIN_TERM_COVERAGE
}

/// Perform BM25 Full-Text Search using SQLite FTS5 (`documents_fts`).
///
/// Candidates come from the strict AND query, or the OR query when AND finds nothing in scope.
/// Either way a candidate must contain at least [`MIN_TERM_COVERAGE`] of the query's words.
pub fn search_fts_bm25(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> WikiResult<Vec<SearchResultItem>> {
    let query = normalize_query(query);
    let query = query.as_str();
    let fts_query = build_safe_fts5_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let allowed_doc_ids: Option<HashSet<i64>> = match &opts.filter {
        Some(f) => Some(resolve_matching_document_ids(conn, f)?),
        None => None,
    };

    let allowed_collections: HashSet<&str> = if opts.collections.is_empty() {
        [CollectionScope::Wiki.as_str()].into_iter().collect()
    } else {
        opts.collections
            .iter()
            .map(CollectionScope::as_str)
            .collect()
    };

    let overfetch_limit = (opts.limit.max(10) * 5).max(50) as i64;
    let terms = coverage_terms(query);

    let in_scope = |fts_q: &str| -> WikiResult<Vec<(i64, f64, DocumentInfo)>> {
        let mut kept = Vec::new();
        for (doc_id, raw_bm25) in run_fts_query(conn, fts_q, overfetch_limit).unwrap_or_default() {
            if allowed_doc_ids
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&doc_id))
            {
                continue;
            }
            let info = get_document_info_by_id(conn, doc_id)?;
            if !allowed_collections.contains(info.collection.as_str()) {
                continue;
            }
            if meets_term_coverage(&terms, &document_terms(conn, doc_id)?) {
                kept.push((doc_id, raw_bm25, info));
            }
        }
        Ok(kept)
    };

    let mut rows = in_scope(&fts_query)?;
    if rows.is_empty() {
        let fallback_q = build_fallback_or_fts5_query(query);
        if !fallback_q.is_empty() {
            rows = in_scope(&fallback_q)?;
        }
    }

    let mut results = Vec::new();
    for (doc_id, raw_bm25, info) in rows {
        let abs_bm25 = raw_bm25.abs() as f32;
        // Normalize BM25 score (0..25+) into (0.0..1.0) using smooth saturation `x / (x + 2.5)`
        let mut normalized_score = if abs_bm25 <= 0.0 {
            0.0
        } else {
            (abs_bm25 / (abs_bm25 + 2.5)).clamp(0.0, 1.0)
        };
        if is_system_catalog_doc(&info.rel_path, query) {
            normalized_score *= SYSTEM_CATALOG_SCORE_DAMPING;
        }

        if normalized_score < opts.min_score {
            continue;
        }

        let (raw_body, line_count): (String, i64) = conn.query_row(
            "SELECT raw_body, line_count FROM documents WHERE id = ?1",
            params![doc_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let frontmatter_offset =
            (line_count.max(0) as usize).saturating_sub(raw_body.lines().count());
        let (snippet, rel_line) = extract_best_snippet(&raw_body, query, 3);
        let match_line = frontmatter_offset + rel_line;
        let context = resolve_path_context_for_doc(conn, info.collection, &info.scoped_path)?;
        let (outgoing_links, backlinks, unresolved_links) =
            resolve_wiki_links_for_doc(conn, doc_id, &info.rel_path)?;
        let is_wiki_linked = !outgoing_links.is_empty() || !backlinks.is_empty();
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
            is_wiki_linked,
            outgoing_links,
            backlinks,
            unresolved_links,
            explain: None,
        });
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    results.truncate(opts.limit);

    Ok(results)
}

fn expand_vietnamese_d_variants(token: &str) -> (String, Option<String>) {
    let ascii_d = token.replace('đ', "d").replace('Đ', "D");
    let viet_d = ascii_d.replace('d', "đ").replace('D', "Đ");
    if ascii_d != viet_d {
        (ascii_d, Some(viet_d))
    } else {
        (ascii_d, None)
    }
}

struct QueryClause {
    text: String,
    is_phrase: bool,
    negated: bool,
}

/// Split a raw user query into `"quoted phrases"`, `-negated` terms, and bare words.
fn parse_query_clauses(raw_query: &str) -> Vec<QueryClause> {
    let mut clauses = Vec::new();
    let mut chars = raw_query.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        // Check for `-` negation prefix
        let negated = if ch == '-' {
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
                clauses.push(QueryClause {
                    text: clean_phrase,
                    is_phrase: true,
                    negated,
                });
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
                clauses.push(QueryClause {
                    text: clean_token.to_string(),
                    is_phrase: false,
                    negated,
                });
            }
        }
    }
    clauses
}

/// Render one bare word as an FTS5 clause. Short Vietnamese syllables stay exact because a prefix
/// would pull in unrelated words (`ma*` matches `manifest`, `bac*` matches `backend`).
fn word_clause(token: &str) -> String {
    let star = if token.chars().count() >= MIN_PREFIX_CHARS {
        "*"
    } else {
        ""
    };
    let (ascii_d, viet_d) = expand_vietnamese_d_variants(token);
    match viet_d {
        Some(viet_d) => format!("(\"{ascii_d}\"{star} OR \"{viet_d}\"{star})"),
        None => format!("\"{token}\"{star}"),
    }
}

fn positive_clause(clause: &QueryClause) -> String {
    if clause.is_phrase {
        format!("\"{}\"", clause.text)
    } else {
        word_clause(&clause.text)
    }
}

/// Sanitize a raw user query string into a valid SQLite FTS5 query string.
/// Preserves `"quoted phrases"`, `-negated` terms, and converts bare words into safe tokens.
pub fn build_safe_fts5_query(raw_query: &str) -> String {
    let (negatives, positives): (Vec<_>, Vec<_>) = parse_query_clauses(raw_query)
        .into_iter()
        .partition(|c| c.negated);

    if positives.is_empty() {
        return String::new();
    }

    // Put positive clauses before NOT clauses (FTS5 requires `A NOT B`)
    let mut combined = positives
        .iter()
        .map(positive_clause)
        .collect::<Vec<_>>()
        .join(" ");
    for neg in negatives {
        combined.push_str(&format!(" NOT \"{}\"", neg.text));
    }
    combined
}

fn build_fallback_or_fts5_query(raw_query: &str) -> String {
    let (negatives, positives): (Vec<_>, Vec<_>) = parse_query_clauses(raw_query)
        .into_iter()
        .partition(|c| c.negated);

    if positives.is_empty() {
        return String::new();
    }

    let mut combined = format!(
        "({})",
        positives
            .iter()
            .map(positive_clause)
            .collect::<Vec<_>>()
            .join(" OR ")
    );
    for neg in negatives {
        combined.push_str(&format!(" NOT \"{}\"", neg.text));
    }
    combined
}

/// Extract a relevant snippet and 1-indexed line number around matching query terms.
pub fn extract_best_snippet(body: &str, query: &str, context_lines: usize) -> (String, usize) {
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return (String::new(), 1);
    }

    let exact_terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.chars().count() >= 2)
        .map(|s| s.to_lowercase())
        .collect();
    let folded_terms: Vec<String> = exact_terms.iter().map(|s| fold_vietnamese_str(s)).collect();

    let mut best_line_idx = 0usize;
    let mut best_score = 0usize;

    if !folded_terms.is_empty() {
        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let line_lower = line.to_lowercase();
            let line_folded = fold_vietnamese_str(line);

            let folded_hits = folded_terms
                .iter()
                .filter(|t| line_folded.contains(t.as_str()))
                .count();
            if folded_hits == 0 {
                continue;
            }
            let exact_hits = exact_terms
                .iter()
                .filter(|t| line_lower.contains(t.as_str()))
                .count();
            let is_reference_only = trimmed.starts_with("> **Raw Sources:**")
                || trimmed.starts_with("- **Source")
                || trimmed.starts_with("- Raw source:");
            let prose_bonus = usize::from(!is_reference_only);
            let score = folded_hits * 4 + exact_hits * 2 + prose_bonus;

            if score > best_score {
                best_score = score;
                best_line_idx = idx;
            }
        }
    }

    let start = best_line_idx.saturating_sub(context_lines / 2);
    let end = (start + context_lines.max(1)).min(lines.len());
    let snippet = lines[start..end].join("\n").trim().to_string();
    (snippet, best_line_idx + 1)
}
