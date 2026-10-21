use llm_wiki::{
    smart_chunk_markdown, ChunkConfig, CollectionScope, ExpandedQuery, GetOptions,
    HybridQueryRequest, IngestSourceRequest, MetadataDiscoveryRequest, MetadataFilter, NodeType,
    RerankProvider, SubQuery, SubQueryType, TreeOptions, TreeScope, WikiConfig, WikiEngine,
    WikiResult, WikiSourceLayout,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(1);

fn create_temp_vault(prefix: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "llm_wiki_test_{}_{}_{}",
        prefix,
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Requirement #4: Only need to configure the data folder (`WikiEngine::open(data_root)`).
#[test]
fn test_req4_single_data_folder_initialization() {
    let root = create_temp_vault("req4_init");
    let engine = WikiEngine::open(&root).expect("Failed to open engine");

    // Verify all standard folders & files are automatically created inside `root`
    assert!(root.join("sources").is_dir());
    assert!(root.join("sources/assets").is_dir());
    assert!(root.join("wiki").is_dir());
    assert!(root.join("wiki/sources").is_dir());
    assert!(root.join("wiki/entities").is_dir());
    assert!(root.join("wiki/concepts").is_dir());
    assert!(root.join("wiki/syntheses").is_dir());
    assert!(root.join("SCHEMA.md").is_file());
    assert!(root.join("wiki/index.md").is_file());
    assert!(root.join("wiki/log.md").is_file());
    assert!(root.join(".wiki/index.sqlite").is_file());
    assert!(root.join(".obsidian/graph.json").is_file());

    let status = engine.status().expect("status failed");
    assert_eq!(status.sources_count, 0);
    // `wiki/index.md` and `wiki/log.md` are indexed automatically
    assert_eq!(status.wiki_count, 2);

    let _ = std::fs::remove_dir_all(&root);
}

/// Requirement #6: API when creating a file automatically creates nested folders
/// (e.g. `"docs/flow/lib.md"` creates `docs` and `docs/flow`).
/// Also tests path traversal protection (`../`).
#[test]
fn test_req6_nested_folder_creation_and_security() {
    let root = create_temp_vault("req6_nested");
    let engine = WikiEngine::open(&root).unwrap();

    let doc = engine
        .write_source_file(
            "docs/flow/lib.md",
            "# Flow Library\n\nDetailed specification of the Flow library.",
        )
        .expect("Failed to write nested source file");

    assert!(root.join("sources/docs").is_dir());
    assert!(root.join("sources/docs/flow").is_dir());
    assert!(root.join("sources/docs/flow/lib.md").is_file());
    assert_eq!(doc.rel_path, "sources/docs/flow/lib.md");
    assert_eq!(doc.scoped_path, "docs/flow/lib.md");
    assert_eq!(doc.file_name, "lib.md");
    assert_eq!(doc.file_stem, "lib");
    assert_eq!(doc.title, "Flow Library");

    // Also test deeply nested wiki file creation without `.md` extension
    let wiki_doc = engine
        .write_wiki_file(
            "concepts/distributed/consensus/raft",
            "# Raft Consensus\n\nConsensus algorithm linked to [[lib]].",
        )
        .expect("Failed to write nested wiki file");

    assert!(root
        .join("wiki/concepts/distributed/consensus/raft.md")
        .is_file());
    assert_eq!(
        wiki_doc.rel_path,
        "wiki/concepts/distributed/consensus/raft.md"
    );

    // Reject path traversal attempts (`..`)
    let traversal_err = engine.write_source_file("../../etc/passwd.md", "malicious");
    assert!(traversal_err.is_err());

    let _ = std::fs::remove_dir_all(&root);
}

/// Requirement #8: Wiki file name MUST be the exact file name from `sources/`,
/// never an auto-generated ID or hash when performing wiki ingest.
#[test]
fn test_req8_wiki_filename_preserves_exact_source_filename() {
    let root = create_temp_vault("req8_filename");
    let engine = WikiEngine::open(&root).unwrap();

    // 1. Create source at `sources/docs/flow/lib.md`
    engine
        .write_source_file(
            "docs/flow/lib.md",
            "# Flow Engine Spec\n\nRaw source document about Flow Engine in Rust.",
        )
        .unwrap();

    // 2. Ingest into wiki
    let ingest_res = engine
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: "docs/flow/lib.md".to_string(),
            target_wiki_path: None,
            raw_source_content: None,
            title: "Flow Engine Architecture Summary".to_string(),
            one_line_summary: "Compiled summary of the Flow Engine specification.".to_string(),
            wiki_markdown_body:
                "Flow Engine uses [[rust]] and [[bm25-search]] to index nested markdown files."
                    .to_string(),
            tags: vec!["architecture".to_string(), "rust".to_string()],
            related_entities: vec!["rust".to_string()],
            related_concepts: vec!["bm25-search".to_string()],
        })
        .expect("Failed to ingest source to wiki");

    // Verify exact filename preservation (`lib.md`, NOT an ID)
    assert_eq!(ingest_res.file_name, "lib.md");
    assert_eq!(ingest_res.source_rel_path, "sources/docs/flow/lib.md");
    assert_eq!(ingest_res.wiki_scoped_path, "sources/docs/flow/lib.md");
    assert_eq!(ingest_res.wiki_rel_path, "wiki/sources/docs/flow/lib.md");
    assert!(root.join("wiki/sources/docs/flow/lib.md").is_file());

    // Also test `MirrorDirectlyInWiki` layout option
    let root_direct = create_temp_vault("req8_direct");
    let cfg_direct =
        WikiConfig::new(&root_direct).with_source_layout(WikiSourceLayout::MirrorDirectlyInWiki);
    let engine_direct = WikiEngine::open_with_config(cfg_direct).unwrap();
    let res_direct = engine_direct
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: "docs/flow/lib.md".to_string(),
            target_wiki_path: None,
            raw_source_content: Some("# Direct Source\nContent".to_string()),
            title: "Direct Wiki Summary".to_string(),
            one_line_summary: "Summary".to_string(),
            wiki_markdown_body: "Body".to_string(),
            tags: vec![],
            related_entities: vec![],
            related_concepts: vec![],
        })
        .unwrap();

    assert_eq!(res_direct.file_name, "lib.md");
    assert_eq!(res_direct.wiki_rel_path, "wiki/docs/flow/lib.md");
    assert!(root_direct.join("wiki/docs/flow/lib.md").is_file());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root_direct);
}

