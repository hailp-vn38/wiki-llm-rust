<p align="right">🌐 <b>🇬🇧 English</b> · <a href="README.vi.md">🇻🇳 Tiếng Việt</a></p>

# LLM Wiki RS (`llm-wiki-rs`)

A local-first Rust library combining Andrej Karpathy's **[LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** compounding knowledge architecture with **[QMD (Query Markup Documents)](https://github.com/tobi/qmd)** hybrid Markdown search and 100% **[Obsidian Graph View](https://obsidian.md)** compatibility.

- **Crate**: [`llm-wiki-rs`](https://crates.io/crates/llm-wiki-rs) `v0.1.2` (imported in Rust as `llm_wiki`), edition 2021
- **Storage**: Embedded SQLite (`rusqlite` bundled + FTS5 BM25 + vector cosine distance + graph edge index) stored directly inside `<data_root>/.wiki/index.sqlite`
- **Detailed Architecture**: [ARCHITECTURE.md](./ARCHITECTURE.md) · [docs/karpathy-llm-wiki.md](./docs/karpathy-llm-wiki.md)

## Features

| Area | Description |
|---|---|
| **Single-Folder Setup** | `WikiEngine::open(data_root)` initializes `sources/`, `wiki/`, `SCHEMA.md`, `wiki/index.md`, `wiki/log.md`, `.obsidian/graph.json`, and `.wiki/index.sqlite` automatically. |
| **QMD Smart Chunking** | ~900-token chunks with 15% overlap, semantic breakpoint scoring (`H1`–`H6`, paragraphs, horizontal rules), squared distance decay, and **Code Fence Protection** (never splits inside ```` ``` ```` blocks). |
| **QMD Hybrid Search** | Combines **SQLite FTS5 (BM25)** + **Vector Similarity** + **Reciprocal Rank Fusion (RRF, $k=60$)** ($2\times$ weight for primary query, top-rank bonus $+0.05 / +0.02$) + **Position-Aware Reranker Blending**. |
| **Wiki-First Query & Graph Relations** | Queries default to `collections: ["wiki"]` (pass `["sources"]` or `["wiki", "sources"]` to include raw sources). Every `SearchResultItem` includes `is_wiki_linked`, `outgoing_links`, `backlinks`, and `unresolved_links`. |
| **Metadata AST Filter & Discovery** | Indexes YAML frontmatter and `qmd.metadata` into typed SQLite tables with recursive boolean filter AST (`and`, `or`, `not`, `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `in`, `nin`, `all`, `contains`, `prefix`, `suffix`, `exists`, `type`). |
| **Obsidian Graph View** | Full support for `[[WikiLinks]]`, `[[path#Heading\|Alias]]`, `![[Embeds]]`, YAML frontmatter property links (`source_file: "[[sources/...\|...]]"`), 1-level `<domain>` folder routing (`wiki/entities/<domain>/`, `wiki/concepts/<domain>/`, `wiki/syntheses/<domain>/`), global unique slug redirect, and automatic on-disk WikiLink healing. |
| **Exact Source Filename Preservation** | Ingesting `sources/docs/flow/lib.md` creates `wiki/sources/docs/flow/lib.md` (preserving `lib.md` — never a random hash/ID). |
| **Nested Auto-Mkdir & Tree API** | Writing `"docs/flow/lib.md"` validates against path traversal (`..`) and creates intermediate directories automatically. `get_tree()` returns recursive `FileTreeNode` + `.to_ascii_tree()`. |
| **13 Built-in LLM Tools & Adaptive Budget** | OpenAI/Anthropic/Gemini/MCP-ready tool schemas (`llm_tool_definitions()`) and `execute_llm_tool_with_budget()` with `AdaptiveReadBudget`, topic-prioritized compact index (`read_compact_index_md_for_topic`), and Reactive Auto-Flow (`build_next_autoflow_prompt`). |

## Installation

Add `llm-wiki-rs` to your `Cargo.toml`:

```toml
[dependencies]
llm-wiki-rs = "0.1.2"
```

> Note: The library target name is `llm_wiki`, so you import it in Rust code with `use llm_wiki::*;`.

## Quick Start

```rust
use llm_wiki::{
    AdaptiveReadBudget, HybridQueryRequest, IngestSourceRequest, TreeOptions, TreeScope, WikiEngine,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open or initialize an Obsidian-compatible vault with a single folder path
    let engine = WikiEngine::open("./my_knowledge_vault")?;

    // 2. Write a nested raw source file (auto-creates `sources/docs/flow/`)
    engine.write_source_file(
        "docs/flow/lib.md",
        "# Flow Engine\n\nFlow Engine is written in Rust and uses SQLite FTS5.",
    )?;

    // 3. Ingest source into `wiki/` (strictly preserves filename `lib.md` at `wiki/sources/docs/flow/lib.md`)
    engine.ingest_source_to_wiki(IngestSourceRequest {
        source_rel_path: "docs/flow/lib.md".to_string(),
        target_wiki_path: None,
        raw_source_content: None,
        title: "Flow Engine Architecture".to_string(),
        one_line_summary: "Summary of the Flow Engine architecture.".to_string(),
        wiki_markdown_body: "Flow Engine uses [[rust]] and [[bm25-search]] for hybrid retrieval."
            .to_string(),
        tags: vec!["rust".to_string(), "search".to_string()],
        related_entities: vec!["rust".to_string()],
        related_concepts: vec!["bm25-search".to_string()],
    })?;

    // 4. Query (`collections` defaults to `["wiki"]`, and results include WikiLink relationships)
    let hits = engine.query(HybridQueryRequest::from_query("Flow Engine hybrid retrieval"))?;
    for hit in &hits {
        println!(
            "{} ({}) score={:.2} is_wiki_linked={} outgoing={:?} backlinks={:?}",
            hit.rel_path, hit.docid, hit.score, hit.is_wiki_linked, hit.outgoing_links, hit.backlinks
        );
    }

    // 5. Inspect directory tree
    let tree = engine.get_tree(TreeOptions::for_scope(TreeScope::All))?;
    println!("{}", tree.to_ascii_tree());

    // 6. Execute built-in LLM tools with adaptive token budget
    let budget = AdaptiveReadBudget::for_context_limit(200_000);
    let out = engine.execute_llm_tool_with_budget(
        "wiki_query",
        json!({ "query": "Flow Engine" }),
        budget,
    )?;
    println!("{}", out.content);

    Ok(())
}
```

## Built-in LLM Tools (`llm_tool_definitions`)

| Tool Name | Mode | Description |
|---|---|---|
| `wiki_tree` | Read-Only | Get directory and file tree structure of `sources`, `wiki`, or `all` (JSON + ASCII tree). |
| `wiki_query` | Read-Only | QMD Hybrid Search (`lex` BM25 + `vec` Semantic + `hyde` + RRF + Metadata AST `filter`). Defaults to `collections: ["wiki"]` and returns `is_wiki_linked`, `outgoing_links`, `backlinks`, `unresolved_links`. |
| `wiki_get` | Read-Only | Read a document by path, stem, or `#docid` with `:from:count` line slicing and `AdaptiveReadBudget`. |
| `wiki_multi_get` | Read-Only | Batch retrieve multiple documents by glob pattern (`wiki/entities/**/*.md`) or comma-separated list. |
| `wiki_metadata` | Read-Only | Discover indexed YAML frontmatter keys, value types, and document counts. |
| `wiki_status` | Read-Only | Return index health, collection document counts, and Obsidian graph link statistics. |
| `wiki_graph_neighbors` | Read-Only | Inspect incoming backlinks and outgoing `[[WikiLinks]]` for a document or full graph summary. |
| `wiki_write_file` | Mutating | Create or overwrite a Markdown file in `wiki/` or `sources/` (auto-creates nested folders & heals WikiLinks). |
| `wiki_patch_file` | Mutating | Replace a target text block or append a section to an existing Markdown file. |
| `wiki_ingest_source` | Mutating | Ingest a raw source from `sources/` into `wiki/` preserving the exact source filename, updating `wiki/index.md` and `wiki/log.md`. |
| `wiki_file_synthesis` | Mutating | File a synthesis/comparison page into `wiki/syntheses/`, updating `wiki/index.md` and `wiki/log.md`. |
| `wiki_lint` | Mutating | Audit the vault for uningested sources, broken `[[WikiLinks]]`, orphan wiki pages, unlinked mentions, and stale claims. |
| `wiki_log` | Read/Mutate | Read recent chronological entries from `wiki/log.md` or append a new entry. |
| `wiki_schema` | Read/Mutate | Read or update `SCHEMA.md`. |
| `wiki_delete_file` | Mutating | Delete a duplicate or obsolete file in `wiki/` or `sources/`, pruning empty folders and refreshing `wiki/index.md`. |

## Running Tests & Examples

```bash
# Run unit & integration test suite
cargo test

# Run basic end-to-end workflow example
cargo run --example full_workflow

# Run Live YouTube Playlist -> Raw Source Markdown -> Karpathy LLM Wiki (via orchestrator-agent)
cp .env.live.example .env.live   # fill in G0_OPENAI_BASE_URL, G0_OPENAI_MODEL, G0_OPENAI_API_KEY
cargo run --example youtube_playlist_wiki -- "<PLAYLIST_URL>" --limit 3 --lang en,vi

# Run PDF / Document -> anydoc (GFM Markdown) -> Multi-Subagent -> Karpathy LLM Wiki
cargo run --example pdf_anydoc_wiki -- "<PDF_PATH>" --max-chars 60000
# Or run immediately with a self-generated sample technical PDF:
cargo run --example pdf_anydoc_wiki -- --sample-pdf
```

## Documentation

- [ARCHITECTURE.md](./ARCHITECTURE.md) — Technical specification, SQLite schema, QMD algorithms & Rust API reference.
- [docs/karpathy-llm-wiki.md](./docs/karpathy-llm-wiki.md) — Andrej Karpathy's original LLM Wiki concept and mapping to `llm-wiki-rs`.
- [examples/youtube_playlist_wiki/](./examples/youtube_playlist_wiki/main.rs) — Multi-subagent YouTube playlist ingestion into `sources/<domain>/<topic>/` and `wiki/`.
- [examples/pdf_anydoc_wiki/](./examples/pdf_anydoc_wiki/main.rs) — Multi-subagent PDF/document ingestion using [`anydoc`](https://crates.io/crates/anydoc) and [`orchestrator-agent`](https://crates.io/crates/orchestrator-agent).

## License

Licensed under either of **MIT** or **Apache-2.0** at your option.
