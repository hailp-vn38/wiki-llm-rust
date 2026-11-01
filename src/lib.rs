pub mod config;
pub mod db;
pub mod error;
pub mod fs;
pub mod graph;
pub mod markdown;
pub mod prompts;
pub mod search;
pub mod tools;
pub mod types;
pub mod wiki;

pub use config::{ChunkConfig, WikiConfig, WikiSourceLayout};
pub use db::filter_sql::{
    ComparisonOperator, LogicalOperator, MetadataCondition, MetadataFilter, NotOperator,
};
pub use error::{WikiError, WikiResult};
pub use fs::file_ops::{
    clip_transcript, fold_vietnamese, is_media_url, normalize_media_markdown_links,
    sanitize_mermaid_fences, sanitize_topic_folder, slice_and_format_lines,
    slice_lines_with_budget, slugify, vtt_to_text, AdaptiveReadBudget, DEFAULT_READ_MAX_CHARS,
    DEFAULT_READ_WINDOW_LINES,
};
pub use fs::tree::{FileTreeNode, NodeType, TreeDocMeta, TreeOptions, TreeScope};
pub use graph::analysis::{GraphData, GraphEdge, GraphNode};
pub use markdown::chunker::{smart_chunk_markdown, MarkdownChunk};
pub use markdown::frontmatter::{parse_frontmatter, ParsedFrontmatter, TypedMetaValue};
pub use markdown::parser::{
    parse_markdown_document, ExtractedLink, ExtractedLinkType, HeadingInfo, ParsedMarkdownDoc,
};
pub use prompts::{
    all_wiki_templates, apply_wikilink_substitutions, build_chunk_analysis_system_prompt,
    build_chunk_analysis_user_prompt, build_enrich_wikilinks_system_prompt,
    build_evidence_quality_gate_system_prompt, build_ingest_analysis_prompt,
    build_ingest_batch_generation_prompt, build_ingest_tool_agent_prompt, build_language_directive,
    build_language_reminder, build_page_merge_system_prompt, build_page_merge_user_prompt,
    build_query_agent_system_prompt, build_research_synthesis_prompt, build_semantic_lint_prompt,
    build_truncated_file_repair_prompt, detect_language_prompt_name, get_wiki_template,
    is_safe_ingest_wiki_path, parse_file_blocks, parse_review_blocks, parse_semantic_lint_blocks,
    IngestPromptContext, OutputLanguage, ParseFileBlocksResult, ParsedFileBlock, ParsedReviewBlock,
    ParsedSemanticLintBlock, ReviewBlockType, SemanticLintIssueType, WikiRetrievalMode,
    WikiTemplate, WikiTemplateId, WikilinkSubstitution, ROOT_DISPATCHER_PROMPT,
    SOURCE_COLLECTOR_PROMPT, WIKI_CURATOR_PROMPT, WIKI_INGESTOR_PROMPT,
};
pub use search::metadata::{
    DiscoveredMetadataKey, DiscoveredMetadataTypeBucket, DiscoveredValueCount,
    MetadataDiscoveryRequest, MetadataDiscoveryResult, MetadataSortOrder, NumericRangeStats,
};
pub use search::vector::{EmbeddingProvider, LocalNgramEmbedder, RerankProvider};
pub use search::{
    ExpandedQuery, HybridQueryRequest, RrfContribution, RrfExplainTrace, SearchExplainTrace,
    SearchOptions, SearchResultItem, SubQuery, SubQueryType,
};
pub use tools::definitions::{is_read_only_wiki_tool, LlmToolSpec};
pub use tools::executor::{execute_tool_call_with_budget, LlmToolOutcome};
pub use types::{
    CollectionScope, DocumentInfo, DocumentResult, EmbedStats, EngineStatus, GetOptions,
    MultiGetError, MultiGetOptions, MultiGetResult, PathContextItem, SyncStats,
};
pub use wiki::ingest::{
    map_source_to_wiki_scoped_path, render_topic_source_markdown, FileSynthesisRequest,
    IngestSourceRequest, IngestSourceResponse, WriteTopicSourceRequest, WriteTopicSourceResponse,
};
pub use wiki::linter::{
    BrokenLinkItem, FlaggedClaimItem, IncompleteIngestItem, StaleWikiPageItem,
    UningestedSourceItem, UnlinkedMentionItem, WikiLintReport,
};
pub use wiki::log_md::{LogAction, LogEntry};

use crate::db::index_store::{
    get_all_path_contexts, index_single_file, load_tree_metadata_map, multi_get_documents,
    remove_document_by_rel_path, resolve_canonical_wiki_scoped_path, retrieve_document,
    sync_filesystem, upsert_path_context,
};
use crate::db::schema::init_database;
use crate::fs::file_ops::{delete_nested_file, move_nested_file, write_nested_file};
use crate::fs::path_guard::ensure_markdown_extension;
use crate::fs::tree::build_directory_tree;
use crate::graph::analysis::{
    build_graph_data, get_backlinks_for_doc, get_outgoing_links_for_doc, init_obsidian_graph_config,
};
use crate::graph::link_resolver::resolve_single_wikilink;
use crate::search::fts::search_fts_bm25;
use crate::search::metadata::discover_metadata;
use crate::search::rrf::execute_hybrid_query;
use crate::search::vector::{embed_pending_documents, search_vector};
use crate::tools::definitions::build_llm_tool_definitions;
use crate::tools::executor::execute_tool_call;
use crate::wiki::index_md::{
    extract_topic_from_doc, find_unsynthesized_topics, generate_compact_index_md_for_topic,
    generate_index_md_content, DEFAULT_COMPACT_INDEX_MAX_CHARS, DEFAULT_SCHEMA_MD,
};
use crate::wiki::ingest::{
    normalize_source_scoped_path, render_source_wiki_markdown, render_synthesis_markdown,
};
use crate::wiki::linter::run_wiki_lint;
use crate::wiki::log_md::{append_entry_to_log_file, parse_log_entries, DEFAULT_LOG_MD_HEADER};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Options for [`WikiEngine::plan_next_autoflow`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoflowPlanOptions<'a> {
    /// Maximum sources handed to one stage; `None` hands over all of them.
    pub max_sources: Option<usize>,
    /// Quarantined `sources/...` paths: neither planned nor counted as unhealthy.
    pub exclude_sources: &'a [String],
}