/// Requirement #7: API to get the directory tree of `sources`, `wiki`, `all`, and sub-paths.
#[test]
fn test_req7_directory_tree_api() {
    let root = create_temp_vault("req7_tree");
    let engine = WikiEngine::open(&root).unwrap();

    engine
        .write_source_file("docs/flow/lib.md", "# Flow Lib\nContent")
        .unwrap();
    engine
        .write_source_file("papers/attention.md", "# Attention Paper\nContent")
        .unwrap();
    engine
        .write_wiki_file("entities/rust.md", "# Rust Programming Language\nContent")
        .unwrap();

    // 1. Get tree of `sources`
    let sources_tree = engine
        .get_tree(TreeOptions::for_scope(TreeScope::Sources))
        .expect("Failed to get sources tree");
    assert_eq!(sources_tree.name, "sources");
    assert_eq!(sources_tree.node_type, NodeType::Directory);
    assert_eq!(sources_tree.total_files(), 2);

    let ascii_sources = sources_tree.to_ascii_tree();
    assert!(ascii_sources.contains("docs/"));
    assert!(ascii_sources.contains("flow/"));
    assert!(ascii_sources.contains("lib.md"));
    assert!(ascii_sources.contains("Flow Lib"));

    // 2. Get tree of `wiki`
    let wiki_tree = engine
        .get_tree(TreeOptions::for_scope(TreeScope::Wiki))
        .expect("Failed to get wiki tree");
    assert_eq!(wiki_tree.name, "wiki");
    // index.md, log.md, entities/rust.md = 3 files
    assert_eq!(wiki_tree.total_files(), 3);

    // 3. Get tree of a specific sub-path (`docs/flow` inside `sources`)
    let sub_tree = engine
        .get_tree(TreeOptions::for_scope(TreeScope::Sources).with_sub_path("docs/flow"))
        .expect("Failed to get sub_path tree");
    assert_eq!(sub_tree.name, "flow");
    assert_eq!(sub_tree.total_files(), 1);
    assert_eq!(sub_tree.children[0].name, "lib.md");

    let _ = std::fs::remove_dir_all(&root);
}

