use crate::config::WikiConfig;
use crate::error::{WikiError, WikiResult};
use crate::fs::file_ops::{slice_and_format_lines, slugify};
use crate::fs::path_guard::{
    compute_content_hash, compute_docid, parse_target_with_line_range, sanitize_rel_path,
};
use crate::fs::tree::TreeDocMeta;
use crate::graph::link_resolver::resolve_all_links_in_db;
use crate::markdown::chunker::smart_chunk_markdown;
use crate::markdown::frontmatter::{parse_frontmatter, TypedMetaValue};
use crate::markdown::parser::parse_markdown_document;
use crate::types::{
    CollectionScope, DocumentInfo, DocumentResult, GetOptions, MultiGetError, MultiGetOptions,
    MultiGetResult, PathContextItem, SyncStats,
};
use chrono::Utc;
use globset::Glob;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// Index or update a single Markdown file into SQLite (`documents`, `documents_fts`, `content_chunks`, `document_metadata`, `graph_links`).
pub fn index_single_file(
    conn: &Connection,
    config: &WikiConfig,
    collection: CollectionScope,
    scoped_rel_path: &str,
    full_path: &Path,
    resolve_links_immediately: bool,
) -> WikiResult<(DocumentInfo, bool)> {
    let clean_scoped = sanitize_rel_path(scoped_rel_path)?;
    let collection_dir_name = match collection {
        CollectionScope::Sources => &config.sources_dir_name,
        CollectionScope::Wiki => &config.wiki_dir_name,
    };
    let vault_rel_path = format!("{collection_dir_name}/{clean_scoped}");
    let docid = compute_docid(&vault_rel_path);

    let raw_content = std::fs::read_to_string(full_path)?;
    let fs_meta = std::fs::metadata(full_path)?;
    let byte_size = fs_meta.len();
    let mtime_ms = fs_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let content_hash = compute_content_hash(&raw_content);

    // Check if document already exists with identical content_hash
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, content_hash FROM documents WHERE rel_path = ?1",
            params![vault_rel_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let file_name = Path::new(&clean_scoped)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&clean_scoped)
        .to_string();
    let file_stem = Path::new(&clean_scoped)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file_name)
        .to_string();

    let parsed = parse_markdown_document(&raw_content, &file_stem);
    let headings_joined = parsed
        .headings
        .iter()
        .map(|h| h.text.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    let tags_joined = parsed.tags.join(" ");
    let tags_json = serde_json::to_string(&parsed.tags)?;
    let aliases_json = serde_json::to_string(&parsed.frontmatter.aliases)?;

    let mut metadata_json_map = BTreeMap::new();
    for (k, v) in &parsed.frontmatter.qmd_metadata {
        metadata_json_map.insert(k.clone(), v.to_json_value());
    }
    let metadata_json = serde_json::to_string(&metadata_json_map)?;
    let frontmatter_json = match &parsed.frontmatter.json_map {
        Some(m) => Some(serde_json::to_string(m)?),
        None => None,
    };

    // Infer category if in `wiki/` and not explicitly set in frontmatter
    let category = parsed.frontmatter.category.clone().or_else(|| {
        if collection == CollectionScope::Wiki {
            if clean_scoped == "index.md" || clean_scoped == "log.md" {
                Some("system".to_string())
            } else if clean_scoped.starts_with("sources/") {
                Some("source_summary".to_string())
            } else if clean_scoped.starts_with("entities/") {
                Some("entity".to_string())
            } else if clean_scoped.starts_with("concepts/") {
                Some("concept".to_string())
            } else if clean_scoped.starts_with("syntheses/") {
                Some("synthesis".to_string())
            } else {
                Some("wiki_page".to_string())
            }
        } else {
            Some("raw_source".to_string())
        }
    });

    // Infer `source_rel_path` if this is a wiki source summary
    let source_rel_path = parsed
        .frontmatter
        .source_file
        .as_deref()
        .map(extract_clean_path_from_wikilink)
        .or_else(|| {
            if collection == CollectionScope::Wiki {
                clean_scoped
                    .strip_prefix("sources/")
                    .map(|stripped| format!("{}/{stripped}", config.sources_dir_name))
            } else {
                None
            }
        });

    let now_str = Utc::now().to_rfc3339();
    let is_new = existing.is_none();

    let doc_id = if let Some((id, old_hash)) = existing {
        if old_hash == content_hash {
            // Unchanged content: update mtime_ms and return existing info
            conn.execute(
                "UPDATE documents SET mtime_ms = ?1 WHERE id = ?2",
                params![mtime_ms, id],
            )?;
            let info = get_document_info_by_id(conn, id)?;
            return Ok((info, false));
        }

        conn.execute(
            "UPDATE documents SET
                docid = ?1,
                collection = ?2,
                scoped_path = ?3,
                file_name = ?4,
                file_stem = ?5,
                title = ?6,
                category = ?7,
                source_rel_path = ?8,
                content_hash = ?9,
                raw_body = ?10,
                frontmatter_json = ?11,
                tags_json = ?12,
                aliases_json = ?13,
                metadata_json = ?14,
                byte_size = ?15,
                line_count = ?16,
                mtime_ms = ?17,
                indexed_at = ?18,
                needs_embedding = 1
             WHERE id = ?19",
            params![
                docid,
                collection.as_str(),
                clean_scoped,
                file_name,
                file_stem,
                parsed.title,
                category,
                source_rel_path,
                content_hash,
                parsed.frontmatter.body,
                frontmatter_json,
                tags_json,
                aliases_json,
                metadata_json,
                byte_size as i64,
                parsed.line_count as i64,
                mtime_ms,
                now_str,
                id
            ],
        )?;

        // Clean old children before re-inserting
        conn.execute("DELETE FROM documents_fts WHERE rowid = ?1", params![id])?;
        conn.execute(
            "DELETE FROM content_chunks WHERE document_id = ?1",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM document_metadata WHERE document_id = ?1",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM graph_links WHERE source_doc_id = ?1",
            params![id],
        )?;
        id
    } else {
        conn.execute(
            "INSERT INTO documents (
                docid, collection, rel_path, scoped_path, file_name, file_stem,
                title, category, source_rel_path, content_hash, raw_body,
                frontmatter_json, tags_json, aliases_json, metadata_json,
                byte_size, line_count, mtime_ms, indexed_at, needs_embedding
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, 1
             )",
            params![
                docid,
                collection.as_str(),
                vault_rel_path,
                clean_scoped,
                file_name,
                file_stem,
                parsed.title,
                category,
                source_rel_path,
                content_hash,
                parsed.frontmatter.body,
                frontmatter_json,
                tags_json,
                aliases_json,
                metadata_json,
                byte_size as i64,
                parsed.line_count as i64,
                mtime_ms,
                now_str,
            ],
        )?;
        conn.last_insert_rowid()
    };

    // Insert into FTS5 index (`rowid` = `doc_id`)
    conn.execute(
        "INSERT INTO documents_fts (rowid, docid, rel_path, collection, title, headings, body, tags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            doc_id,
            docid,
            vault_rel_path,
            collection.as_str(),
            parsed.title,
            headings_joined,
            parsed.frontmatter.body,
            tags_joined,
        ],
    )?;

    // Smart chunking (QMD algorithm)
    let chunks = smart_chunk_markdown(
        &parsed.frontmatter.body,
        &parsed.title,
        parsed.frontmatter.frontmatter_lines,
        &config.chunk_config,
    );

    for chunk in &chunks {
        conn.execute(
            "INSERT INTO content_chunks (
                document_id, seq, char_pos, start_line, end_line,
                heading_context, chunk_text, formatted_text, token_estimate
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                doc_id,
                chunk.seq as i64,
                chunk.char_pos as i64,
                chunk.start_line as i64,
                chunk.end_line as i64,
                chunk.heading_context,
                chunk.chunk_text,
                chunk.formatted_text,
                chunk.token_estimate as i64,
            ],
        )?;
    }

    // Insert QMD typed metadata rows
    for (key, typed_val) in &parsed.frontmatter.qmd_metadata {
        insert_metadata_rows(conn, doc_id, key, typed_val)?;
    }

    // Insert extracted Obsidian graph links
    for link in &parsed.links {
        conn.execute(
            "INSERT INTO graph_links (
                source_doc_id, source_rel_path, raw_target, target_path_spec,
                target_heading, display_alias, link_type, line_number
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                doc_id,
                vault_rel_path,
                link.raw_target,
                link.target_path_spec,
                link.target_heading,
                link.display_alias,
                link.link_type.as_str(),
                link.line_number as i64,
            ],
        )?;
    }

    if resolve_links_immediately {
        resolve_all_links_in_db(conn)?;
        heal_category_wikilinks_on_disk(conn, config)?;
    }

    let info = get_document_info_by_id(conn, doc_id)?;
    Ok((info, is_new))
}