/// The next Auto-Flow stage: its prompt and the `sources/...` paths it is responsible for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoflowPlan {
    pub prompt: String,
    pub ingest_batch: Vec<String>,
}

/// How far a raw source has progressed into `wiki/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceIngestState {
    NotIngested,
    /// A summary page exists, but pages it announces are missing or the source changed since.
    Incomplete,
    Ingested,
}

/// A raw source found by [`WikiEngine::find_source_by_frontmatter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMatch {
    pub rel_path: String,
    pub scoped_path: String,
    pub ingest_state: SourceIngestState,
}

/// Main entry point for the `llm-wiki` library.
///
/// Combines Andrej Karpathy's **LLM Wiki** architecture with **QMD**'s hybrid markdown search
/// and **Obsidian Graph View** compatibility.
pub struct WikiEngine {
    config: WikiConfig,
    db: Mutex<Connection>,
    embedder: Arc<dyn EmbeddingProvider>,
    reranker: Option<Arc<dyn RerankProvider>>,
}

impl WikiEngine {
    // =========================================================================
    // 1. INITIALIZATION & CONFIGURATION (Requirement #4)
    // =========================================================================

    /// Open or initialize the knowledge base requiring only a single `data_root` directory path (Requirement #4).
    ///
    /// Automatically creates:
    /// - `<data_root>/sources/` (and `sources/assets/`)
    /// - `<data_root>/wiki/` (`wiki/sources/`, `wiki/entities/`, `wiki/concepts/`, `wiki/syntheses/`, `wiki/index.md`, `wiki/log.md`)
    /// - `<data_root>/SCHEMA.md`
    /// - `<data_root>/.wiki/index.sqlite`
    /// - `<data_root>/.obsidian/graph.json` (color groups for Obsidian Graph View)
    pub fn open<P: AsRef<Path>>(data_root: P) -> WikiResult<Self> {
        Self::open_with_config(WikiConfig::new(data_root))
    }

    /// Open or initialize the knowledge base with a custom `WikiConfig`.
    pub fn open_with_config(config: WikiConfig) -> WikiResult<Self> {
        // 1. Create standard directory tree
        std::fs::create_dir_all(config.sources_dir().join("assets"))?;
        std::fs::create_dir_all(config.wiki_dir().join("sources"))?;
        std::fs::create_dir_all(config.wiki_dir().join("entities"))?;
        std::fs::create_dir_all(config.wiki_dir().join("concepts"))?;
        std::fs::create_dir_all(config.wiki_dir().join("syntheses"))?;

        let db_path = config.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if config.init_obsidian_config {
            init_obsidian_graph_config(&config.obsidian_dir())?;
        }

        // 2. Initialize default SCHEMA.md if missing
        let schema_path = config.schema_path();
        if !schema_path.exists() {
            std::fs::write(&schema_path, DEFAULT_SCHEMA_MD)?;
        }

        // 3. Open SQLite connection & initialize schema
        let conn = Connection::open(&db_path)?;
        init_database(&conn)?;

        // 4. Initialize `wiki/log.md` and `wiki/index.md` if missing
        let log_path = config.log_md_path();
        let is_first_init = !log_path.exists();
        if is_first_init {
            std::fs::write(&log_path, DEFAULT_LOG_MD_HEADER)?;
            append_entry_to_log_file(
                &log_path,
                LogAction::Init,
                "Knowledge base initialized",
                &[
                    "Created directory structure (`sources/`, `wiki/`, `.wiki/index.sqlite`)."
                        .to_string(),
                ],
            )?;
        }

        let index_path = config.index_md_path();
        if !index_path.exists() {
            let initial_index = generate_index_md_content(&conn, &config, &HashMap::new())?;
            std::fs::write(&index_path, initial_index)?;
        }

        // 5. Sync existing files on disk into SQLite
        sync_filesystem(&conn, &config)?;

        Ok(Self {
            config,
            db: Mutex::new(conn),
            embedder: Arc::new(LocalNgramEmbedder::default()),
            reranker: None,
        })
    }