/// Requirement #2: Full compatibility with Obsidian's Graph View (`[[WikiLinks]]`, aliases, headings,
/// YAML frontmatter links, link resolution priority, backlinks, outgoing links, orphans, unresolved links).
#[test]
fn test_req2_obsidian_graph_view_compatibility() {
    let root = create_temp_vault("req2_obsidian");
    let engine = WikiEngine::open(&root).unwrap();

    // Create entity & concept pages
    engine
        .write_wiki_file(
            "entities/rust.md",
            "---\ntitle: \"Rust\"\ncategory: \"entity\"\naliases:\n  - \"RustLang\"\ntags:\n  - \"lang/rust\"\n---\n\n# Rust\n\nSystems programming language used with [[bm25-search|BM25]].",
        )
        .unwrap();

    engine
        .write_wiki_file(
            "concepts/bm25-search.md",
            "---\ntitle: \"BM25 Search\"\ncategory: \"concept\"\n---\n\n# BM25 Search\n\nRanking function implemented in [[rust#Memory Safety|Rust]] and mentioning [[missing-concept]].",
        )
        .unwrap();

    // Ingest source `docs/flow/lib.md` which links to both `[[sources/docs/flow/lib.md|lib.md]]` in frontmatter
    // and `[[RustLang]]` (via alias!) and `[[bm25-search]]` in body
    engine
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: "docs/flow/lib.md".to_string(),
            target_wiki_path: None,
            raw_source_content: Some("# Raw Flow Lib\nSource text.".to_string()),
            title: "Flow Lib Wiki".to_string(),
            one_line_summary: "Flow summary".to_string(),
            wiki_markdown_body: "Uses [[RustLang]] and [[bm25-search]] for high speed search."
                .to_string(),
            tags: vec!["graph-test".to_string()],
            related_entities: vec!["rust".to_string()],
            related_concepts: vec!["bm25-search".to_string()],
        })
        .unwrap();

    // Verify alias resolution: `[[RustLang]]` resolves to `wiki/entities/rust.md`
    let resolved_alias = engine
        .resolve_wikilink("[[RustLang]]", Some("wiki/sources/docs/flow/lib.md"))
        .unwrap()
        .expect("Failed to resolve alias [[RustLang]]");
    assert_eq!(resolved_alias.rel_path, "wiki/entities/rust.md");

    // Verify backlinks to `wiki/entities/rust.md`
    let rust_backlinks = engine.get_backlinks("wiki/entities/rust.md").unwrap();
    let backlink_sources: Vec<&str> = rust_backlinks
        .iter()
        .map(|b| b.source_rel_path.as_str())
        .collect();
    assert!(backlink_sources.contains(&"wiki/concepts/bm25-search.md"));
    assert!(backlink_sources.contains(&"wiki/sources/docs/flow/lib.md"));

    // Verify frontmatter `source_file: "[[sources/docs/flow/lib.md|lib.md]]"` created a graph edge to raw source!
    let raw_source_backlinks = engine.get_backlinks("sources/docs/flow/lib.md").unwrap();
    assert!(raw_source_backlinks
        .iter()
        .any(|b| b.source_rel_path == "wiki/sources/docs/flow/lib.md"));

    // Verify full GraphData includes unresolved target `missing-concept`
    let graph = engine.get_graph(None).unwrap();
    assert!(graph
        .unresolved_targets
        .contains(&"missing-concept".to_string()));

    let _ = std::fs::remove_dir_all(&root);
}

