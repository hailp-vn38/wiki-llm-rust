use thiserror::Error;

/// Unified error type for `llm-wiki`.
#[derive(Debug, Error)]
pub enum WikiError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML frontmatter error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Glob pattern error: {0}")]
    Glob(#[from] globset::Error),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Path traversal or invalid relative path rejected: '{0}'")]
    PathTraversal(String),

    #[error("Invalid file name or path: '{0}'")]
    InvalidFileName(String),

    #[error("Document not found: '{target}'. Similar files: {similar_files:?}")]
    DocumentNotFound {
        target: String,
        similar_files: Vec<String>,
    },

    #[error("Invalid metadata filter or option: {0}")]
    InvalidMetadataFilter(String),

    #[error("Embedding provider error: {0}")]
    Embedding(String),

    #[error("Reranker provider error: {0}")]
    Rerank(String),

    #[error("Unknown LLM tool: '{0}'")]
    UnknownTool(String),

    #[error("Invalid LLM tool arguments for '{tool}': {message}")]
    InvalidToolArguments { tool: String, message: String },

    #[error("Operation error: {0}")]
    Other(String),
}

pub type WikiResult<T> = Result<T, WikiError>;