    /// Attach a custom `EmbeddingProvider` (e.g. GGUF / FastEmbed / OpenAI / Ollama).
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = embedder;
        self
    }

    /// Attach a custom `RerankProvider` (e.g. Qwen3-Reranker / Cross-Encoder).
    pub fn with_reranker(mut self, reranker: Arc<dyn RerankProvider>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Return a reference to the current `WikiConfig`.
    pub fn config(&self) -> &WikiConfig {
        &self.config
    }

    /// Apply a built-in domain `WikiTemplateId` (`Research`, `Personal`, `Team`, `Course`, `Book`)
    /// by writing its `SCHEMA.md` and registering the 5 standard QMD path contexts
    /// (`sources:/`, `wiki:/sources`, `wiki:/entities`, `wiki:/concepts`, `wiki:/syntheses`).
    pub fn apply_template(&self, template_id: WikiTemplateId) -> WikiResult<WikiTemplate> {
        let template = get_wiki_template(template_id);
        self.set_schema_md(&template.schema_md)?;
        self.set_path_context(
            Some(CollectionScope::Sources),
            "/",
            "Raw immutable source documents organized by topic folder (<domain>/<topic>)",
        )?;
        self.set_path_context(
            Some(CollectionScope::Wiki),
            "/sources",
            "Compiled source summary wiki pages preserving raw source filenames and topic folders",
        )?;
        self.set_path_context(
            Some(CollectionScope::Wiki),
            "/entities",
            "Canonical entity pages (people, platforms, languages, tools, datasets)",
        )?;
        self.set_path_context(
            Some(CollectionScope::Wiki),
            "/concepts",
            "Canonical concept pages (strategies, algorithms, architectural patterns, methods)",
        )?;
        self.set_path_context(
            Some(CollectionScope::Wiki),
            "/syntheses",
            "Cross-source comparative syntheses, tables, and knowledge maps",
        )?;
        Ok(template)
    }

    // =========================================================================
    // 2. FILE & NESTED FOLDER CRUD + TREE API (Requirements #3, #6, #7)
    // =========================================================================

    /// Create or overwrite a Markdown file inside `sources/`.
    ///
    /// **Requirement #6**: Supports arbitrarily nested folder paths like `"docs/flow/lib.md"` —
    /// automatically creates `sources/docs/` and `sources/docs/flow/`, writes `lib.md`, and indexes it.
    pub fn write_source_file(&self, rel_path: &str, content: &str) -> WikiResult<DocumentInfo> {
        self.write_file(CollectionScope::Sources, rel_path, content)
    }

    /// Write a standardized Raw Source Markdown file organized by `<domain>/<topic>` folder
    /// into `sources/<topic_folder>/<file_stem>.md` with YAML frontmatter + `qmd.metadata`,
    /// returning both the indexed `DocumentInfo` and a compact agent preview (`WriteTopicSourceResponse`).
    pub fn write_topic_source(
        &self,
        req: WriteTopicSourceRequest,
    ) -> WikiResult<WriteTopicSourceResponse> {
        let (raw_md, scoped_path, topic_folder, domain, body_char_count, body_line_count) =
            render_topic_source_markdown(&req);
        let topic = topic_folder
            .split('/')
            .nth(1)
            .unwrap_or(&domain)
            .to_string();
        let info = self.write_source_file(&scoped_path, &raw_md)?;
        let expected_wiki_path =
            format!("{}/sources/{}", self.config.wiki_dir_name, info.scoped_path);
        let preview_limit = req.preview_chars.filter(|&n| n > 0).unwrap_or(600);
        let short_preview = clip_transcript(&req.raw_body, preview_limit);
        let agent_output = format!(
            "RAW_SOURCE_SAVED: {}\nSOURCE_SCOPED_PATH: {}\nEXPECTED_WIKI_PATH: {}\nTOPIC_FOLDER: {}\nDOCID: {}\nTITLE: {}\nCONTENT_LENGTH_CHARS: {}\nCONTENT_LINES: {}\n\n=== BEGIN SHORT PREVIEW ===\n{}\n=== END SHORT PREVIEW ===",
            info.rel_path,
            info.scoped_path,
            expected_wiki_path,
            topic_folder,
            info.docid,
            req.title,
            body_char_count,
            info.line_count,
            short_preview
        );
        Ok(WriteTopicSourceResponse {
            info,
            topic_folder,
            domain,
            topic,
            expected_wiki_path,
            body_char_count,
            body_line_count,
            agent_output,
        })
    }

    /// Create or overwrite a Markdown file inside `wiki/`.
    ///
    /// **Requirement #6**: Supports arbitrarily nested folder paths like `"concepts/search/bm25.md"` —
    /// automatically creates `wiki/concepts/` and `wiki/concepts/search/`, writes `bm25.md`, and indexes it.
    pub fn write_wiki_file(&self, rel_path: &str, content: &str) -> WikiResult<DocumentInfo> {
        self.write_file(CollectionScope::Wiki, rel_path, content)
    }

    /// Create or overwrite a Markdown file inside the specified collection (`Sources` or `Wiki`),
    /// automatically creating all intermediate directories (Requirement #6) and indexing into SQLite.
    pub fn write_file(
        &self,
        scope: CollectionScope,
        rel_path: &str,
        content: &str,
    ) -> WikiResult<DocumentInfo> {
        let md_rel_path = ensure_markdown_extension(rel_path)?;
        let base_dir = match scope {
            CollectionScope::Sources => self.config.sources_dir(),
            CollectionScope::Wiki => self.config.wiki_dir(),
        };

        // Strip redundant collection prefix if caller passed `"sources/docs/flow/lib.md"` with `CollectionScope::Sources`
        let prefix = format!("{}/", scope.as_str());
        let clean_scoped_input = md_rel_path.strip_prefix(&prefix).unwrap_or(&md_rel_path);

        let sanitized = sanitize_mermaid_fences(content);
        let conn = self.db.lock().unwrap();
        let target_scoped = if scope == CollectionScope::Wiki {
            resolve_canonical_wiki_scoped_path(&conn, clean_scoped_input, &sanitized)?
        } else {
            clean_scoped_input.to_string()
        };
        let (clean_scoped, full_path) = write_nested_file(&base_dir, &target_scoped, &sanitized)?;

        let (info, _) =
            index_single_file(&conn, &self.config, scope, &clean_scoped, &full_path, true)?;
        Ok(info)
    }

    /// Retrieve a single document by its relative path, file stem, or QMD `#docid` (e.g. `"#a1b2c3"`),
    /// with optional line slicing (`from_line`, `max_lines`) and `:from:count` suffix support.
    pub fn get_document(
        &self,
        path_or_docid: &str,
        opts: GetOptions,
    ) -> WikiResult<DocumentResult> {
        let conn = self.db.lock().unwrap();
        retrieve_document(&conn, &self.config, path_or_docid, &opts)
    }

    /// Batch retrieve multiple documents matching a glob pattern (e.g. `"wiki/entities/**/*.md"`)
    /// or a comma-separated list of paths / `#docids`.
    pub fn multi_get(
        &self,
        pattern_or_list: &str,
        opts: MultiGetOptions,
    ) -> WikiResult<MultiGetResult> {
        let conn = self.db.lock().unwrap();
        multi_get_documents(&conn, &self.config, pattern_or_list, &opts)
    }

    /// Delete a file from `sources` or `wiki`, pruning empty parent folders and removing it from SQLite & Graph.
    pub fn delete_file(&self, scope: CollectionScope, rel_path: &str) -> WikiResult<bool> {
        let md_rel = ensure_markdown_extension(rel_path)?;
        let prefix = format!("{}/", scope.as_str());
        let scoped = md_rel.strip_prefix(&prefix).unwrap_or(&md_rel);

        let (base_dir, dir_name) = match scope {
            CollectionScope::Sources => (self.config.sources_dir(), &self.config.sources_dir_name),
            CollectionScope::Wiki => (self.config.wiki_dir(), &self.config.wiki_dir_name),
        };

        let deleted_disk = delete_nested_file(&base_dir, scoped)?;
        let vault_rel = format!("{dir_name}/{scoped}");
        let conn = self.db.lock().unwrap();
        let deleted_db = remove_document_by_rel_path(&conn, &vault_rel)?;
        Ok(deleted_disk || deleted_db)
    }

    /// Delete a Markdown page inside `wiki/`, pruning empty parent folders, removing it from SQLite & Graph,
    /// and syncing `wiki/index.md`.
    pub fn delete_wiki_file(&self, rel_path: &str) -> WikiResult<bool> {
        let deleted = self.delete_file(CollectionScope::Wiki, rel_path)?;
        if deleted {
            let _ = self.rebuild_or_sync_index_md();
        }
        Ok(deleted)
    }

    /// Delete a raw source Markdown file inside `sources/`, pruning empty parent folders and removing it from SQLite.
    pub fn delete_source_file(&self, rel_path: &str) -> WikiResult<bool> {
        self.delete_file(CollectionScope::Sources, rel_path)
    }

    /// Move or rename a file within `sources` or `wiki`, automatically creating nested target directories
    /// and re-indexing links.
    pub fn move_file(
        &self,
        scope: CollectionScope,
        from_rel: &str,
        to_rel: &str,
    ) -> WikiResult<DocumentInfo> {
        let from_md = ensure_markdown_extension(from_rel)?;
        let to_md = ensure_markdown_extension(to_rel)?;
        let prefix = format!("{}/", scope.as_str());
        let from_scoped = from_md.strip_prefix(&prefix).unwrap_or(&from_md);
        let to_scoped = to_md.strip_prefix(&prefix).unwrap_or(&to_md);

        let (base_dir, dir_name) = match scope {
            CollectionScope::Sources => (self.config.sources_dir(), &self.config.sources_dir_name),
            CollectionScope::Wiki => (self.config.wiki_dir(), &self.config.wiki_dir_name),
        };

        let (clean_to_scoped, to_full_path) = move_nested_file(&base_dir, from_scoped, to_scoped)?;
        let old_vault_rel = format!("{dir_name}/{from_scoped}");

        let conn = self.db.lock().unwrap();
        remove_document_by_rel_path(&conn, &old_vault_rel)?;
        let (info, _) = index_single_file(
            &conn,
            &self.config,
            scope,
            &clean_to_scoped,
            &to_full_path,
            true,
        )?;
        Ok(info)
    }

    /// Retrieve the hierarchical directory tree of `sources`, `wiki`, or `all` (Requirement #7).
    pub fn get_tree(&self, opts: TreeOptions) -> WikiResult<FileTreeNode> {
        let meta_map = {
            let conn = self.db.lock().unwrap();
            load_tree_metadata_map(&conn)?
        };
        build_directory_tree(&self.config, &opts, &meta_map)
    }

    // =========================================================================
    // 3. KARPATHY LLM WIKI WORKFLOW OPERATIONS (Requirement #8)
    // =========================================================================

    /// Resolve the target wiki path (relative to `data_root` and relative to `wiki/`) for a source file,
    /// strictly preserving the exact filename from `sources/` (Requirement #8).
    pub fn resolve_wiki_path_for_source(
        &self,
        source_rel_path: &str,
    ) -> WikiResult<(String, String, String)> {
        let (wiki_scoped, file_name) =
            map_source_to_wiki_scoped_path(source_rel_path, &self.config)?;
        let wiki_vault_rel = format!("{}/{wiki_scoped}", self.config.wiki_dir_name);
        Ok((wiki_vault_rel, wiki_scoped, file_name))
    }

    /// Ingest a raw source from `sources/` into `wiki/`.
    ///
    /// **Requirement #8**: The generated wiki page filename is ALWAYS the exact filename of the source
    /// (e.g., `sources/docs/flow/lib.md` -> `wiki/sources/docs/flow/lib.md` with filename `lib.md`),
    /// never an auto-generated ID.
    ///
    /// Also:
    /// - Creates nested folders automatically (Requirement #6)
    /// - Adds Obsidian YAML frontmatter linking back to `[[sources/...|lib.md]]` (Requirement #2)
    /// - Updates `wiki/index.md`
    /// - Appends an ingest record to `wiki/log.md`
    pub fn ingest_source_to_wiki(
        &self,
        req: IngestSourceRequest,
    ) -> WikiResult<IngestSourceResponse> {
        let clean_source_scoped =
            normalize_source_scoped_path(&req.source_rel_path, &self.config.sources_dir_name)?;
        let vault_source_rel = format!("{}/{clean_source_scoped}", self.config.sources_dir_name);

        // If caller provided `raw_source_content`, write/update the raw source file first
        let source_full_path = self.config.sources_dir().join(&clean_source_scoped);
        if let Some(ref raw_src) = req.raw_source_content {
            self.write_source_file(&clean_source_scoped, raw_src)?;
        } else if !source_full_path.exists() {
            return Err(WikiError::DocumentNotFound {
                target: vault_source_rel,
                similar_files: Vec::new(),
            });
        }

        let (wiki_scoped_path, file_name) = if let Some(ref custom_target) = req
            .target_wiki_path
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            let clean_target = ensure_markdown_extension(custom_target)?;
            let prefix = format!("{}/", self.config.wiki_dir_name);
            let scoped = clean_target
                .strip_prefix(&prefix)
                .unwrap_or(&clean_target)
                .to_string();
            let fname = Path::new(&scoped)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&scoped)
                .to_string();
            (scoped, fname)
        } else {
            map_source_to_wiki_scoped_path(&clean_source_scoped, &self.config)?
        };

        let source_file_name = Path::new(&clean_source_scoped)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file_name)
            .to_string();

        // Check if target wiki file already exists on disk so we preserve its previously linked `sources:`
        let target_full_path = self.config.wiki_dir().join(&wiki_scoped_path);
        let mut previous_sources = Vec::new();
        if target_full_path.exists() {
            if let Ok(existing_md) = std::fs::read_to_string(&target_full_path) {
                let fm = parse_frontmatter(&existing_md);
                for raw_src in fm.source_files {
                    let clean_prev = raw_src
                        .trim()
                        .strip_prefix("[[")
                        .and_then(|s| s.strip_suffix("]]"))
                        .unwrap_or(raw_src.trim())
                        .split('|')
                        .next()
                        .unwrap_or("")
                        .split('#')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .replace('\\', "/");
                    if !clean_prev.is_empty() {
                        previous_sources.push(clean_prev);
                    }
                }
            }
        }

        let formatted_wiki_md = render_source_wiki_markdown(
            &req,
            &vault_source_rel,
            &source_file_name,
            &previous_sources,
        );

        let wiki_doc_info = self.write_wiki_file(&wiki_scoped_path, &formatted_wiki_md)?;

        // Refresh `wiki/index.md`
        let mut overrides = HashMap::new();
        overrides.insert(
            wiki_doc_info.rel_path.clone(),
            req.one_line_summary.trim().to_string(),
        );
        self.rebuild_index_md_with_overrides(&overrides)?;

        // Append chronological entry to `wiki/log.md`
        let mut log_details = vec![
            format!("Source: `{vault_source_rel}`"),
            format!(
                "Wiki page: `{}` (filename preserved: `{file_name}`)",
                wiki_doc_info.rel_path
            ),
        ];
        if !req.related_entities.is_empty() {
            log_details.push(format!(
                "Entities linked: {}",
                req.related_entities.join(", ")
            ));
        }
        if !req.related_concepts.is_empty() {
            log_details.push(format!(
                "Concepts linked: {}",
                req.related_concepts.join(", ")
            ));
        }
        self.append_log(LogAction::Ingest, &clean_source_scoped, &log_details)?;

        Ok(IngestSourceResponse {
            source_rel_path: vault_source_rel,
            wiki_scoped_path,
            wiki_rel_path: wiki_doc_info.rel_path,
            file_name,
            wiki_docid: wiki_doc_info.docid,
        })
    }

    /// Apply a batch of Stage 2 LLM `---FILE: wiki/...---` blocks directly into the `WikiEngine`,
    /// automatically validating paths, indexing all written pages into SQLite + Obsidian Graph View,
    /// refreshing `wiki/index.md`, and appending an ingest record to `wiki/log.md`.
    pub fn apply_ingest_file_blocks(
        &self,
        source_rel_path: &str,
        raw_llm_output: &str,
    ) -> WikiResult<(ParseFileBlocksResult, Vec<String>)> {
        let parsed = parse_file_blocks(raw_llm_output);
        let mut written_rel_paths = Vec::new();

        for block in &parsed.blocks {
            let scoped = block.path.strip_prefix("wiki/").unwrap_or(&block.path);
            if scoped == "index.md" || scoped == "log.md" {
                continue;
            }
            let info = self.write_wiki_file(scoped, &block.content)?;
            written_rel_paths.push(info.rel_path);
        }

        if !written_rel_paths.is_empty() {
            self.rebuild_or_sync_index_md()?;
            let details: Vec<String> = written_rel_paths
                .iter()
                .map(|p| format!("Wrote wiki page: `{p}`"))
                .collect();
            self.append_log(LogAction::Ingest, source_rel_path, &details)?;
        }

        Ok((parsed, written_rel_paths))
    }

    /// File a valuable query answer / synthesis back into `wiki/syntheses/`
    /// (Karpathy's Query operation: "good answers can be filed back into the wiki as new pages").
    pub fn file_query_synthesis(&self, req: FileSynthesisRequest) -> WikiResult<DocumentInfo> {
        let clean_rel = ensure_markdown_extension(&req.rel_path)?;
        let prefix = format!("{}/", self.config.wiki_dir_name);
        let stripped = clean_rel.strip_prefix(&prefix).unwrap_or(&clean_rel);
        let target_scoped = if stripped.starts_with("syntheses/") {
            stripped.to_string()
        } else {
            format!("syntheses/{stripped}")
        };

        let formatted_md = render_synthesis_markdown(&req);
        let info = self.write_wiki_file(&target_scoped, &formatted_md)?;

        let mut overrides = HashMap::new();
        overrides.insert(
            info.rel_path.clone(),
            req.one_line_summary.trim().to_string(),
        );
        self.rebuild_index_md_with_overrides(&overrides)?;

        self.append_log(
            LogAction::Query,
            &req.query,
            &[
                format!("Filed synthesis page: `{}`", info.rel_path),
                format!("Title: {}", req.title),
            ],
        )?;

        Ok(info)
    }

    /// Read the contents of `wiki/index.md`.
    pub fn read_index_md(&self) -> WikiResult<String> {
        let p = self.config.index_md_path();
        if p.exists() {
            Ok(std::fs::read_to_string(p)?)
        } else {
            self.rebuild_or_sync_index_md()
        }
    }

    /// Generate a compact, token-budgeted Wiki Index (`slug -> [[path|Title]] [aliases]`)
    /// inspired by `javis-os` `_read_index`, capping output at `max_chars` (or `DEFAULT_COMPACT_INDEX_MAX_CHARS` if 0).
    pub fn read_compact_index_md(&self, max_chars: usize) -> WikiResult<String> {
        self.read_compact_index_md_for_topic(max_chars, None)
    }

    /// Generate a topic-prioritized compact Wiki Index for Multi-Topic Vaults:
    /// guarantees pages belonging to `preferred_topic` (or the latest active `<domain>/<topic>` if `None`)
    /// are listed first before filling remaining `max_chars` budget with other topics.
    pub fn read_compact_index_md_for_topic(
        &self,
        max_chars: usize,
        preferred_topic: Option<&str>,
    ) -> WikiResult<String> {
        let budget = if max_chars == 0 {
            DEFAULT_COMPACT_INDEX_MAX_CHARS
        } else {
            max_chars
        };
        let conn = self.db.lock().unwrap();
        generate_compact_index_md_for_topic(&conn, &self.config, budget, preferred_topic)
    }

    /// Return all `<domain>/<topic>` folders under `wiki/sources/` that do not yet have a corresponding
    /// synthesis page in `wiki/syntheses/`.
    pub fn unsynthesized_topics(&self) -> WikiResult<Vec<String>> {
        let conn = self.db.lock().unwrap();
        find_unsynthesized_topics(&conn)
    }

    /// Rebuild and index `wiki/index.md` from all currently indexed wiki pages.
    pub fn rebuild_or_sync_index_md(&self) -> WikiResult<String> {
        self.rebuild_index_md_with_overrides(&HashMap::new())
    }

    fn rebuild_index_md_with_overrides(
        &self,
        overrides: &HashMap<String, String>,
    ) -> WikiResult<String> {
        let content = {
            let conn = self.db.lock().unwrap();
            generate_index_md_content(&conn, &self.config, overrides)?
        };
        self.write_wiki_file("index.md", &content)?;
        Ok(content)
    }

    /// Append a chronological entry (`## [YYYY-MM-DD HH:MM] <action> | <title>`) to `wiki/log.md` and re-index it.
    pub fn append_log(&self, action: LogAction, title: &str, details: &[String]) -> WikiResult<()> {
        let log_path = self.config.log_md_path();
        append_entry_to_log_file(&log_path, action, title, details)?;
        let conn = self.db.lock().unwrap();
        let _ = index_single_file(
            &conn,
            &self.config,
            CollectionScope::Wiki,
            "log.md",
            &log_path,
            false,
        )?;
        Ok(())
    }

    /// Read the most recent `limit` entries from `wiki/log.md`.
    pub fn read_recent_logs(&self, limit: usize) -> WikiResult<Vec<LogEntry>> {
        let log_path = self.config.log_md_path();
        if !log_path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(log_path)?;
        Ok(parse_log_entries(&raw, limit))
    }

    /// Read `SCHEMA.md`.
    pub fn get_schema_md(&self) -> WikiResult<String> {
        Ok(std::fs::read_to_string(self.config.schema_path())?)
    }

    /// Update `SCHEMA.md`.
    pub fn set_schema_md(&self, content: &str) -> WikiResult<()> {
        std::fs::write(self.config.schema_path(), content)?;
        Ok(())
    }

    /// Run Karpathy's Wiki Lint health-check (`uningested_sources`, `broken_links`, `orphan_wiki_pages`, `unlinked_mentions`, `missing_frontmatter_pages`).
    pub fn lint_wiki(&self) -> WikiResult<WikiLintReport> {
        let conn = self.db.lock().unwrap();
        run_wiki_lint(&conn, &self.config)
    }

    // =========================================================================
    // 4. QMD SEARCH & INDEXING API (Requirement #1)
    // =========================================================================

    /// Rescan `sources/` and `wiki/` on disk and synchronize all changes with SQLite.
    pub fn sync_index(&self) -> WikiResult<SyncStats> {
        let conn = self.db.lock().unwrap();
        sync_filesystem(&conn, &self.config)
    }

    /// Generate vector embeddings for all pending chunks (or all chunks if `force_all == true`).
    pub fn embed_pending(&self, force_all: bool) -> WikiResult<EmbedStats> {
        let conn = self.db.lock().unwrap();
        embed_pending_documents(&conn, self.embedder.as_ref(), force_all)
    }

    /// Fast lexical keyword search using SQLite FTS5 BM25 (`qmd search`).
    pub fn search_lex(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> WikiResult<Vec<SearchResultItem>> {
        let conn = self.db.lock().unwrap();
        search_fts_bm25(&conn, query, &opts)
    }

    /// Vector semantic similarity search (`qmd vsearch`).
    pub fn search_vec(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> WikiResult<Vec<SearchResultItem>> {
        let conn = self.db.lock().unwrap();
        search_vector(&conn, self.embedder.as_ref(), query, &opts)
    }

    /// Full QMD Hybrid Search (`qmd query`): combines typed sub-queries (`lex`, `vec`, `hyde`),
    /// Reciprocal Rank Fusion ($k = 60$, $2\times$ weight for first query, top-rank bonus),
    /// Metadata AST Filtering, and Position-Aware Reranker Blending.
    pub fn query(&self, req: HybridQueryRequest) -> WikiResult<Vec<SearchResultItem>> {
        let conn = self.db.lock().unwrap();
        let reranker_ref = self.reranker.as_deref();
        execute_hybrid_query(&conn, self.embedder.as_ref(), reranker_ref, &req)
    }

    /// Set a QMD Path Context description for a collection path prefix (e.g. `scope = Some(Wiki)`, `path_prefix = "/entities"`).
    /// Pass `scope = None` for global context across all collections.
    pub fn set_path_context(
        &self,
        scope: Option<CollectionScope>,
        path_prefix: &str,
        description: &str,
    ) -> WikiResult<()> {
        let scope_str = scope.map(|s| s.as_str()).unwrap_or("global");
        let conn = self.db.lock().unwrap();
        upsert_path_context(&conn, scope_str, path_prefix, description)
    }

    /// List all configured QMD Path Contexts.
    pub fn list_path_contexts(&self) -> WikiResult<Vec<PathContextItem>> {
        let conn = self.db.lock().unwrap();
        get_all_path_contexts(&conn)
    }

    /// Discover indexed YAML frontmatter metadata keys, value types, and document counts (`qmd metadata`).
    pub fn discover_metadata(
        &self,
        req: MetadataDiscoveryRequest,
    ) -> WikiResult<MetadataDiscoveryResult> {
        let conn = self.db.lock().unwrap();
        discover_metadata(&conn, &req)
    }

    /// Get index health, document counts, embedding status, and Obsidian graph link statistics (`qmd status`).
    pub fn status(&self) -> WikiResult<EngineStatus> {
        let conn = self.db.lock().unwrap();

        let sources_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM documents WHERE collection = 'sources'",
            [],
            |r| r.get(0),
        )?;
        let wiki_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM documents WHERE collection = 'wiki'",
            [],
            |r| r.get(0),
        )?;
        let total_chunks: i64 =
            conn.query_row("SELECT COUNT(*) FROM content_chunks", [], |r| r.get(0))?;
        let embedded_chunks: i64 =
            conn.query_row("SELECT COUNT(*) FROM chunk_vectors", [], |r| r.get(0))?;
        let pending_embedding_docs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM documents WHERE needs_embedding = 1",
            [],
            |r| r.get(0),
        )?;
        let total_graph_links: i64 =
            conn.query_row("SELECT COUNT(*) FROM graph_links", [], |r| r.get(0))?;
        let resolved_graph_links: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_links WHERE resolved_doc_id IS NOT NULL",
            [],
            |r| r.get(0),
        )?;

        let mut meta_stmt = conn.prepare(
            "SELECT meta_key, COUNT(DISTINCT document_id) AS cnt
             FROM document_metadata
             GROUP BY meta_key
             ORDER BY cnt DESC, meta_key ASC
             LIMIT 10",
        )?;
        let top_metadata_keys: Vec<(String, usize)> = meta_stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let contexts = get_all_path_contexts(&conn)?;

        Ok(EngineStatus {
            data_root: self.config.data_root.to_string_lossy().to_string(),
            db_path: self.config.db_path().to_string_lossy().to_string(),
            sources_count: sources_count as usize,
            wiki_count: wiki_count as usize,
            total_documents: (sources_count + wiki_count) as usize,
            total_chunks: total_chunks as usize,
            embedded_chunks: embedded_chunks as usize,
            pending_embedding_docs: pending_embedding_docs as usize,
            total_graph_links: total_graph_links as usize,
            resolved_graph_links: resolved_graph_links as usize,
            unresolved_graph_links: (total_graph_links - resolved_graph_links).max(0) as usize,
            top_metadata_keys,
            contexts,
        })
    }

    // =========================================================================
    // 5. OBSIDIAN GRAPH VIEW API (Requirement #2)
    // =========================================================================

    /// Get the full Obsidian Graph View dataset (Nodes, Edges, Unresolved targets).
    pub fn get_graph(&self, filter_collection: Option<CollectionScope>) -> WikiResult<GraphData> {
        let conn = self.db.lock().unwrap();
        build_graph_data(&conn, filter_collection)
    }

    /// Get all incoming links (Backlinks) pointing to `rel_path`.
    pub fn get_backlinks(&self, rel_path: &str) -> WikiResult<Vec<GraphEdge>> {
        let conn = self.db.lock().unwrap();
        get_backlinks_for_doc(&conn, rel_path)
    }

    /// Get all outgoing links (`[[WikiLinks]]`, embeds, internal links) starting from `rel_path`.
    pub fn get_outgoing_links(&self, rel_path: &str) -> WikiResult<Vec<GraphEdge>> {
        let conn = self.db.lock().unwrap();
        get_outgoing_links_for_doc(&conn, rel_path)
    }

    /// Resolve an Obsidian `[[WikiLink]]` target string to its target `DocumentInfo`.
    pub fn resolve_wikilink(
        &self,
        wikilink_target: &str,
        from_rel_path: Option<&str>,
    ) -> WikiResult<Option<DocumentInfo>> {
        let conn = self.db.lock().unwrap();
        resolve_single_wikilink(&conn, wikilink_target, from_rel_path)
    }

    // =========================================================================
    // 6. LLM TOOL CALLING & REACTIVE AUTO-FLOW API (Requirement #5)
    // =========================================================================

    /// Return JSON Schema tool definitions for all 13 built-in LLM tools.
    pub fn llm_tool_definitions(&self) -> Vec<LlmToolSpec> {
        build_llm_tool_definitions()
    }

    /// Execute a named LLM tool with JSON arguments and return a JSON value result.
    pub fn execute_llm_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> WikiResult<serde_json::Value> {
        execute_tool_call(self, tool_name, arguments)
    }

    /// Execute a named LLM tool with an `AdaptiveReadBudget` and return a compact `LlmToolOutcome`
    /// ready to plug into any LLM agent/subagent tool loop.
    pub fn execute_llm_tool_with_budget(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        read_budget: AdaptiveReadBudget,
    ) -> WikiResult<LlmToolOutcome> {
        execute_tool_call_with_budget(self, tool_name, arguments, read_budget)
    }

    /// Inspect the live state of `sources/` and `wiki/` on disk to compute the next reactive
    /// Auto-Flow dispatcher prompt without relying on prior session conversation history:
    /// - If `sources/` is empty, returns `initial_collection_prompt`.
    /// - If `uningested_sources` exist in `sources/`, returns an `[AUTO-FLOW: NEW SOURCES DETECTED]`
    ///   prompt listing the uningested paths alongside a topic-prioritized compact `wiki/index.md`.
    /// - If any topic folder in `wiki/sources/` lacks a synthesis page in `wiki/syntheses/` OR
    ///   `wiki_lint` reports `broken_links` / `orphan_wiki_pages`, returns an `[AUTO-FLOW: WIKI LINT]`
    ///   prompt for `wiki_curator`.
    /// - If the vault is fully ingested, synthesized across all topics, and healthy (`is_healthy: true`),
    ///   returns `Ok(None)`.
    pub fn build_next_autoflow_prompt(
        &self,
        initial_collection_prompt: Option<&str>,
    ) -> WikiResult<Option<String>> {
        Ok(self
            .plan_next_autoflow(initial_collection_prompt, &AutoflowPlanOptions::default())?
            .map(|plan| plan.prompt))
    }

    /// Like [`Self::build_next_autoflow_prompt`], but limits how many sources the next stage takes
    /// and skips quarantined ones, reporting exactly which sources the stage is responsible for.
    pub fn plan_next_autoflow(
        &self,
        initial_collection_prompt: Option<&str>,
        options: &AutoflowPlanOptions<'_>,
    ) -> WikiResult<Option<AutoflowPlan>> {
        let _ = self.rebuild_or_sync_index_md();
        let status = self.status()?;
        if status.sources_count == 0 {
            return Ok(initial_collection_prompt.map(|prompt| AutoflowPlan {
                prompt: prompt.to_string(),
                ingest_batch: Vec::new(),
            }));
        }

        let lint = self.lint_wiki()?;
        let excluded = |path: &str| options.exclude_sources.iter().any(|e| e == path);

        let mut pending: Vec<String> = lint
            .uningested_sources
            .iter()
            .map(|s| s.source_rel_path.clone())
            .chain(
                lint.incomplete_ingests
                    .iter()
                    .map(|i| i.source_rel_path.clone()),
            )
            .filter(|path| !excluded(path))
            .collect();
        pending.dedup();

        if !pending.is_empty() {
            let total = pending.len();
            let batch_size = options.max_sources.unwrap_or(total).clamp(1, total);
            pending.truncate(batch_size);
            let first_rel = pending[0].trim_start_matches("sources/");
            let preferred_topic = extract_topic_from_doc(first_rel, "source_summary", None);
            let compact_index = self
                .read_compact_index_md_for_topic(2_500, preferred_topic.as_deref())
                .unwrap_or_default();

            let new_sources_list = pending
                .iter()
                .map(|path| {
                    let line = format!("- `{}`", path.trim_start_matches("sources/"));
                    match lint
                        .incomplete_ingests
                        .iter()
                        .find(|i| &i.source_rel_path == path)
                    {
                        Some(item) if item.source_updated => format!(
                            "{line} — nguồn đã được cập nhật: ingest lại và gộp vào `{}`, giữ nguyên các nội dung cũ còn đúng",
                            item.wiki_rel_path
                        ),
                        Some(item) => format!(
                            "{line} — ingest dở: đã có `{}` nhưng còn thiếu {}; chỉ tạo các trang còn thiếu, không ingest lại từ đầu",
                            item.wiki_rel_path,
                            item.missing_targets
                                .iter()
                                .map(|t| format!("`{t}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        None => line,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let remaining = total - batch_size;
            let remaining_note = if remaining > 0 {
                format!("\nHệ thống còn {remaining} tài liệu khác sẽ được xử lý ở task sau; chỉ xử lý các file đã liệt kê.\n")
            } else {
                String::new()
            };
            return Ok(Some(AutoflowPlan {
                prompt: format!(
                    "[AUTO-FLOW: NEW SOURCES DETECTED]\n\
                     Hệ thống phát hiện {} tài liệu cần ingest từ `sources/` vào `wiki/`:\n{}\n{}\n\
                     === CURRENT COMPACT WIKI INDEX ===\n{}\n\
                     ==================================\n\n\
                     Hãy điều phối subagent `wiki_ingestor` (gọi tuần tự từng file một) cho các file trên. Không gọi `wiki_curator`: hệ thống tạo chặng tổng hợp riêng khi hàng đợi đã hết. Báo rõ file nào thất bại.",
                    batch_size,
                    new_sources_list,
                    remaining_note,
                    compact_index.trim()
                ),
                ingest_batch: pending,
            }));
        }

        let is_excluded_page = |page: &str| {
            lint.incomplete_ingests
                .iter()
                .any(|i| i.wiki_rel_path == page && excluded(&i.source_rel_path))
        };
        let broken_links: Vec<_> = lint
            .broken_links
            .iter()
            .filter(|b| !is_excluded_page(&b.source_rel_path))
            .collect();
        let healthy = broken_links.is_empty()
            && lint.orphan_wiki_pages.is_empty()
            && lint
                .stale_wiki_pages
                .iter()
                .all(|s| is_excluded_page(&s.wiki_rel_path));

        let unsynthesized = self.unsynthesized_topics().unwrap_or_default();
        if !unsynthesized.is_empty() || !healthy {
            let preferred_topic = unsynthesized.first().map(String::as_str);
            let compact_index = self
                .read_compact_index_md_for_topic(2_500, preferred_topic)
                .unwrap_or_default();

            return Ok(Some(AutoflowPlan {
                prompt: format!(
                    "[AUTO-FLOW: WIKI LINT]\n\
                     Trạng thái Wiki hiện tại: `unsynthesized_topics={:?}`, `broken_links={:?}`, `orphan_wiki_pages={:?}`.\n\n\
                     === CURRENT COMPACT WIKI INDEX ===\n{}\n\
                     ==================================\n\n\
                     Hãy điều phối subagent `wiki_curator` để:\n\
                     1. Nếu `unsynthesized_topics` không rỗng: tạo trang tổng hợp `wiki/syntheses/<topic-slug>-synthesis.md` cho từng chủ đề trong `unsynthesized_topics` và gắn link hai chiều từ các trang `wiki/sources/<domain>/<topic>/...`.\n\
                     2. Sửa toàn bộ `broken_links` và `orphan_wiki_pages`, sau đó chạy `wiki_lint` đảm bảo `is_healthy: true`.",
                    unsynthesized,
                    broken_links,
                    lint.orphan_wiki_pages,
                    compact_index.trim()
                ),
                ingest_batch: Vec::new(),
            }));
        }

        Ok(None)
    }

    /// Find a raw source by an identity stored in its frontmatter (`video_id`, `content_sha256`, ...),
    /// independent of its file name. Returns the first match in path order.
    pub fn find_source_by_frontmatter(
        &self,
        key: &str,
        value: &str,
    ) -> WikiResult<Option<SourceMatch>> {
        Ok(self.sources_by_frontmatter(key)?.remove(value))
    }

    /// Every raw source that has frontmatter `key`, by value, with its ingest state. Runs one lint,
    /// so looking up many identities through this map costs the same as looking up one.
    pub fn sources_by_frontmatter(
        &self,
        key: &str,
    ) -> WikiResult<std::collections::HashMap<String, SourceMatch>> {
        let mut out = std::collections::HashMap::new();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Ok(out);
        }
        let rows: Vec<(String, String, String)> = {
            let conn = self.db.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT CAST(json_extract(frontmatter_json, ?1) AS TEXT), rel_path, scoped_path
                 FROM documents
                 WHERE collection = 'sources' AND json_extract(frontmatter_json, ?1) IS NOT NULL
                 ORDER BY rel_path DESC",
            )?;
            let found = stmt
                .query_map(rusqlite::params![format!("$.{key}")], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            found
        };
        if rows.is_empty() {
            return Ok(out);
        }
        let lint = self.lint_wiki()?;
        for (value, rel_path, scoped_path) in rows {
            let ingest_state = if lint
                .uningested_sources
                .iter()
                .any(|s| s.source_rel_path == rel_path)
            {
                SourceIngestState::NotIngested
            } else if lint
                .incomplete_ingests
                .iter()
                .any(|i| i.source_rel_path == rel_path)
            {
                SourceIngestState::Incomplete
            } else {
                SourceIngestState::Ingested
            };
            // Sorted descending: the first path in ascending order wins.
            out.insert(
                value,
                SourceMatch {
                    rel_path,
                    scoped_path,
                    ingest_state,
                },
            );
        }
        Ok(out)
    }
}