fn extract_clean_path_from_wikilink(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(trimmed);
    let before_alias = inner.split('|').next().unwrap_or(inner);
    let before_heading = before_alias.split('#').next().unwrap_or(before_alias);
    before_heading.trim().replace('\\', "/")
}

fn insert_metadata_rows(
    conn: &Connection,
    doc_id: i64,
    key: &str,
    val: &TypedMetaValue,
) -> WikiResult<()> {
    match val {
        TypedMetaValue::String(s) => {
            conn.execute(
                "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_str, val_str_ci)
                 VALUES (?1, ?2, 'string', 0, ?3, ?4)",
                params![doc_id, key, s, s.to_ascii_lowercase()],
            )?;
        }
        TypedMetaValue::Number(n) => {
            conn.execute(
                "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_num)
                 VALUES (?1, ?2, 'number', 0, ?3)",
                params![doc_id, key, n],
            )?;
        }
        TypedMetaValue::Bool(b) => {
            conn.execute(
                "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_bool)
                 VALUES (?1, ?2, 'boolean', 0, ?3)",
                params![doc_id, key, if *b { 1 } else { 0 }],
            )?;
        }
        TypedMetaValue::StringArray(arr) => {
            let mut seen = HashSet::new();
            for s in arr {
                if seen.insert(s) {
                    conn.execute(
                        "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_str, val_str_ci)
                         VALUES (?1, ?2, 'string', 1, ?3, ?4)",
                        params![doc_id, key, s, s.to_ascii_lowercase()],
                    )?;
                }
            }
        }
        TypedMetaValue::NumberArray(arr) => {
            for n in arr {
                conn.execute(
                    "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_num)
                     VALUES (?1, ?2, 'number', 1, ?3)",
                    params![doc_id, key, n],
                )?;
            }
        }
        TypedMetaValue::BoolArray(arr) => {
            let mut seen = HashSet::new();
            for &b in arr {
                if seen.insert(b) {
                    conn.execute(
                        "INSERT INTO document_metadata (document_id, meta_key, value_type, is_array, val_bool)
                         VALUES (?1, ?2, 'boolean', 1, ?3)",
                        params![doc_id, key, if b { 1 } else { 0 }],
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Remove a document from SQLite by its vault-relative path (`rel_path`).
pub fn remove_document_by_rel_path(conn: &Connection, vault_rel_path: &str) -> WikiResult<bool> {
    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM documents WHERE rel_path = ?1",
            params![vault_rel_path],
            |r| r.get(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute("DELETE FROM documents_fts WHERE rowid = ?1", params![id])?;
        conn.execute("DELETE FROM documents WHERE id = ?1", params![id])?;
        resolve_all_links_in_db(conn)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Synchronize both `sources/` and `wiki/` directories on disk with the SQLite database.
pub fn sync_filesystem(conn: &Connection, config: &WikiConfig) -> WikiResult<SyncStats> {
    let mut stats = SyncStats::default();
    let mut seen_rel_paths = HashSet::new();

    for (collection, base_dir, dir_name) in [
        (
            CollectionScope::Sources,
            config.sources_dir(),
            &config.sources_dir_name,
        ),
        (
            CollectionScope::Wiki,
            config.wiki_dir(),
            &config.wiki_dir_name,
        ),
    ] {
        if !base_dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&base_dir)
            .into_iter()
            .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_md = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if !is_md {
                continue;
            }

            let scoped_rel = path
                .strip_prefix(&base_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let vault_rel = format!("{dir_name}/{scoped_rel}");
            seen_rel_paths.insert(vault_rel.clone());

            let existed_before: Option<String> = conn
                .query_row(
                    "SELECT content_hash FROM documents WHERE rel_path = ?1",
                    params![vault_rel],
                    |r| r.get(0),
                )
                .optional()?;

            let (_, is_new) =
                index_single_file(conn, config, collection, &scoped_rel, path, false)?;
            if is_new {
                stats.indexed_new += 1;
            } else {
                let current_hash: String = conn.query_row(
                    "SELECT content_hash FROM documents WHERE rel_path = ?1",
                    params![vault_rel],
                    |r| r.get(0),
                )?;
                if existed_before.as_deref() == Some(current_hash.as_str()) {
                    stats.unchanged += 1;
                } else {
                    stats.updated += 1;
                }
            }
        }
    }

    // Remove deleted documents from DB
    let mut existing_stmt = conn.prepare("SELECT id, rel_path FROM documents")?;
    let all_db_docs: Vec<(i64, String)> = existing_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, rel_path) in all_db_docs {
        if !seen_rel_paths.contains(&rel_path) {
            conn.execute("DELETE FROM documents_fts WHERE rowid = ?1", params![id])?;
            conn.execute("DELETE FROM documents WHERE id = ?1", params![id])?;
            stats.removed += 1;
        }
    }

    // Resolve all Obsidian graph links across the vault
    resolve_all_links_in_db(conn)?;
    heal_category_wikilinks_on_disk(conn, config)?;

    stats.total_chunks = conn.query_row("SELECT COUNT(*) FROM content_chunks", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    stats.total_links = conn.query_row("SELECT COUNT(*) FROM graph_links", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    stats.needs_embedding = conn.query_row(
        "SELECT COUNT(*) FROM documents WHERE needs_embedding = 1",
        [],
        |r| r.get::<_, i64>(0),
    )? as usize;

    Ok(stats)
}

/// Fetch `DocumentInfo` by database primary key `id`.
pub fn get_document_info_by_id(conn: &Connection, id: i64) -> WikiResult<DocumentInfo> {
    conn.query_row(
        "SELECT id, docid, collection, rel_path, scoped_path, file_name, file_stem,
                title, category, source_rel_path, content_hash, byte_size, line_count,
                mtime_ms, tags_json, metadata_json
         FROM documents WHERE id = ?1",
        params![id],
        map_row_to_document_info,
    )
    .map_err(WikiError::from)
}

pub fn map_row_to_document_info(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentInfo> {
    let collection_str: String = row.get(2)?;
    let collection =
        CollectionScope::from_str_opt(&collection_str).unwrap_or(CollectionScope::Wiki);
    let tags_json: String = row.get(14)?;
    let meta_json: String = row.get(15)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let metadata: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&meta_json).unwrap_or_default();

    Ok(DocumentInfo {
        id: row.get(0)?,
        docid: row.get(1)?,
        collection,
        rel_path: row.get(3)?,
        scoped_path: row.get(4)?,
        file_name: row.get(5)?,
        file_stem: row.get(6)?,
        title: row.get(7)?,
        category: row.get(8)?,
        source_rel_path: row.get(9)?,
        content_hash: row.get(10)?,
        byte_size: row.get::<_, i64>(11)? as u64,
        line_count: row.get::<_, i64>(12)? as usize,
        mtime_ms: row.get(13)?,
        tags,
        metadata,
    })
}

/// Build a lookup map of `vault_rel_path -> TreeDocMeta` for `get_tree`.
pub fn load_tree_metadata_map(conn: &Connection) -> WikiResult<HashMap<String, TreeDocMeta>> {
    let mut stmt = conn.prepare("SELECT rel_path, docid, title, tags_json FROM documents")?;
    let rows = stmt.query_map([], |row| {
        let rel_path: String = row.get(0)?;
        let docid: String = row.get(1)?;
        let title: String = row.get(2)?;
        let tags_json: String = row.get(3)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        Ok((rel_path, TreeDocMeta { docid, title, tags }))
    })?;

    let mut map = HashMap::new();
    for r in rows {
        let (k, v) = r?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Retrieve a document by path (`rel_path`, `scoped_path`, `qmd://...`), `file_stem`, or `#docid`,
/// supporting QMD `:from[:count]` suffix and fuzzy suggestions if not found.
pub fn retrieve_document(
    conn: &Connection,
    config: &WikiConfig,
    path_or_docid: &str,
    opts: &GetOptions,
) -> WikiResult<DocumentResult> {
    let (raw_target, suffix_from, suffix_count) = parse_target_with_line_range(path_or_docid);
    let clean_target = raw_target
        .trim()
        .strip_prefix("qmd://")
        .unwrap_or(raw_target.trim())
        .replace('\\', "/");
    let clean_target = clean_target.trim_start_matches('/');

    // Special case: allow reading root `SCHEMA.md` via `get_document("SCHEMA.md", ...)`
    if clean_target.eq_ignore_ascii_case(&config.schema_file_name)
        || clean_target.eq_ignore_ascii_case("schema")
    {
        let schema_path = config.schema_path();
        if schema_path.exists() {
            let raw_text = std::fs::read_to_string(&schema_path)?;
            let from_line = opts.from_line.or(suffix_from);
            let max_lines = opts.max_lines.or(suffix_count);
            let content =
                slice_and_format_lines(&raw_text, from_line, max_lines, opts.line_numbers);
            let info = DocumentInfo {
                id: 0,
                docid: compute_docid(&config.schema_file_name),
                collection: CollectionScope::Wiki,
                rel_path: config.schema_file_name.clone(),
                scoped_path: config.schema_file_name.clone(),
                file_name: config.schema_file_name.clone(),
                file_stem: "SCHEMA".to_string(),
                title: "LLM Wiki Schema".to_string(),
                category: Some("schema".to_string()),
                source_rel_path: None,
                content_hash: compute_content_hash(&raw_text),
                byte_size: raw_text.len() as u64,
                line_count: raw_text.lines().count().max(1),
                mtime_ms: 0,
                tags: vec!["wiki/schema".to_string()],
                metadata: BTreeMap::new(),
            };
            return Ok(DocumentResult {
                info,
                context: Some("Root LLM Wiki Schema & Operating Conventions".to_string()),
                content,
                virtual_uri: format!("qmd://root/{}", config.schema_file_name),
            });
        }
    }

    let doc_id_opt = find_document_id_flexible(conn, clean_target)?;
    let Some(doc_id) = doc_id_opt else {
        let similar = find_similar_files(conn, clean_target, 5)?;
        return Err(WikiError::DocumentNotFound {
            target: path_or_docid.to_string(),
            similar_files: similar,
        });
    };

    let info = get_document_info_by_id(conn, doc_id)?;
    let full_disk_path = config.data_root.join(&info.rel_path);
    let raw_text = if opts.include_frontmatter && full_disk_path.exists() {
        std::fs::read_to_string(&full_disk_path)?
    } else {
        conn.query_row(
            "SELECT raw_body FROM documents WHERE id = ?1",
            params![doc_id],
            |r| r.get::<_, String>(0),
        )?
    };

    let from_line = opts.from_line.or(suffix_from);
    let max_lines = opts.max_lines.or(suffix_count);
    let content = slice_and_format_lines(&raw_text, from_line, max_lines, opts.line_numbers);
    let context = resolve_path_context_for_doc(conn, info.collection, &info.scoped_path)?;
    let virtual_uri = format!("qmd://{}/{}", info.collection.as_str(), info.scoped_path);

    Ok(DocumentResult {
        info,
        context,
        content,
        virtual_uri,
    })
}

fn find_document_id_flexible(conn: &Connection, target: &str) -> WikiResult<Option<i64>> {
    // 1. Check if it's a `#docid` or 6-char hex docid
    let docid_candidate = if target.starts_with('#') {
        target.to_ascii_lowercase()
    } else if target.len() == 6 && target.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", target.to_ascii_lowercase())
    } else {
        String::new()
    };

    if !docid_candidate.is_empty() {
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM documents WHERE lower(docid) = ?1",
                params![docid_candidate],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(Some(id));
        }
    }

    let with_md = if target.ends_with(".md") {
        target.to_string()
    } else {
        format!("{target}.md")
    };

    // 2. Exact match on `rel_path` or `scoped_path` (exact `rel_path` always wins first)
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM documents
             WHERE rel_path = ?1 OR rel_path = ?2 OR scoped_path = ?1 OR scoped_path = ?2
             ORDER BY
                CASE WHEN rel_path = ?1 OR rel_path = ?2 THEN 0 ELSE 1 END,
                CASE WHEN collection = 'wiki' THEN 0 ELSE 1 END,
                length(rel_path) ASC
             LIMIT 1",
            params![target, with_md],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(Some(id));
    }

    // 2b. Domain-scoped category lookup (`entities/<domain>/<slug>.md`, `concepts/<domain>/<slug>.md`, `syntheses/<domain>/<slug>.md`)
    let target_lower = target.to_ascii_lowercase();
    let without_wiki = target_lower.strip_prefix("wiki/").unwrap_or(&target_lower);
    for cat_prefix in ["entities/", "concepts/", "syntheses/"] {
        if without_wiki.starts_with(cat_prefix) {
            let leaf = without_wiki.rsplit('/').next().unwrap_or(without_wiki);
            let leaf_stem = leaf.strip_suffix(".md").unwrap_or(leaf);
            if !leaf_stem.is_empty() {
                let like_pattern = format!("{cat_prefix}%");
                if let Some(id) = conn
                    .query_row(
                        "SELECT id FROM documents
                         WHERE collection = 'wiki'
                           AND lower(scoped_path) LIKE ?1
                           AND (lower(file_stem) = ?2 OR lower(file_name) = ?3)
                         ORDER BY length(rel_path) ASC
                         LIMIT 1",
                        params![like_pattern, leaf_stem, leaf],
                        |r| r.get(0),
                    )
                    .optional()?
                {
                    return Ok(Some(id));
                }
            }
            break;
        }
    }

    // 3. Match on `file_stem` or `file_name` (case-insensitive)
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM documents
             WHERE lower(file_stem) = lower(?1) OR lower(file_name) = lower(?1)
             ORDER BY CASE WHEN collection = 'wiki' THEN 0 ELSE 1 END, length(rel_path) ASC
             LIMIT 1",
            params![target],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(Some(id));
    }

    // 4. Match on `aliases_json` (inspired by `javis-os` `_wiki_trung` alias lookup)
    let stem_target = target
        .strip_suffix(".md")
        .unwrap_or(target)
        .trim()
        .to_ascii_lowercase();
    if !stem_target.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT id, aliases_json FROM documents
             ORDER BY CASE WHEN collection = 'wiki' THEN 0 ELSE 1 END, length(rel_path) ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, aliases_json) = row?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            if aliases
                .iter()
                .any(|a| a.trim().eq_ignore_ascii_case(&stem_target))
            {
                return Ok(Some(id));
            }
        }
    }

    Ok(None)
}

fn extract_numbers(s: &str) -> Vec<String> {
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            nums.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        nums.push(cur);
    }
    nums
}

fn find_similar_files(conn: &Connection, query: &str, limit: usize) -> WikiResult<Vec<String>> {
    let q_lower = query.to_ascii_lowercase();
    let q_nums = extract_numbers(&q_lower);
    let mut stmt =
        conn.prepare("SELECT rel_path, file_stem, title, aliases_json FROM documents")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;

    let mut scored: Vec<(usize, String)> = Vec::new();
    for row in rows {
        let (rel_path, stem, title, aliases_json) = row?;
        let rel_l = rel_path.to_ascii_lowercase();
        let stem_l = stem.to_ascii_lowercase();
        let title_l = title.to_ascii_lowercase();
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();

        // Number-guard from `javis-os` `_wiki_trung`: if both strings contain numbers and they differ
        // (e.g., "IFRS 7" vs "IFRS 9", or "01-video" vs "02-video"), penalize fuzzy similarity heavily.
        let cand_nums = extract_numbers(&stem_l);
        let number_mismatch =
            !q_nums.is_empty() && !cand_nums.is_empty() && q_nums != cand_nums;

        let alias_match = aliases
            .iter()
            .any(|a| a.to_ascii_lowercase().contains(&q_lower));

        let mut score = if rel_l.contains(&q_lower) || q_lower.contains(&stem_l) || alias_match {
            100
        } else {
            common_char_score(&q_lower, &stem_l).max(common_char_score(&q_lower, &title_l))
        };
        if number_mismatch {
            score /= 3;
        }
        if score > 20 {
            scored.push((score, rel_path));
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(scored.into_iter().take(limit).map(|(_, p)| p).collect())
}

fn common_char_score(a: &str, b: &str) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let a_bigrams: HashSet<(char, char)> = a.chars().zip(a.chars().skip(1)).collect();
    let b_bigrams: HashSet<(char, char)> = b.chars().zip(b.chars().skip(1)).collect();
    if a_bigrams.is_empty() || b_bigrams.is_empty() {
        return 0;
    }
    let inter = a_bigrams.intersection(&b_bigrams).count();
    (inter * 200) / (a_bigrams.len() + b_bigrams.len())
}

/// Batch retrieve multiple documents by glob pattern or comma-separated list (QMD `multi_get`).
pub fn multi_get_documents(
    conn: &Connection,
    config: &WikiConfig,
    pattern_or_list: &str,
    opts: &MultiGetOptions,
) -> WikiResult<MultiGetResult> {
    let trimmed = pattern_or_list.trim();
    let mut docs = Vec::new();
    let mut errors = Vec::new();

    let get_opts = GetOptions {
        from_line: None,
        max_lines: opts.max_lines,
        line_numbers: opts.line_numbers,
        include_frontmatter: true,
    };

    // Check if comma-separated list vs glob pattern
    if trimmed.contains(',') || (!trimmed.contains('*') && !trimmed.contains('?')) {
        for part in trimmed.split(',') {
            let item = part.trim();
            if item.is_empty() {
                continue;
            }
            match retrieve_document(conn, config, item, &get_opts) {
                Ok(doc) => {
                    if (doc.info.byte_size as usize) <= opts.max_bytes {
                        docs.push(doc);
                    } else {
                        errors.push(MultiGetError {
                            target: item.to_string(),
                            reason: format!(
                                "File size ({} bytes) exceeds max_bytes ({})",
                                doc.info.byte_size, opts.max_bytes
                            ),
                        });
                    }
                }
                Err(e) => errors.push(MultiGetError {
                    target: item.to_string(),
                    reason: e.to_string(),
                }),
            }
        }
        return Ok(MultiGetResult { docs, errors });
    }

    // Glob pattern matching over `rel_path` and `scoped_path`
    let glob_matcher = Glob::new(trimmed)?.compile_matcher();
    let mut stmt = conn
        .prepare("SELECT rel_path, scoped_path, byte_size FROM documents ORDER BY rel_path ASC")?;
    let all_rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (rel_path, scoped_path, byte_size) in all_rows {
        if glob_matcher.is_match(&rel_path) || glob_matcher.is_match(&scoped_path) {
            if (byte_size as usize) > opts.max_bytes {
                errors.push(MultiGetError {
                    target: rel_path,
                    reason: format!(
                        "File size ({byte_size} bytes) exceeds max_bytes ({})",
                        opts.max_bytes
                    ),
                });
                continue;
            }
            match retrieve_document(conn, config, &rel_path, &get_opts) {
                Ok(doc) => docs.push(doc),
                Err(e) => errors.push(MultiGetError {
                    target: rel_path,
                    reason: e.to_string(),
                }),
            }
        }
    }

    Ok(MultiGetResult { docs, errors })
}

/// Resolve the hierarchical QMD Path Context for a document (global + longest matching path prefix).
pub fn resolve_path_context_for_doc(
    conn: &Connection,
    collection: CollectionScope,
    scoped_path: &str,
) -> WikiResult<Option<String>> {
    let global_ctx: Option<String> = conn
        .query_row(
            "SELECT description FROM path_contexts WHERE scope = 'global' AND path_prefix = '/'",
            [],
            |r| r.get(0),
        )
        .optional()?;

    let mut stmt = conn.prepare(
        "SELECT path_prefix, description FROM path_contexts
         WHERE scope = ?1
         ORDER BY length(path_prefix) DESC",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![collection.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let norm_doc_path = format!("/{}", scoped_path.trim_start_matches('/'));
    let mut matched_desc = None;
    for (prefix, desc) in rows {
        let norm_prefix = if prefix.starts_with('/') {
            prefix
        } else {
            format!("/{prefix}")
        };
        if norm_prefix == "/"
            || norm_doc_path == norm_prefix
            || norm_doc_path.starts_with(&format!("{norm_prefix}/"))
        {
            matched_desc = Some(desc);
            break;
        }
    }

    match (global_ctx, matched_desc) {
        (Some(g), Some(m)) => Ok(Some(format!("{g} — {m}"))),
        (Some(g), None) => Ok(Some(g)),
        (None, Some(m)) => Ok(Some(m)),
        (None, None) => Ok(None),
    }
}

/// Resolve outgoing `[[WikiLinks]]`, incoming backlinks (excluding system files `index.md` / `log.md`),
/// and unresolved link targets for a document in `SearchResultItem`.
pub fn resolve_wiki_links_for_doc(
    conn: &Connection,
    doc_id: i64,
    doc_rel_path: &str,
) -> WikiResult<(Vec<String>, Vec<String>, Vec<String>)> {
    let mut out_stmt = conn.prepare(
        "SELECT resolved_rel_path, target_path_spec
         FROM graph_links
         WHERE source_doc_id = ?1
         ORDER BY line_number ASC, id ASC",
    )?;
    let out_rows = out_stmt.query_map(params![doc_id], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
    })?;

    let mut outgoing_links = Vec::new();
    let mut unresolved_links = Vec::new();
    for row in out_rows {
        let (resolved_opt, target_spec) = row?;
        if let Some(target_rel) = resolved_opt {
            if target_rel != doc_rel_path && !outgoing_links.contains(&target_rel) {
                outgoing_links.push(target_rel);
            }
        } else if !target_spec.is_empty() && !unresolved_links.contains(&target_spec) {
            unresolved_links.push(target_spec);
        }
    }

    let mut back_stmt = conn.prepare(
        "SELECT DISTINCT gl.source_rel_path
         FROM graph_links gl
         JOIN documents src ON src.id = gl.source_doc_id
         WHERE gl.resolved_doc_id = ?1
           AND COALESCE(src.category, '') != 'system'
         ORDER BY gl.source_rel_path ASC",
    )?;
    let back_rows = back_stmt.query_map(params![doc_id], |r| r.get::<_, String>(0))?;

    let mut backlinks = Vec::new();
    for row in back_rows {
        let src_rel = row?;
        if src_rel != doc_rel_path && !backlinks.contains(&src_rel) {
            backlinks.push(src_rel);
        }
    }

    Ok((outgoing_links, backlinks, unresolved_links))
}

/// Set or update a QMD Path Context description.
pub fn upsert_path_context(
    conn: &Connection,
    scope: &str,
    path_prefix: &str,
    description: &str,
) -> WikiResult<()> {
    let norm_prefix = if path_prefix.trim().is_empty() || path_prefix.trim() == "/" {
        "/".to_string()
    } else {
        format!("/{}", path_prefix.trim().trim_matches('/'))
    };
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO path_contexts (scope, path_prefix, description, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scope, path_prefix) DO UPDATE SET
            description = excluded.description,
            updated_at = excluded.updated_at",
        params![scope, norm_prefix, description, now],
    )?;
    Ok(())
}

/// List all configured QMD Path Contexts.
pub fn get_all_path_contexts(conn: &Connection) -> WikiResult<Vec<PathContextItem>> {
    let mut stmt = conn.prepare(
        "SELECT scope, path_prefix, description FROM path_contexts ORDER BY scope ASC, path_prefix ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PathContextItem {
            scope: r.get(0)?,
            path_prefix: r.get(1)?,
            description: r.get(2)?,
        })
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

/// Extract the canonical `<domain>` folder slug (e.g. `"quantitative-finance"`, `"ai-engineering"`, `"shared"`)
/// from a Markdown document's YAML frontmatter (`domain` property or `topic/<domain>/...` tag).
pub fn extract_domain_from_markdown_frontmatter(markdown_content: &str) -> Option<String> {
    let fm = parse_frontmatter(markdown_content);
    if let Some(TypedMetaValue::String(d)) = fm.qmd_metadata.get("domain") {
        let slug = slugify(d, 40);
        if !slug.is_empty() {
            return Some(slug);
        }
    }
    if let Some(ref map) = fm.json_map {
        if let Some(d) = map.get("domain").and_then(|v| v.as_str()) {
            let slug = slugify(d, 40);
            if !slug.is_empty() {
                return Some(slug);
            }
        }
    }
    for tag in &fm.tags {
        let clean = tag.trim().trim_start_matches('#');
        if let Some(rest) = clean.strip_prefix("topic/") {
            let first_seg = rest.split('/').next().unwrap_or(rest).trim();
            let slug = slugify(first_seg, 40);
            if !slug.is_empty() {
                return Some(slug);
            }
        }
    }
    None
}

/// Resolve the canonical `scoped_path` inside `wiki/` for `entities/`, `concepts/`, and `syntheses/` pages:
/// 1. **Global Unique Slug Redirect**: If a page with the same `file_stem` already exists anywhere under
///    the same category folder (`entities/.../<slug>.md`, `concepts/.../<slug>.md`, `syntheses/.../<slug>.md`),
///    always redirect to that existing `scoped_path` to prevent duplicate files across domains/topics.
/// 2. **1-Level `<domain>` Folder Routing**: For a new page in `entities/`, `concepts/`, or `syntheses/`,
///    if the Markdown frontmatter specifies `domain: "<domain>"` (or a `topic/<domain>/...` tag),
///    automatically organize it under `<category>/<domain>/<slug>.md` (or `<category>/shared/<slug>.md`).
pub fn resolve_canonical_wiki_scoped_path(
    conn: &Connection,
    requested_scoped: &str,
    markdown_content: &str,
) -> WikiResult<String> {
    let clean_requested = sanitize_rel_path(requested_scoped)?;
    let lower_req = clean_requested.to_ascii_lowercase();

    for cat_prefix in ["entities/", "concepts/", "syntheses/"] {
        if lower_req.starts_with(cat_prefix) {
            let rest = &clean_requested[cat_prefix.len()..];
            let file_name = rest.rsplit('/').next().unwrap_or(rest);
            let file_stem = file_name.strip_suffix(".md").unwrap_or(file_name);
            if file_stem.is_empty() {
                return Ok(clean_requested);
            }

            // 1. Check if a page with the same `file_stem` already exists anywhere in this category
            let like_pattern = format!("{cat_prefix}%");
            if let Some(existing_scoped) = conn
                .query_row(
                    "SELECT scoped_path FROM documents
                     WHERE collection = 'wiki'
                       AND lower(scoped_path) LIKE ?1
                       AND lower(file_stem) = lower(?2)
                     ORDER BY
                        CASE WHEN lower(scoped_path) = lower(?3) THEN 0 ELSE 1 END,
                        length(scoped_path) ASC
                     LIMIT 1",
                    params![like_pattern, file_stem, clean_requested],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(existing_scoped);
            }

            // 2. If explicitly placed in `shared/<slug>.md`, preserve `shared`
            let rest_lower = rest.to_ascii_lowercase();
            if rest_lower.starts_with("shared/") {
                return Ok(format!("{cat_prefix}shared/{file_name}"));
            }

            // 3. If frontmatter specifies `domain` (or `topic/<domain>/...`), organize into 1-level `<category>/<domain>/<file_name>`
            if let Some(domain) = extract_domain_from_markdown_frontmatter(markdown_content) {
                return Ok(format!("{cat_prefix}{domain}/{file_name}"));
            }

            return Ok(clean_requested);
        }
    }

    Ok(clean_requested)
}

/// Rewrite category-prefixed Obsidian `[[WikiLinks]]` (`[[entities/...]]`, `[[concepts/...]]`, `[[syntheses/...]]`)
/// outside fenced code blocks so their path matches the canonical `scoped_path` on disk (e.g. `[[entities/quantitative-finance/aiv-bank|AIV Bank]]`).
pub fn heal_category_wikilinks_in_markdown(
    markdown: &str,
    canonical_map: &HashMap<(String, String), String>,
) -> String {
    if canonical_map.is_empty() || !markdown.contains("[[") {
        return markdown.to_string();
    }

    let mut out_lines = Vec::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out_lines.push(line.to_string());
            continue;
        }
        if in_fence || !line.contains("[[") {
            out_lines.push(line.to_string());
            continue;
        }

        let mut healed_line = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(open_idx) = rest.find("[[") {
            healed_line.push_str(&rest[..open_idx]);
            let after_open = &rest[open_idx + 2..];
            if let Some(close_idx) = after_open.find("]]") {
                let inner = &after_open[..close_idx];
                let (before_pipe, alias_suffix) = match inner.find('|') {
                    Some(p) => (&inner[..p], &inner[p..]),
                    None => (inner, ""),
                };
                let (raw_path, heading_suffix) = match before_pipe.find('#') {
                    Some(h) => (&before_pipe[..h], &before_pipe[h..]),
                    None => (before_pipe, ""),
                };
                let clean_path = raw_path.trim().replace('\\', "/");
                let clean_lower = clean_path.to_ascii_lowercase();
                let has_wiki_prefix = clean_lower.starts_with("wiki/");
                let without_wiki = if has_wiki_prefix {
                    &clean_path[5..]
                } else {
                    clean_path.as_str()
                };
                let without_wiki_lower = without_wiki.to_ascii_lowercase();

                let mut rewritten_path: Option<String> = None;
                for cat in ["entities", "concepts", "syntheses"] {
                    let prefix = format!("{cat}/");
                    if without_wiki_lower.starts_with(&prefix) {
                        let leaf = without_wiki_lower
                            .rsplit('/')
                            .next()
                            .unwrap_or(&without_wiki_lower);
                        let leaf_stem = leaf.strip_suffix(".md").unwrap_or(leaf);
                        if let Some(canonical_scoped_no_ext) =
                            canonical_map.get(&(cat.to_string(), leaf_stem.to_string()))
                        {
                            let md_ext = if without_wiki_lower.ends_with(".md") {
                                ".md"
                            } else {
                                ""
                            };
                            let candidate = if has_wiki_prefix {
                                format!("wiki/{canonical_scoped_no_ext}{md_ext}")
                            } else {
                                format!("{canonical_scoped_no_ext}{md_ext}")
                            };
                            if !candidate.eq_ignore_ascii_case(&clean_path) {
                                rewritten_path = Some(candidate);
                            }
                        }
                        break;
                    }
                }

                healed_line.push_str("[[");
                if let Some(new_p) = rewritten_path {
                    healed_line.push_str(&new_p);
                } else {
                    healed_line.push_str(raw_path);
                }
                healed_line.push_str(heading_suffix);
                healed_line.push_str(alias_suffix);
                healed_line.push_str("]]");
                rest = &after_open[close_idx + 2..];
            } else {
                healed_line.push_str(&rest[open_idx..]);
                rest = "";
                break;
            }
        }
        healed_line.push_str(rest);
        out_lines.push(healed_line);
    }

    let mut joined = out_lines.join("\n");
    if markdown.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Synchronize category-prefixed `[[WikiLinks]]` on disk for any `wiki` pages whose link targets
/// (`entities/...`, `concepts/...`, `syntheses/...`) are stored inside `<domain>` subfolders.
pub fn heal_category_wikilinks_on_disk(conn: &Connection, config: &WikiConfig) -> WikiResult<usize> {
    let mut cat_stmt = conn.prepare(
        "SELECT scoped_path, file_stem
         FROM documents
         WHERE collection = 'wiki'
           AND (scoped_path LIKE 'entities/%' OR scoped_path LIKE 'concepts/%' OR scoped_path LIKE 'syntheses/%')",
    )?;
    let rows = cat_stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;

    let mut canonical_map: HashMap<(String, String), String> = HashMap::new();
    for row in rows {
        let (scoped_path, file_stem) = row?;
        let cat = scoped_path
            .split('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let scoped_no_ext = scoped_path
            .strip_suffix(".md")
            .unwrap_or(&scoped_path)
            .to_string();
        canonical_map.insert((cat, file_stem.to_ascii_lowercase()), scoped_no_ext);
    }

    if canonical_map.is_empty() {
        return Ok(0);
    }

    let mut link_stmt = conn.prepare(
        "SELECT src.scoped_path, src.rel_path, gl.target_path_spec
         FROM graph_links gl
         JOIN documents src ON src.id = gl.source_doc_id
         WHERE src.collection = 'wiki'
           AND src.scoped_path NOT IN ('index.md', 'log.md')",
    )?;
    let link_rows = link_stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;

    let mut files_to_heal: BTreeSet<(String, String)> = BTreeSet::new();
    for row in link_rows {
        let (src_scoped, src_rel, target_spec) = row?;
        let spec_clean = target_spec.trim().trim_start_matches('/').replace('\\', "/");
        let spec_lower = spec_clean.to_ascii_lowercase();
        let without_wiki = spec_lower.strip_prefix("wiki/").unwrap_or(&spec_lower);
        let without_wiki_no_md = without_wiki.strip_suffix(".md").unwrap_or(without_wiki);

        for cat in ["entities", "concepts", "syntheses"] {
            let prefix = format!("{cat}/");
            if without_wiki_no_md.starts_with(&prefix) {
                let leaf_stem = without_wiki_no_md
                    .rsplit('/')
                    .next()
                    .unwrap_or(without_wiki_no_md);
                if let Some(canonical_scoped_no_ext) =
                    canonical_map.get(&(cat.to_string(), leaf_stem.to_string()))
                {
                    if !canonical_scoped_no_ext.eq_ignore_ascii_case(without_wiki_no_md) {
                        files_to_heal.insert((src_scoped.clone(), src_rel.clone()));
                    }
                }
                break;
            }
        }
    }

    if files_to_heal.is_empty() {
        return Ok(0);
    }

    let mut healed_count = 0usize;
    for (src_scoped, src_rel) in &files_to_heal {
        let full_path = config.data_root.join(src_rel);
        if !full_path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&full_path)?;
        let healed = heal_category_wikilinks_in_markdown(&raw, &canonical_map);
        if healed != raw {
            std::fs::write(&full_path, &healed)?;
            index_single_file(
                conn,
                config,
                CollectionScope::Wiki,
                src_scoped,
                &full_path,
                false,
            )?;
            healed_count += 1;
        }
    }

    if healed_count > 0 {
        resolve_all_links_in_db(conn)?;
    }

    Ok(healed_count)
}
