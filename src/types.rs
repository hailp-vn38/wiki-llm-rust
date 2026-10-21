use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Logical collection scope within the knowledge base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionScope {
    /// Raw immutable source documents (`sources/`).
    Sources,
    /// LLM-generated and maintained wiki pages (`wiki/`).
    Wiki,
}

impl CollectionScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sources => "sources",
            Self::Wiki => "wiki",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sources" | "source" | "raw" => Some(Self::Sources),
            "wiki" => Some(Self::Wiki),
            _ => None,
        }
    }
}

/// Summary metadata for an indexed Markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    /// Database primary key.
    pub id: i64,
    /// Short 6-character QMD document ID (e.g., `"#a1b2c3"`).
    pub docid: String,
    /// Collection scope (`sources` or `wiki`).
    pub collection: CollectionScope,
    /// Relative path from `data_root` (e.g., `"sources/docs/flow/lib.md"` or `"wiki/sources/docs/flow/lib.md"`).
    pub rel_path: String,
    /// Relative path within its collection (e.g., `"docs/flow/lib.md"` or `"sources/docs/flow/lib.md"`).
    pub scoped_path: String,
    /// File name with extension (e.g., `"lib.md"`).
    pub file_name: String,
    /// File stem without `.md` extension (e.g., `"lib"`), used as node label in Obsidian Graph View.
    pub file_stem: String,
    /// Human-readable document title.
    pub title: String,
    /// Optional wiki category (`source_summary`, `entity`, `concept`, `synthesis`, `system`).
    pub category: Option<String>,
    /// If this is a wiki source summary page, the `rel_path` of the raw source it summarizes.
    pub source_rel_path: Option<String>,
    /// BLAKE3 content hash.
    pub content_hash: String,
    /// File size in bytes.
    pub byte_size: u64,
    /// Number of lines in the file.
    pub line_count: usize,
    /// Last modification timestamp in milliseconds since UNIX epoch.
    pub mtime_ms: i64,
    /// Extracted tags (from YAML frontmatter and inline `#tags`).
    pub tags: Vec<String>,
    /// Extracted QMD typed metadata map.
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Options for retrieving a single document via `get_document`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetOptions {
    /// 1-indexed start line (optional).
    pub from_line: Option<usize>,
    /// Maximum number of lines to return (optional).
    pub max_lines: Option<usize>,
    /// Prefix returned lines with `1: `, `2: ` line numbers (QMD default: `false` in raw API, `true` in tool).
    pub line_numbers: bool,
    /// Include full raw content including YAML frontmatter (default: `true`).
    pub include_frontmatter: bool,
}

impl Default for GetOptions {
    fn default() -> Self {
        Self {
            from_line: None,
            max_lines: None,
            line_numbers: false,
            include_frontmatter: true,
        }
    }
}

/// Full document result returned by `get_document`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentResult {
    pub info: DocumentInfo,
    /// Matching QMD path context description (if configured via `set_path_context`).
    pub context: Option<String>,
    /// Document text content (potentially sliced by `from_line` / `max_lines` and formatted with line numbers).
    pub content: String,
    /// Virtual QMD URI (`qmd://wiki/...` or `qmd://sources/...`).
    pub virtual_uri: String,
}

/// Options for `multi_get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiGetOptions {
    /// Skip files larger than `max_bytes` (default: 65536 = 64KB, matching QMD).
    pub max_bytes: usize,
    /// Optional maximum lines per file.
    pub max_lines: Option<usize>,
    /// Prefix lines with line numbers.
    pub line_numbers: bool,
}

impl Default for MultiGetOptions {
    fn default() -> Self {
        Self {
            max_bytes: 65_536,
            max_lines: None,
            line_numbers: false,
        }
    }
}

/// Batch retrieval result from `multi_get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiGetResult {
    pub docs: Vec<DocumentResult>,
    pub errors: Vec<MultiGetError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiGetError {
    pub target: String,
    pub reason: String,
}

/// QMD Path Context entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathContextItem {
    /// `"global"`, `"sources"`, or `"wiki"`.
    pub scope: String,
    /// Path prefix inside the scope (e.g. `"/"`, `"/docs/flow"`, `"/entities"`).
    pub path_prefix: String,
    /// Descriptive context returned alongside search results.
    pub description: String,
}

/// Result of syncing the filesystem with the SQLite index (`sync_index`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStats {
    pub indexed_new: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub total_chunks: usize,
    pub total_links: usize,
    pub needs_embedding: usize,
}

/// Result of generating embeddings (`embed_pending`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedStats {
    pub documents_embedded: usize,
    pub chunks_embedded: usize,
    pub model_name: String,
    pub dimensions: usize,
}

/// Overall health and statistics of the `WikiEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub data_root: String,
    pub db_path: String,
    pub sources_count: usize,
    pub wiki_count: usize,
    pub total_documents: usize,
    pub total_chunks: usize,
    pub embedded_chunks: usize,
    pub pending_embedding_docs: usize,
    pub total_graph_links: usize,
    pub resolved_graph_links: usize,
    pub unresolved_graph_links: usize,
    pub top_metadata_keys: Vec<(String, usize)>,
    pub contexts: Vec<PathContextItem>,
}
