pub mod fts;
pub mod metadata;
pub mod rrf;
pub mod vector;

use crate::db::filter_sql::MetadataFilter;
use crate::types::CollectionScope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Type of sub-query in QMD Hybrid Search (`lex`, `vec`, `hyde`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubQueryType {
    /// Lexical BM25 full-text search (routed to SQLite FTS5).
    Lex,
    /// Dense vector semantic search (routed to vector embeddings).
    Vec,
    /// Hypothetical Document Embedding (routed to vector embeddings).
    Hyde,
}

impl SubQueryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lex => "lex",
            Self::Vec => "vec",
            Self::Hyde => "hyde",
        }
    }
}

/// Typed sub-query for QMD hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubQuery {
    #[serde(rename = "type")]
    pub query_type: SubQueryType,
    pub query: String,
}

pub type ExpandedQuery = SubQuery;

/// Options for direct `search_lex` or `search_vec` calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOptions {
    #[serde(default)]
    pub collections: Vec<CollectionScope>,
    pub filter: Option<MetadataFilter>,
    pub limit: usize,
    pub min_score: f32,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            collections: Vec::new(),
            filter: None,
            limit: 10,
            min_score: 0.0,
        }
    }
}

/// Full request structure for QMD Hybrid Search (`WikiEngine::query`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridQueryRequest {
    /// Optional single query string (if `searches` is empty, automatically expands into `lex` + `vec`).
    pub query: Option<String>,
    /// Typed sub-queries (`lex`, `vec`, `hyde`). First entry receives 2x RRF weight.
    #[serde(default)]
    pub searches: Vec<SubQuery>,
    /// Collections to search (`wiki`, `sources`). Empty = search both.
    #[serde(default)]
    pub collections: Vec<CollectionScope>,
    /// Recursive QMD metadata filter AST.
    pub filter: Option<MetadataFilter>,
    /// Optional disambiguation intent string.
    pub intent: Option<String>,
    /// Max results to return (default: 10).
    pub limit: usize,
    /// Minimum relevance score threshold in `[0.0, 1.0]` (default: 0.0).
    pub min_score: f32,
    /// Max candidates passed from RRF into reranker (default: 40).
    pub candidate_limit: Option<usize>,
    /// Whether to run cross-encoder reranking if a `RerankProvider` is configured (default: true).
    pub rerank: bool,
    /// Attach detailed score breakdown (`SearchExplainTrace`) to each result.
    pub explain: bool,
}

impl Default for HybridQueryRequest {
    fn default() -> Self {
        Self {
            query: None,
            searches: Vec::new(),
            collections: Vec::new(),
            filter: None,
            intent: None,
            limit: 10,
            min_score: 0.0,
            candidate_limit: Some(40),
            rerank: true,
            explain: false,
        }
    }
}

impl HybridQueryRequest {
    pub fn from_query(query: impl Into<String>) -> Self {
        Self {
            query: Some(query.into()),
            ..Default::default()
        }
    }
}

/// A single search result item returned by `search_lex`, `search_vec`, or `query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    /// Short 6-char QMD document ID (e.g. `"#a1b2c3"`).
    pub docid: String,
    /// Vault-relative path (e.g. `"wiki/sources/docs/flow/lib.md"`).
    pub rel_path: String,
    /// Virtual QMD URI (e.g. `"qmd://wiki/sources/docs/flow/lib.md"`).
    pub virtual_uri: String,
    /// Document title.
    pub title: String,
    /// Collection scope (`sources` or `wiki`).
    pub collection: CollectionScope,
    /// Normalized relevance score in `[0.0, 1.0]`.
    pub score: f32,
    /// Raw backend or RRF score prior to final normalization.
    pub raw_backend_score: f32,
    /// 1-indexed line number of the best matching snippet.
    pub match_line: usize,
    /// Contextual snippet around the match.
    pub snippet: String,
    /// Hierarchical path context description if configured.
    pub context: Option<String>,
    /// Extracted QMD metadata map.
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Optional explain trace (`--explain`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<SearchExplainTrace>,
}

/// Detailed retrieval & RRF score trace (`--explain` in QMD).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchExplainTrace {
    pub fts_scores: Vec<f32>,
    pub vector_scores: Vec<f32>,
    pub reranker_score: Option<f32>,
    pub rrf: RrfExplainTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RrfExplainTrace {
    pub rank: usize,
    pub weight: f32,
    pub base_score: f32,
    pub top_rank_bonus: f32,
    pub total_score: f32,
    pub contributions: Vec<RrfContribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RrfContribution {
    pub source: String,
    pub query_type: String,
    pub query: String,
    pub rank: usize,
    pub weight: f32,
    pub backend_score: f32,
    pub rrf_contribution: f32,
}
