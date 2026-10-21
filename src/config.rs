use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Strategy for mapping a file in `sources/` to its corresponding summary page in `wiki/`.
/// All variants strictly preserve the exact source file name (Requirement #8),
/// never generating random IDs or hashes as filenames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiSourceLayout {
    /// Places source summaries under `wiki/sources/<source_rel_path>`.
    /// Example: `sources/docs/flow/lib.md` -> `wiki/sources/docs/flow/lib.md` (filename `lib.md` preserved).
    #[default]
    MirrorUnderWikiSources,
    /// Places source summaries directly under `wiki/<source_rel_path>`.
    /// Example: `sources/docs/flow/lib.md` -> `wiki/docs/flow/lib.md` (filename `lib.md` preserved).
    MirrorDirectlyInWiki,
    /// Places source summaries flat under `wiki/sources/<file_name>`.
    /// Example: `sources/docs/flow/lib.md` -> `wiki/sources/lib.md` (filename `lib.md` preserved).
    FlatInWikiSources,
}

/// Configuration for QMD Smart Markdown Chunking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkConfig {
    /// Target chunk size in tokens (QMD default: 900).
    pub target_tokens: usize,
    /// Overlap ratio between consecutive chunks (QMD default: 0.15 = 15%).
    pub overlap_ratio: f32,
    /// Search window in tokens before target cutoff to score break points (QMD default: 200).
    pub window_tokens: usize,
    /// Approximate characters per token used for fast on-device estimation (default: 4).
    pub chars_per_token: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_tokens: 900,
            overlap_ratio: 0.15,
            window_tokens: 200,
            chars_per_token: 4,
        }
    }
}

/// Configuration for `WikiEngine`.
/// Only `data_root` is required (Requirement #4); all subdirectories and database paths
/// are automatically derived inside `data_root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiConfig {
    /// Root directory storing all data (`sources/`, `wiki/`, `SCHEMA.md`, `.wiki/index.sqlite`).
    /// This folder can be opened directly as an Obsidian Vault.
    pub data_root: PathBuf,
    /// Folder name for raw immutable sources (default: `"sources"`).
    pub sources_dir_name: String,
    /// Folder name for LLM-maintained wiki pages (default: `"wiki"`).
    pub wiki_dir_name: String,
    /// File name for the LLM schema instructions at the root (default: `"SCHEMA.md"`).
    pub schema_file_name: String,
    /// Relative path inside `data_root` for the SQLite index database (default: `".wiki/index.sqlite"`).
    pub db_rel_path: PathBuf,
    /// Mapping rule from `sources/` to `wiki/` that preserves the source filename (default: `MirrorUnderWikiSources`).
    pub wiki_source_layout: WikiSourceLayout,
    /// QMD smart chunking parameters.
    pub chunk_config: ChunkConfig,
    /// Automatically initialize `.obsidian/graph.json` color groups for Obsidian Graph View (default: `true`).
    pub init_obsidian_config: bool,
}

impl WikiConfig {
    /// Create a configuration requiring only the root data folder path (Requirement #4).
    pub fn new<P: AsRef<Path>>(data_root: P) -> Self {
        Self {
            data_root: data_root.as_ref().to_path_buf(),
            sources_dir_name: "sources".to_string(),
            wiki_dir_name: "wiki".to_string(),
            schema_file_name: "SCHEMA.md".to_string(),
            db_rel_path: PathBuf::from(".wiki/index.sqlite"),
            wiki_source_layout: WikiSourceLayout::MirrorUnderWikiSources,
            chunk_config: ChunkConfig::default(),
            init_obsidian_config: true,
        }
    }

    /// Builder helper to change the source-to-wiki path mapping layout.
    pub fn with_source_layout(mut self, layout: WikiSourceLayout) -> Self {
        self.wiki_source_layout = layout;
        self
    }

    /// Builder helper to customize chunking parameters.
    pub fn with_chunk_config(mut self, chunk_config: ChunkConfig) -> Self {
        self.chunk_config = chunk_config;
        self
    }

    /// Full path to the `sources/` directory.
    pub fn sources_dir(&self) -> PathBuf {
        self.data_root.join(&self.sources_dir_name)
    }

    /// Full path to the `wiki/` directory.
    pub fn wiki_dir(&self) -> PathBuf {
        self.data_root.join(&self.wiki_dir_name)
    }

    /// Full path to the SQLite database file.
    pub fn db_path(&self) -> PathBuf {
        self.data_root.join(&self.db_rel_path)
    }

    /// Full path to `SCHEMA.md`.
    pub fn schema_path(&self) -> PathBuf {
        self.data_root.join(&self.schema_file_name)
    }

    /// Full path to `wiki/index.md`.
    pub fn index_md_path(&self) -> PathBuf {
        self.wiki_dir().join("index.md")
    }

    /// Full path to `wiki/log.md`.
    pub fn log_md_path(&self) -> PathBuf {
        self.wiki_dir().join("log.md")
    }

    /// Full path to `.obsidian/` directory.
    pub fn obsidian_dir(&self) -> PathBuf {
        self.data_root.join(".obsidian")
    }
}
