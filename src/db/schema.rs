use crate::error::WikiResult;
use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;

/// Initialize SQLite connection settings, custom vector functions, and schema DDL.
pub fn init_database(conn: &Connection) -> WikiResult<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        ",
    )?;

    register_vector_functions(conn)?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS path_contexts (
            scope TEXT NOT NULL,
            path_prefix TEXT NOT NULL,
            description TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (scope, path_prefix)
        );

        CREATE TABLE IF NOT EXISTS documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            docid TEXT NOT NULL UNIQUE,
            collection TEXT NOT NULL,
            rel_path TEXT NOT NULL UNIQUE,
            scoped_path TEXT NOT NULL,
            file_name TEXT NOT NULL,
            file_stem TEXT NOT NULL,
            title TEXT NOT NULL,
            category TEXT,
            source_rel_path TEXT,
            content_hash TEXT NOT NULL,
            raw_body TEXT NOT NULL,
            frontmatter_json TEXT,
            tags_json TEXT NOT NULL DEFAULT '[]',
            aliases_json TEXT NOT NULL DEFAULT '[]',
            metadata_json TEXT NOT NULL DEFAULT '{}',
            byte_size INTEGER NOT NULL,
            line_count INTEGER NOT NULL,
            mtime_ms INTEGER NOT NULL,
            indexed_at TEXT NOT NULL,
            needs_embedding INTEGER NOT NULL DEFAULT 1
        );

        CREATE INDEX IF NOT EXISTS idx_docs_collection ON documents(collection);
        CREATE INDEX IF NOT EXISTS idx_docs_file_stem ON documents(file_stem);
        CREATE INDEX IF NOT EXISTS idx_docs_scoped_path ON documents(scoped_path);
        CREATE INDEX IF NOT EXISTS idx_docs_source_rel ON documents(source_rel_path);

        CREATE VIRTUAL TABLE IF NOT EXISTS documents_fts USING fts5(
            docid UNINDEXED,
            rel_path UNINDEXED,
            collection UNINDEXED,
            title,
            headings,
            body,
            tags,
            tokenize = 'unicode61 remove_diacritics 2'
        );

        CREATE TABLE IF NOT EXISTS content_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL,
            char_pos INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            heading_context TEXT NOT NULL DEFAULT '',
            chunk_text TEXT NOT NULL,
            formatted_text TEXT NOT NULL,
            token_estimate INTEGER NOT NULL,
            UNIQUE(document_id, seq)
        );

        CREATE INDEX IF NOT EXISTS idx_chunks_doc ON content_chunks(document_id);

        CREATE TABLE IF NOT EXISTS chunk_vectors (
            chunk_id INTEGER PRIMARY KEY REFERENCES content_chunks(id) ON DELETE CASCADE,
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            collection TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding_blob BLOB NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_vectors_collection ON chunk_vectors(collection);
        CREATE INDEX IF NOT EXISTS idx_vectors_doc ON chunk_vectors(document_id);

        CREATE TABLE IF NOT EXISTS document_metadata (
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            meta_key TEXT NOT NULL,
            value_type TEXT NOT NULL,
            is_array INTEGER NOT NULL,
            val_str TEXT,
            val_str_ci TEXT,
            val_num REAL,
            val_bool INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_meta_key_str ON document_metadata(meta_key, value_type, val_str);
        CREATE INDEX IF NOT EXISTS idx_meta_key_num ON document_metadata(meta_key, value_type, val_num);
        CREATE INDEX IF NOT EXISTS idx_meta_doc ON document_metadata(document_id);

        CREATE TABLE IF NOT EXISTS graph_links (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            source_rel_path TEXT NOT NULL,
            raw_target TEXT NOT NULL,
            target_path_spec TEXT NOT NULL,
            target_heading TEXT,
            display_alias TEXT,
            link_type TEXT NOT NULL,
            resolved_doc_id INTEGER REFERENCES documents(id) ON DELETE SET NULL,
            resolved_rel_path TEXT,
            line_number INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_links_source ON graph_links(source_doc_id);
        CREATE INDEX IF NOT EXISTS idx_links_resolved ON graph_links(resolved_doc_id);
        CREATE INDEX IF NOT EXISTS idx_links_target_spec ON graph_links(target_path_spec);

        CREATE TABLE IF NOT EXISTS llm_cache (
            cache_key TEXT PRIMARY KEY,
            cache_type TEXT NOT NULL,
            response_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        ",
    )?;

    Ok(())
}

/// Register `vec_cosine_distance(blob_a, blob_b)` and `vec_cosine_sim(blob_a, blob_b)` in SQLite.
fn register_vector_functions(conn: &Connection) -> WikiResult<()> {
    conn.create_scalar_function(
        "vec_cosine_sim",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let a_blob = ctx.get_raw(0).as_blob()?;
            let b_blob = ctx.get_raw(1).as_blob()?;
            Ok(cosine_similarity_from_blobs(a_blob, b_blob) as f64)
        },
    )?;

    conn.create_scalar_function(
        "vec_cosine_distance",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let a_blob = ctx.get_raw(0).as_blob()?;
            let b_blob = ctx.get_raw(1).as_blob()?;
            let sim = cosine_similarity_from_blobs(a_blob, b_blob);
            let dist = (1.0 - sim).max(0.0);
            Ok(dist as f64)
        },
    )?;

    Ok(())
}

/// Encode a `f32` slice into little-endian bytes for SQLite `BLOB` storage.
pub fn encode_f32_blob(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &val in vec {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Decode little-endian bytes from SQLite `BLOB` into `Vec<f32>`.
pub fn decode_f32_blob(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|&chunk| f32::from_le_bytes(chunk))
        .collect()
}

/// Compute cosine similarity between two little-endian `f32` byte slices.
pub fn cosine_similarity_from_blobs(a_blob: &[u8], b_blob: &[u8]) -> f32 {
    if a_blob.len() != b_blob.len() || a_blob.len() < 4 {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (&ca, &cb) in a_blob
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b_blob.as_chunks::<4>().0.iter())
    {
        let va = f32::from_le_bytes(ca);
        let vb = f32::from_le_bytes(cb);
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= f32::EPSILON {
        0.0
    } else {
        (dot / denom).clamp(-1.0, 1.0)
    }
}
