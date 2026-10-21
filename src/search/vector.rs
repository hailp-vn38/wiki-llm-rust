use crate::db::filter_sql::resolve_matching_document_ids;
use crate::db::index_store::{get_document_info_by_id, resolve_path_context_for_doc};
use crate::db::schema::encode_f32_blob;
use crate::error::WikiResult;
use crate::search::{ExpandedQuery, SearchOptions, SearchResultItem};
use crate::types::{CollectionScope, EmbedStats};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

/// Pluggable Vector Embedding Provider trait (supports local GGUF/FastEmbed or remote OpenAI/Ollama).
pub trait EmbeddingProvider: Send + Sync {
    /// Model identifier name.
    fn model_name(&self) -> &str;
    /// Vector dimensionality (e.g., 256, 384, 768, 1536).
    fn dimensions(&self) -> usize;
    /// Embed a search query (QMD formats queries as `"task: search result | query: {query}"`).
    fn embed_query(&self, query: &str) -> WikiResult<Vec<f32>>;
    /// Embed a batch of formatted document chunks (`"title: {title} | text: {content}"`).
    fn embed_documents(&self, formatted_chunks: &[String]) -> WikiResult<Vec<Vec<f32>>>;
}

/// Pluggable Cross-Encoder Reranker & Query Expansion Provider trait (QMD pipeline).
pub trait RerankProvider: Send + Sync {
    /// Score each candidate document text against `query` in range `[0.0, 1.0]`.
    fn rerank(&self, query: &str, candidates: &[String]) -> WikiResult<Vec<f32>>;
    /// Generate expanded sub-queries (`lex`, `vec`, `hyde`) for `query`.
    fn expand_query(&self, query: &str, intent: Option<&str>) -> WikiResult<Vec<ExpandedQuery>>;
}

/// Built-in on-device deterministic Feature-Hashing (Word + Char N-gram) Embedder.
/// Enables local vector search and hybrid RRF fusion out-of-the-box without external model downloads,
/// while users can replace it with any neural GGUF/ONNX/API `EmbeddingProvider`.
#[derive(Debug, Clone)]
pub struct LocalNgramEmbedder {
    dimensions: usize,
}

impl Default for LocalNgramEmbedder {
    fn default() -> Self {
        Self { dimensions: 256 }
    }
}

impl LocalNgramEmbedder {
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions: dimensions.max(32),
        }
    }

    fn compute_vector(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimensions];
        let lower = text.to_lowercase();

        // 1. Word unigrams and bigrams (higher weight)
        let words: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2)
            .collect();

        for (i, &w) in words.iter().enumerate() {
            self.Accumulate_token(&mut vec, w, 2.0);
            if i + 1 < words.len() {
                let bigram = format!("{}_{}", w, words[i + 1]);
                self.Accumulate_token(&mut vec, &bigram, 1.5);
            }
            // Character 3-grams for morphological/fuzzy resilience
            let chars: Vec<char> = w.chars().collect();
            if chars.len() >= 3 {
                for win in chars.windows(3) {
                    let trigram: String = win.iter().collect();
                    self.Accumulate_token(&mut vec, &trigram, 0.5);
                }
            }
        }

        // L2 normalize
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for v in &mut vec {
                *v /= norm;
            }
        }
        vec
    }

    #[allow(non_snake_case)]
    fn Accumulate_token(&self, vec: &mut [f32], token: &str, weight: f32) {
        let hash = blake3::hash(token.as_bytes());
        let bytes = hash.as_bytes();
        let idx_raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let sign = if (bytes[4] & 1) == 0 { 1.0f32 } else { -1.0f32 };
        let idx = idx_raw % self.dimensions;
        vec[idx] += sign * weight;
    }
}

