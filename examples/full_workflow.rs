use llm_wiki::{
    CollectionScope, HybridQueryRequest, IngestSourceRequest, SubQuery, SubQueryType, TreeOptions,
    TreeScope, WikiEngine,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = std::env::temp_dir().join("llm_wiki_demo_vault");
    let _ = std::fs::remove_dir_all(&vault_dir);

    println!("=== 1. Opening WikiEngine (Single Folder Config) ===");
    let engine = WikiEngine::open(&vault_dir)?;
    println!(
        "Initialized Obsidian-compatible Vault at: {}",
        vault_dir.display()
    );

    println!("\n=== 2. Creating Nested Source File (`docs/flow/lib.md`) ===");
    let src_doc = engine.write_source_file(
        "docs/flow/lib.md",
        r#"---
title: "Flow Engine Raw Specification"
tags: ["raw", "rust", "search"]
---

# Flow Engine Specification

Flow Engine is a high-performance Rust library combining **LLM Wiki** and **QMD** hybrid search.

```rust
pub struct FlowEngine {
    pub db_path: String,
}
```

## Retrieval Architecture
Uses SQLite FTS5 BM25 and Vector Embeddings fused with Reciprocal Rank Fusion (RRF).
"#,
    )?;
    println!(
        "Created raw source: {} (docid: {})",
        src_doc.rel_path, src_doc.docid
    );

    println!("\n=== 3. Ingesting Source into Wiki (Preserving Exact Filename `lib.md`) ===");
    let ingest_res = engine.ingest_source_to_wiki(IngestSourceRequest {
        source_rel_path: "docs/flow/lib.md".to_string(),
        target_wiki_path: None,
        raw_source_content: None,
        title: "Flow Engine Architecture".to_string(),
        one_line_summary: "Core architecture of Flow Engine combining LLM Wiki and QMD search."
            .to_string(),
        wiki_markdown_body: r#"## Key Takeaways
- Written in [[rust]] for memory safety and zero-cost abstractions.
- Implements [[bm25-search]] via SQLite FTS5 and combines it with [[reciprocal-rank-fusion]].
"#
        .to_string(),
        tags: vec![
            "rust".to_string(),
            "qmd".to_string(),
            "obsidian".to_string(),
        ],
        related_entities: vec!["rust".to_string()],
        related_concepts: vec![
            "bm25-search".to_string(),
            "reciprocal-rank-fusion".to_string(),
        ],
    })?;
    println!(
        "Ingested Wiki Page: {} (Preserved filename: {}, docid: {})",
        ingest_res.wiki_rel_path, ingest_res.file_name, ingest_res.wiki_docid
    );

    // Create linked entity and concept pages in `wiki/`
    engine.write_wiki_file(
        "entities/rust.md",
        "---\ntitle: \"Rust\"\ncategory: \"entity\"\ntags: [\"lang\"]\n---\n\n# Rust\n\nSystems programming language powering [[wiki/sources/docs/flow/lib|Flow Engine]].",
    )?;
    engine.write_wiki_file(
        "concepts/bm25-search.md",
        "---\ntitle: \"BM25 Search\"\ncategory: \"concept\"\ntags: [\"search\"]\n---\n\n# BM25 Search\n\nLexical full-text ranking algorithm in SQLite FTS5, paired with [[reciprocal-rank-fusion]].",
    )?;
    engine.write_wiki_file(
        "concepts/reciprocal-rank-fusion.md",
        "---\ntitle: \"Reciprocal Rank Fusion\"\ncategory: \"concept\"\ntags: [\"search\"]\n---\n\n# Reciprocal Rank Fusion\n\nCombines ranked lists from [[bm25-search]] and vector similarity.",
    )?;
    engine.rebuild_or_sync_index_md()?;

    println!("\n=== 4. Directory Tree (`TreeScope::All`) ===");
    let tree = engine.get_tree(TreeOptions::for_scope(TreeScope::All))?;
    print!("{}", tree.to_ascii_tree());

    println!("\n=== 5. QMD Hybrid Search (`lex` + `vec` + RRF + `--explain`) ===");
    let results = engine.query(HybridQueryRequest {
        query: None,
        searches: vec![
            SubQuery {
                query_type: SubQueryType::Lex,
                query: "Reciprocal Rank Fusion BM25".to_string(),
            },
            SubQuery {
                query_type: SubQueryType::Vec,
                query: "combining lexical and vector search in Rust".to_string(),
            },
        ],
        collections: vec![CollectionScope::Wiki, CollectionScope::Sources],
        filter: None,
        intent: None,
        limit: 5,
        min_score: 0.0,
        candidate_limit: Some(20),
        rerank: true,
        explain: true,
    })?;

    for (idx, r) in results.iter().enumerate() {
        println!(
            "#{}: {} ({}) — Score: {:.1}% | Title: {}",
            idx + 1,
            r.rel_path,
            r.docid,
            r.score * 100.0,
            r.title
        );
    }

    println!("\n=== 6. Obsidian Graph View Inspection & Wiki Lint ===");
    let graph = engine.get_graph(None)?;
    println!(
        "Graph Nodes: {}, Graph Edges: {}, Unresolved Targets: {:?}",
        graph.nodes.len(),
        graph.edges.len(),
        graph.unresolved_targets
    );

    let lint = engine.lint_wiki()?;
    println!("Wiki Lint is_healthy: {}", lint.is_healthy);

    println!("\n=== 7. Executing LLM Tool (`wiki_status`) ===");
    let status_json = engine.execute_llm_tool("wiki_status", json!({}))?;
    println!("{}", serde_json::to_string_pretty(&status_json)?);

    let _ = std::fs::remove_dir_all(&vault_dir);
    Ok(())
}