/// Requirement #1: QMD Smart Chunking with Code Fence Protection, BM25 FTS5, Vector Search,
/// RRF Fusion, Position-Aware Reranker Blending, Metadata AST Filter & Discovery, and Path Contexts.
#[test]
fn test_req1_qmd_smart_chunking_and_hybrid_search() {
    // 1. Test Smart Chunking with Code Fence Protection
    let sample_md = r#"# Section One
Introductory paragraph explaining the system architecture.

```rust
fn inside_code_block() {
    // # Fake Heading Inside Code Block
    let x = 10;
    let y = 20;
}
```

## Subsection Two
More detailed prose about Reciprocal Rank Fusion and BM25 ranking in SQLite FTS5.
"#;

    let chunks = smart_chunk_markdown(
        sample_md,
        "Chunking Test",
        0,
        &ChunkConfig {
            target_tokens: 25,
            overlap_ratio: 0.15,
            window_tokens: 10,
            chars_per_token: 4,
        },
    );
    assert!(!chunks.is_empty());
    // Ensure code block was not split at `# Fake Heading Inside Code Block`
    let code_chunk = chunks
        .iter()
        .find(|c| c.chunk_text.contains("fn inside_code_block()"))
        .expect("Code block missing");
    assert!(code_chunk
        .chunk_text
        .contains("# Fake Heading Inside Code Block"));

    // 2. Test QMD Hybrid Search, Path Contexts, Metadata Filtering & Discovery, and Custom Reranker
    let root = create_temp_vault("req1_qmd_search");
    let engine = WikiEngine::open(&root).unwrap();

    engine
        .set_path_context(
            Some(CollectionScope::Wiki),
            "/concepts",
            "Core algorithmic and architectural concepts",
        )
        .unwrap();

    engine
        .write_wiki_file(
            "concepts/hybrid-rrf.md",
            r#"---
title: "Reciprocal Rank Fusion (RRF)"
category: "concept"
qmd:
  metadata:
    topics:
      - search
      - ranking
    status: published
    priority: 5
    reviewed: true
---

# Reciprocal Rank Fusion (RRF)

RRF combines lexical BM25 results and vector semantic search results using `1 / (60 + rank + 1)`.
"#,
        )
        .unwrap();

    engine
        .write_wiki_file(
            "concepts/draft-note.md",
            r#"---
title: "Draft Search Note"
category: "concept"
qmd:
  metadata:
    topics:
      - search
    status: draft
    priority: 1
    reviewed: false
---

# Draft Search Note

Preliminary notes on BM25 search and vector ranking.
"#,
        )
        .unwrap();

    // Test Metadata Discovery
    let discovery = engine
        .discover_metadata(MetadataDiscoveryRequest {
            collections: vec![CollectionScope::Wiki],
            ..Default::default()
        })
        .unwrap();
    let topic_key = discovery
        .keys
        .iter()
        .find(|k| k.key == "topics")
        .expect("topics metadata key should be discovered");
    assert_eq!(topic_key.documents, 2);

    // Test Metadata Filter AST (status == "published" AND topics ALL ["search", "ranking"] AND priority >= 3)
    let filter_ast: MetadataFilter = serde_json::from_value(json!({
        "operator": "and",
        "operands": [
            { "field": "status", "operator": "eq", "value": "published" },
            { "field": "topics", "operator": "all", "value": ["search", "ranking"] },
            { "field": "priority", "operator": "gte", "value": 3 }
        ]
    }))
    .unwrap();

    let hybrid_results = engine
        .query(HybridQueryRequest {
            query: None,
            searches: vec![
                SubQuery {
                    query_type: SubQueryType::Lex,
                    query: "Reciprocal Rank Fusion BM25".to_string(),
                },
                SubQuery {
                    query_type: SubQueryType::Vec,
                    query: "combining lexical and vector search results".to_string(),
                },
            ],
            collections: vec![CollectionScope::Wiki],
            filter: Some(filter_ast),
            intent: Some("search ranking fusion".to_string()),
            limit: 5,
            min_score: 0.0,
            candidate_limit: Some(20),
            rerank: true,
            explain: true,
        })
        .unwrap();

    assert_eq!(hybrid_results.len(), 1);
    assert_eq!(hybrid_results[0].rel_path, "wiki/concepts/hybrid-rrf.md");
    assert_eq!(
        hybrid_results[0].context.as_deref(),
        Some("Core algorithmic and architectural concepts")
    );
    let explain = hybrid_results[0]
        .explain
        .as_ref()
        .expect("Explain trace should be attached");
    assert_eq!(explain.rrf.rank, 1);
    assert!((explain.rrf.top_rank_bonus - 0.05).abs() < 1e-5);

    // Retrieve by QMD `#docid` and `:from:count` line range
    let docid_with_range = format!("{}:1:5", hybrid_results[0].docid);
    let doc_slice = engine
        .get_document(
            &docid_with_range,
            GetOptions {
                line_numbers: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(doc_slice.content.starts_with("1: "));

    let _ = std::fs::remove_dir_all(&root);
}

/// Requirement #5: Full suite of 15 LLM Tools (`llm_tool_definitions` + `execute_llm_tool`).
#[test]
fn test_req5_llm_tools_end_to_end() {
    let root = create_temp_vault("req5_llm_tools");
    let engine = WikiEngine::open(&root).unwrap();

    let tools = engine.llm_tool_definitions();
    assert_eq!(tools.len(), 15);

    // 1. Call `wiki_write_file` to create a nested raw source `docs/flow/lib.md`
    let write_src_res = engine
        .execute_llm_tool(
            "wiki_write_file",
            json!({
                "scope": "sources",
                "rel_path": "docs/flow/lib.md",
                "content": "# Flow Engine\n\nFlow Engine is written in Rust and uses SQLite FTS5."
            }),
        )
        .unwrap();
    assert_eq!(write_src_res["rel_path"], "sources/docs/flow/lib.md");

    // 2. Call `wiki_lint` BEFORE ingest -> should report `sources/docs/flow/lib.md` as un-ingested
    let lint_before = engine.execute_llm_tool("wiki_lint", json!({})).unwrap();
    assert_eq!(
        lint_before["uningested_sources"][0]["source_rel_path"],
        "sources/docs/flow/lib.md"
    );

    // 3. Call `wiki_ingest_source` -> creates `wiki/sources/docs/flow/lib.md` (preserving `lib.md`)
    let ingest_res = engine
        .execute_llm_tool(
            "wiki_ingest_source",
            json!({
                "source_rel_path": "docs/flow/lib.md",
                "title": "Flow Engine Summary",
                "one_line_summary": "Summary of the Flow Engine architecture.",
                "wiki_markdown_body": "Flow Engine leverages [[rust]] for safe concurrency.",
                "tags": ["rust", "flow"],
                "related_entities": ["rust"]
            }),
        )
        .unwrap();
    assert_eq!(ingest_res["file_name"], "lib.md");
    assert_eq!(ingest_res["wiki_rel_path"], "wiki/sources/docs/flow/lib.md");

    // 4. Call `wiki_write_file` to create `wiki/entities/rust.md`
    engine
        .execute_llm_tool(
            "wiki_write_file",
            json!({
                "scope": "wiki",
                "rel_path": "entities/rust.md",
                "content": "---\ntitle: \"Rust\"\ncategory: \"entity\"\n---\n\n# Rust\n\nUsed by [[wiki/sources/docs/flow/lib|Flow Engine]]."
            }),
        )
        .unwrap();

    // 5. Call `wiki_patch_file` to append a section to `wiki/entities/rust.md`
    engine
        .execute_llm_tool(
            "wiki_patch_file",
            json!({
                "scope": "wiki",
                "rel_path": "entities/rust.md",
                "append_text": "\n## Ecosystem\nIncludes `rusqlite` and `pulldown-cmark`."
            }),
        )
        .unwrap();

    // 6. Call `wiki_tree`
    let tree_res = engine
        .execute_llm_tool("wiki_tree", json!({ "scope": "all" }))
        .unwrap();
    assert!(tree_res["ascii_tree"].as_str().unwrap().contains("lib.md"));

    // 7. Call `wiki_query`
    let query_res = engine
        .execute_llm_tool(
            "wiki_query",
            json!({
                "searches": [
                    { "type": "lex", "query": "Flow Engine concurrency" },
                    { "type": "vec", "query": "Rust concurrency in Flow Engine" }
                ],
                "limit": 5
            }),
        )
        .unwrap();
    assert!(query_res["count"].as_u64().unwrap() >= 1);

    // 8. Call `wiki_get` & `wiki_multi_get`
    let get_res = engine
        .execute_llm_tool(
            "wiki_get",
            json!({ "file": "wiki/entities/rust.md", "line_numbers": true }),
        )
        .unwrap();
    assert!(get_res["content"].as_str().unwrap().contains("rusqlite"));

    let multi_res = engine
        .execute_llm_tool("wiki_multi_get", json!({ "pattern": "wiki/**/*.md" }))
        .unwrap();
    assert!(multi_res["docs"].as_array().unwrap().len() >= 3);

    // 9. Call `wiki_graph_neighbors`
    let graph_res = engine
        .execute_llm_tool(
            "wiki_graph_neighbors",
            json!({ "rel_path": "wiki/entities/rust.md" }),
        )
        .unwrap();
    assert!(graph_res["backlinks_count"].as_u64().unwrap() >= 1);

    // 10. Call `wiki_lint` AFTER ingest & entity creation -> should be 100% healthy!
    let lint_after = engine.execute_llm_tool("wiki_lint", json!({})).unwrap();
    assert_eq!(lint_after["is_healthy"], true);

    // 11. Call `wiki_log` & `wiki_status` & `wiki_metadata`
    let log_res = engine
        .execute_llm_tool("wiki_log", json!({ "mode": "read", "limit": 5 }))
        .unwrap();
    assert!(log_res["count"].as_u64().unwrap() >= 2);

    let status_res = engine.execute_llm_tool("wiki_status", json!({})).unwrap();
    assert_eq!(status_res["sources_count"], 1);

    let meta_res = engine
        .execute_llm_tool("wiki_metadata", json!({ "collections": ["wiki"] }))
        .unwrap();
    assert!(meta_res["total_keys"].as_u64().unwrap() >= 1);

    let _ = std::fs::remove_dir_all(&root);
}

/// Test custom `EmbeddingProvider` and `RerankProvider` traits with Position-Aware Blending.
#[test]
fn test_custom_embedder_and_reranker_blending() {
    struct MockReranker;
    impl RerankProvider for MockReranker {
        fn rerank(&self, _query: &str, candidates: &[String]) -> WikiResult<Vec<f32>> {
            Ok(candidates
                .iter()
                .map(|c| if c.contains("TargetDoc") { 0.99 } else { 0.10 })
                .collect())
        }

        fn expand_query(
            &self,
            query: &str,
            _intent: Option<&str>,
        ) -> WikiResult<Vec<ExpandedQuery>> {
            Ok(vec![ExpandedQuery {
                query_type: SubQueryType::Lex,
                query: format!("{query} TargetDoc"),
            }])
        }
    }

    let root = create_temp_vault("custom_reranker");
    let engine = WikiEngine::open(&root)
        .unwrap()
        .with_reranker(Arc::new(MockReranker));

    engine
        .write_wiki_file("concepts/a.md", "# Doc A\nGeneral systems note.")
        .unwrap();
    engine
        .write_wiki_file(
            "concepts/b.md",
            "# TargetDoc\nGeneral systems note with special target insights.",
        )
        .unwrap();

    let results = engine
        .query(HybridQueryRequest {
            query: Some("General systems".to_string()),
            explain: true,
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    assert!(results[0]
        .explain
        .as_ref()
        .unwrap()
        .reranker_score
        .is_some());

    let _ = std::fs::remove_dir_all(&root);
}

/// Test file move, file delete (with empty parent folder cleanup), external filesystem sync,
/// orphan detection, unlinked mention detection, and complex QMD Metadata Filter AST operators.
#[test]
fn test_file_lifecycle_sync_and_advanced_lint() {
    let root = create_temp_vault("lifecycle_and_lint");
    let engine = WikiEngine::open(&root).unwrap();

    // 1. Create deeply nested file, then move it, then delete it and verify empty parent dirs are pruned
    engine
        .write_source_file("a/b/c/temp_doc.md", "# Temporary Doc\nSome content.")
        .unwrap();
    assert!(root.join("sources/a/b/c/temp_doc.md").is_file());

    let moved = engine
        .move_file(
            CollectionScope::Sources,
            "a/b/c/temp_doc.md",
            "x/y/moved_doc.md",
        )
        .unwrap();
    assert_eq!(moved.rel_path, "sources/x/y/moved_doc.md");
    assert!(!root.join("sources/a").exists()); // Empty `a/b/c` pruned automatically!
    assert!(root.join("sources/x/y/moved_doc.md").is_file());

    let deleted = engine
        .delete_file(CollectionScope::Sources, "x/y/moved_doc.md")
        .unwrap();
    assert!(deleted);
    assert!(!root.join("sources/x").exists()); // Empty `x/y` pruned automatically!

    // 2. Test external file creation on disk + `sync_index()`
    let external_dir = root.join("sources/external/nested");
    std::fs::create_dir_all(&external_dir).unwrap();
    std::fs::write(
        external_dir.join("paper.md"),
        "# External Paper\nDiscusses [[sqlite]] and vector indexing.",
    )
    .unwrap();

    let sync_stats = engine.sync_index().unwrap();
    assert_eq!(sync_stats.indexed_new, 1);

    // 3. Test Orphan & Unlinked Mention detection in `lint_wiki()`
    engine
        .write_wiki_file(
            "entities/sqlite.md",
            "---\ntitle: \"SQLite\"\ncategory: \"entity\"\n---\n\n# SQLite\nEmbedded relational database.",
        )
        .unwrap();
    engine
        .write_wiki_file(
            "concepts/vector-indexing.md",
            "---\ntitle: \"Vector Indexing\"\ncategory: \"concept\"\n---\n\n# Vector Indexing\nUses SQLite under the hood to store embeddings.",
        )
        .unwrap();

    // Rebuild index.md (which links to all wiki pages as a system catalog)
    engine.rebuild_or_sync_index_md().unwrap();

    let lint = engine.lint_wiki().unwrap();
    // `sources/external/nested/paper.md` has not been ingested into `wiki/` yet
    assert_eq!(lint.uningested_sources.len(), 1);
    assert_eq!(
        lint.uningested_sources[0].expected_wiki_rel_path,
        "wiki/sources/external/nested/paper.md"
    );
    assert_eq!(lint.uningested_sources[0].file_name, "paper.md");

    // `wiki/concepts/vector-indexing.md` has no inbound links from non-system pages -> detected as orphan!
    assert!(lint
        .orphan_wiki_pages
        .contains(&"wiki/concepts/vector-indexing.md".to_string()));

    // `wiki/concepts/vector-indexing.md` mentions "SQLite" in plain text without `[[sqlite]]` -> detected as unlinked mention!
    assert!(lint.unlinked_mentions.iter().any(|u| {
        u.page_rel_path == "wiki/concepts/vector-indexing.md"
            && u.target_entity_or_concept == "sqlite"
    }));

    // 4. Test complex QMD MetadataFilter operators (`or`, `not`, `in`, `nin`, `prefix`, `contains`, `exists`, `type`)
    engine
        .write_wiki_file(
            "syntheses/report-2026.md",
            r#"---
title: "2026 Benchmark Report"
category: "synthesis"
qmd:
  metadata:
    code: "BENCH-2026-ALPHA"
    tags: ["rust", "sqlite", "qmd"]
    audience: "public"
    score: 95.5
    verified: true
---

# 2026 Benchmark Report
Comprehensive hybrid search evaluation.
"#,
        )
        .unwrap();

    let complex_filter: MetadataFilter = serde_json::from_value(json!({
        "operator": "and",
        "operands": [
            { "field": "code", "operator": "prefix", "value": "bench-2026", "caseInsensitive": true },
            { "field": "code", "operator": "contains", "value": "ALPHA" },
            { "field": "tags", "operator": "in", "value": ["qmd", "other"] },
            { "field": "audience", "operator": "nin", "value": ["internal", "secret"] },
            { "field": "score", "operator": "type", "value": "number" },
            { "field": "verified", "operator": "exists", "value": true },
            {
                "operator": "not",
                "operand": { "field": "audience", "operator": "eq", "value": "internal" }
            }
        ]
    }))
    .unwrap();

    let filtered_res = engine
        .search_lex(
            "Benchmark evaluation",
            llm_wiki::SearchOptions {
                collections: vec![CollectionScope::Wiki],
                filter: Some(complex_filter),
                limit: 10,
                min_score: 0.0,
            },
        )
        .unwrap();
    assert_eq!(filtered_res.len(), 1);
    assert_eq!(filtered_res[0].rel_path, "wiki/syntheses/report-2026.md");

    let _ = std::fs::remove_dir_all(&root);
}

/// Test Karpathy's incremental compounding workflow:
/// When a new source arrives (`docs/flow/changelog-v2.md`), the LLM can update an existing
/// wiki page (`wiki/sources/docs/flow/lib.md`) WITHOUT creating a new wiki file, while
/// tracking both sources in Obsidian Graph View and marking both as ingested in `lint_wiki()`.
#[test]
fn test_incremental_source_updates_existing_wiki_without_creating_new_file() {
    let root = create_temp_vault("incremental_update");
    let engine = WikiEngine::open(&root).unwrap();

    // 1. Initial source `docs/flow/lib.md` creates `wiki/sources/docs/flow/lib.md` (filename `lib.md`)
    let first = engine
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: "docs/flow/lib.md".to_string(),
            target_wiki_path: None,
            raw_source_content: Some("# Flow v1\nInitial release of Flow.".to_string()),
            title: "Flow Library".to_string(),
            one_line_summary: "Flow library specification (v1).".to_string(),
            wiki_markdown_body: "## v1 Architecture\nInitial design.".to_string(),
            tags: vec!["flow".to_string()],
            related_entities: vec![],
            related_concepts: vec![],
        })
        .unwrap();
    assert_eq!(first.wiki_rel_path, "wiki/sources/docs/flow/lib.md");
    assert_eq!(first.file_name, "lib.md");

    // 2. A second source `docs/flow/changelog-v2.md` arrives, and the LLM decides to ONLY update
    // the existing `wiki/sources/docs/flow/lib.md` page without creating `changelog-v2.md` in `wiki/`!
    let second = engine
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: "docs/flow/changelog-v2.md".to_string(),
            target_wiki_path: Some("sources/docs/flow/lib.md".to_string()),
            raw_source_content: Some("# Flow v2 Changelog\nAdded streaming support.".to_string()),
            title: "Flow Library".to_string(),
            one_line_summary: "Flow library specification (v1 & v2).".to_string(),
            wiki_markdown_body:
                "## v1 Architecture\nInitial design.\n\n## v2 Update\nAdded streaming support."
                    .to_string(),
            tags: vec!["flow".to_string()],
            related_entities: vec![],
            related_concepts: vec![],
        })
        .unwrap();

    // Verify no new wiki file was created for `changelog-v2.md`
    assert_eq!(second.wiki_rel_path, "wiki/sources/docs/flow/lib.md");
    assert!(!root.join("wiki/sources/docs/flow/changelog-v2.md").exists());

    // Verify `wiki/sources/docs/flow/lib.md` links to BOTH raw sources in Obsidian Graph View
    let backlinks_s1 = engine.get_backlinks("sources/docs/flow/lib.md").unwrap();
    let backlinks_s2 = engine
        .get_backlinks("sources/docs/flow/changelog-v2.md")
        .unwrap();
    assert!(backlinks_s1
        .iter()
        .any(|b| b.source_rel_path == "wiki/sources/docs/flow/lib.md"));
    assert!(backlinks_s2
        .iter()
        .any(|b| b.source_rel_path == "wiki/sources/docs/flow/lib.md"));

    // Verify `lint_wiki()` sees BOTH raw sources as ingested (`uningested_sources` is empty!)
    let lint = engine.lint_wiki().unwrap();
    assert!(lint.uningested_sources.is_empty());

    // 3. Test Karpathy's "File query synthesis back into the wiki" (`wiki_file_synthesis`),
    // `SCHEMA.md` access (`wiki_schema`), local image asset link (`![[assets/arch.png]]`),
    // and contradiction flagging (`> [!WARNING] Contradiction: ...`)
    std::fs::write(root.join("sources/assets/arch.png"), b"fake_png_bytes").unwrap();

    let synth_res = engine
        .execute_llm_tool(
            "wiki_file_synthesis",
            json!({
                "rel_path": "flow-v1-vs-v2.md",
                "query": "How did Flow evolve from v1 to v2?",
                "title": "Flow v1 vs v2 Comparison",
                "one_line_summary": "Comparison of Flow v1 and v2 architectures.",
                "markdown_body": "Compares [[wiki/sources/docs/flow/lib|Flow Lib]] with ![[assets/arch.png]].\n\n> [!WARNING] Contradiction: v1 claimed synchronous I/O was sufficient.",
                "tags": ["comparison"]
            }),
        )
        .unwrap();
    assert_eq!(synth_res["rel_path"], "wiki/syntheses/flow-v1-vs-v2.md");

    let schema_doc = engine
        .get_document("SCHEMA.md", GetOptions::default())
        .unwrap();
    assert!(schema_doc.content.contains("LLM Wiki Schema"));

    let lint2 = engine.lint_wiki().unwrap();
    // `![[assets/arch.png]]` exists in `sources/assets/arch.png`, so `broken_links` MUST be empty!
    assert!(lint2.broken_links.is_empty());
    // And the `[!WARNING] Contradiction:` line is surfaced in `flagged_contradictions_or_gaps`!
    assert_eq!(lint2.flagged_contradictions_or_gaps.len(), 1);

    let _ = std::fs::remove_dir_all(&root);
}