impl EmbeddingProvider for LocalNgramEmbedder {
    fn model_name(&self) -> &str {
        "local-ngram-hash-256"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_query(&self, query: &str) -> WikiResult<Vec<f32>> {
        // Strip QMD query prefix if present so query tokens align cleanly with document tokens
        let clean = query
            .strip_prefix("task: search result | query:")
            .unwrap_or(query)
            .trim();
        Ok(self.compute_vector(clean))
    }

    fn embed_documents(&self, formatted_chunks: &[String]) -> WikiResult<Vec<Vec<f32>>> {
        Ok(formatted_chunks
            .iter()
            .map(|c| self.compute_vector(c))
            .collect())
    }
}

/// Generate and store vector embeddings for all pending documents (or all documents if `force_all == true`).
pub fn embed_pending_documents(
    conn: &Connection,
    embedder: &dyn EmbeddingProvider,
    force_all: bool,
) -> WikiResult<EmbedStats> {
    if force_all {
        conn.execute("DELETE FROM chunk_vectors", [])?;
        conn.execute("UPDATE documents SET needs_embedding = 1", [])?;
    }

    let mut doc_stmt = conn.prepare(
        "SELECT id, collection FROM documents WHERE needs_embedding = 1 ORDER BY id ASC",
    )?;
    let pending_docs: Vec<(i64, String)> = doc_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut stats = EmbedStats {
        documents_embedded: 0,
        chunks_embedded: 0,
        model_name: embedder.model_name().to_string(),
        dimensions: embedder.dimensions(),
    };

    for (doc_id, collection) in pending_docs {
        let mut chunk_stmt = conn.prepare(
            "SELECT id, formatted_text FROM content_chunks WHERE document_id = ?1 ORDER BY seq ASC",
        )?;
        let chunks: Vec<(i64, String)> = chunk_stmt
            .query_map(params![doc_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        conn.execute(
            "DELETE FROM chunk_vectors WHERE document_id = ?1",
            params![doc_id],
        )?;

        if !chunks.is_empty() {
            let texts: Vec<String> = chunks.iter().map(|(_, t)| t.clone()).collect();
            let vectors = embedder.embed_documents(&texts)?;

            for ((chunk_id, _), vec) in chunks.iter().zip(vectors.iter()) {
                let blob = encode_f32_blob(vec);
                conn.execute(
                    "INSERT INTO chunk_vectors (chunk_id, document_id, collection, dimensions, embedding_blob)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![chunk_id, doc_id, collection, vec.len() as i64, blob],
                )?;
                stats.chunks_embedded += 1;
            }
        }

        conn.execute(
            "UPDATE documents SET needs_embedding = 0 WHERE id = ?1",
            params![doc_id],
        )?;
        stats.documents_embedded += 1;
    }

    Ok(stats)
}

/// Perform QMD Vector Semantic Search across embedded chunks, deduplicating by document
/// (retaining each document's highest-scoring chunk) and converting cosine distance via `1 / (1 + distance)`.
pub fn search_vector(
    conn: &Connection,
    embedder: &dyn EmbeddingProvider,
    query: &str,
    opts: &SearchOptions,
) -> WikiResult<Vec<SearchResultItem>> {
    // Auto-embed any pending documents so vector search is always up-to-date
    let pending_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM documents WHERE needs_embedding = 1",
        [],
        |r| r.get(0),
    )?;
    if pending_count > 0 {
        let _ = embed_pending_documents(conn, embedder, false)?;
    }

    let formatted_query = format!("task: search result | query: {query}");
    let query_vec = embedder.embed_query(&formatted_query)?;
    let query_blob = encode_f32_blob(&query_vec);

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

    let mut stmt = conn.prepare(
        "SELECT
            cv.document_id,
            cv.collection,
            cc.chunk_text,
            cc.start_line,
            vec_cosine_distance(cv.embedding_blob, ?1) AS distance
         FROM chunk_vectors cv
         JOIN content_chunks cc ON cc.id = cv.chunk_id
         ORDER BY distance ASC",
    )?;

    let rows = stmt.query_map(params![query_blob], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)? as usize,
            r.get::<_, f64>(4)? as f32,
        ))
    })?;

    // Group by `document_id`, keeping the best (lowest distance) chunk per document
    let mut best_by_doc: HashMap<i64, (f32, String, usize)> = HashMap::new();
    let mut doc_order: Vec<i64> = Vec::new();

    for row in rows {
        let (doc_id, coll_str, chunk_text, start_line, distance) = row?;
        if !allowed_collections.contains(coll_str.as_str()) {
            continue;
        }
        if let Some(ref allowed) = allowed_doc_ids {
            if !allowed.contains(&doc_id) {
                continue;
            }
        }

        // QMD score conversion: `1 / (1 + distance)`
        let score = (1.0 / (1.0 + distance.max(0.0))).clamp(0.0, 1.0);
        if score < opts.min_score {
            continue;
        }

        if let std::collections::hash_map::Entry::Vacant(e) = best_by_doc.entry(doc_id) {
            e.insert((score, chunk_text, start_line));
            doc_order.push(doc_id);
        }
    }

    let mut results = Vec::new();
    for doc_id in doc_order {
        if let Some((score, chunk_text, start_line)) = best_by_doc.remove(&doc_id) {
            let info = get_document_info_by_id(conn, doc_id)?;
            let snippet = chunk_text
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            let context = resolve_path_context_for_doc(conn, info.collection, &info.scoped_path)?;
            let virtual_uri = format!("qmd://{}/{}", info.collection.as_str(), info.scoped_path);

            results.push(SearchResultItem {
                docid: info.docid,
                rel_path: info.rel_path,
                virtual_uri,
                title: info.title,
                collection: info.collection,
                score,
                raw_backend_score: score,
                match_line: start_line,
                snippet,
                context,
                metadata: info.metadata,
                explain: None,
            });

            if results.len() >= opts.limit {
                break;
            }
        }
    }

    Ok(results)
}