/// Test reusable flow utilities added to `llm-wiki`:
/// `apply_template`, `write_topic_source`, `execute_llm_tool_with_budget`, `wiki_delete_file`,
/// `build_next_autoflow_prompt`, `AdaptiveReadBudget`, `sanitize_mermaid_fences`, and `is_read_only_wiki_tool`.
#[test]
fn test_flow_utilities_and_adaptive_tool_executor() {
    let root = create_temp_vault("flow_utilities");
    let engine = WikiEngine::open(&root).unwrap();

    // 1. Apply domain template and verify 5 QMD path contexts + SCHEMA.md
    let tpl = engine
        .apply_template(llm_wiki::WikiTemplateId::Research)
        .unwrap();
    assert!(!tpl.schema_md.is_empty());
    assert_eq!(engine.list_path_contexts().unwrap().len(), 5);

    // 2. When vault has 0 sources, `build_next_autoflow_prompt` returns `initial_collection_prompt`
    let step0 = engine
        .build_next_autoflow_prompt(Some("Collect playlist sources"))
        .unwrap();
    assert_eq!(step0.as_deref(), Some("Collect playlist sources"));

    // 3. Write a topic source via `write_topic_source`
    let written = engine
        .write_topic_source(llm_wiki::WriteTopicSourceRequest {
            topic_folder: "sources/Trading / Smart Money Concepts".to_string(),
            file_stem: Some("01-pd-arrays".to_string()),
            title: "PD Arrays & Order Flow".to_string(),
            raw_body: "Line 1 of transcript.\nLine 2 of transcript.".to_string(),
            body_heading: Some("Raw Transcript".to_string()),
            tags: vec!["ict".to_string(), "smc".to_string()],
            extra_frontmatter: vec![("video_id".to_string(), json!("abc123"))],
            header_bullets: vec![("Video ID".to_string(), "`abc123`".to_string())],
            preview_chars: Some(200),
        })
        .unwrap();
    assert_eq!(written.topic_folder, "trading/smart-money-concepts");
    assert_eq!(
        written.info.rel_path,
        "sources/trading/smart-money-concepts/01-pd-arrays.md"
    );
    assert!(written
        .agent_output
        .contains("RAW_SOURCE_SAVED: sources/trading/smart-money-concepts/01-pd-arrays.md"));

    // 4. Now `build_next_autoflow_prompt` detects the uningested source
    let step1 = engine
        .build_next_autoflow_prompt(Some("Collect playlist sources"))
        .unwrap()
        .unwrap();
    assert!(step1.contains("[AUTO-FLOW: NEW SOURCES DETECTED]"));
    assert!(step1.contains("trading/smart-money-concepts/01-pd-arrays.md"));

    // 5. Execute `wiki_get` on `sources/...` via `execute_llm_tool_with_budget` -> frontmatter stripped & budgeted
    let budget = llm_wiki::AdaptiveReadBudget::for_context_limit(200_000);
    assert_eq!(budget.default_lines, 400);
    assert!(llm_wiki::is_read_only_wiki_tool("wiki_get"));
    assert!(!llm_wiki::is_read_only_wiki_tool("wiki_delete_file"));

    let get_out = engine
        .execute_llm_tool_with_budget(
            "wiki_get",
            json!({ "file": "sources/trading/smart-money-concepts/01-pd-arrays.md" }),
            budget,
        )
        .unwrap();
    assert!(!get_out.content.contains("qmd:\n  metadata:"));
    assert!(get_out.content.contains("# PD Arrays & Order Flow"));

    // 6. Execute `wiki_write_file` with malformed `</mermaid>` fence -> automatically sanitized to ```
    engine
        .execute_llm_tool_with_budget(
            "wiki_write_file",
            json!({
                "scope": "wiki",
                "rel_path": "concepts/temp-dup.md",
                "content": "---\ntitle: \"Temp\"\ncategory: \"concept\"\n---\n\n# Temp\n```mermaid\nflowchart LR\n  A[\"One\"] --> B[\"Two\"]\n</mermaid>\n"
            }),
            budget,
        )
        .unwrap();
    let saved_temp = std::fs::read_to_string(root.join("wiki/concepts/temp-dup.md")).unwrap();
    assert!(!saved_temp.contains("</mermaid>"));
    assert!(saved_temp.contains("```\n"));

    // 7. Execute `wiki_delete_file` to remove the duplicate/temp page
    let del_out = engine
        .execute_llm_tool_with_budget(
            "wiki_delete_file",
            json!({ "rel_path": "concepts/temp-dup.md" }),
            budget,
        )
        .unwrap();
    assert!(del_out.content.contains("deleted=true"));
    assert!(!root.join("wiki/concepts/temp-dup.md").exists());

    let _ = std::fs::remove_dir_all(&root);
}
