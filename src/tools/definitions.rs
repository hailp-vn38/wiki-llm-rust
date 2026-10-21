use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Definition of a tool exposed to an LLM Agent (compatible with OpenAI, Anthropic, Gemini, and MCP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Return the complete list of 12 LLM tools for operating on the QMD + Obsidian LLM Wiki.
pub fn build_llm_tool_definitions() -> Vec<LlmToolSpec> {
    vec![
        LlmToolSpec {
            name: "wiki_tree".to_string(),
            description: "Get the directory and file tree structure of `sources`, `wiki`, or the entire knowledge base (`all`). Returns both a structured JSON tree and a compact ASCII tree.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["scope"],
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["all", "sources", "wiki"],
                        "description": "Which folder tree to inspect ('all', 'sources', or 'wiki')."
                    },
                    "sub_path": {
                        "type": "string",
                        "description": "Optional relative subfolder inside the chosen scope (e.g. 'docs/flow' or 'concepts')."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Optional maximum traversal depth."
                    },
                    "include_metadata": {
                        "type": "boolean",
                        "default": true,
                        "description": "Include short #docid, title, and tags for each markdown file."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_query".to_string(),
            description: "Search across `wiki` and `sources` using QMD Hybrid Search (BM25 full-text + Vector semantic search + Reciprocal Rank Fusion + optional Reranking and recursive Metadata Filtering).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Simple search query string (if `searches` is omitted)."
                    },
                    "searches": {
                        "type": "array",
                        "description": "Typed QMD sub-queries (`lex` for BM25 keywords, `vec` for semantic search, `hyde` for hypothetical document). First sub-query gets 2x RRF weight.",
                        "items": {
                            "type": "object",
                            "required": ["type", "query"],
                            "properties": {
                                "type": { "type": "string", "enum": ["lex", "vec", "hyde"] },
                                "query": { "type": "string" }
                            }
                        }
                    },
                    "collections": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["wiki", "sources"] },
                        "description": "Filter by collection names (defaults to searching both 'wiki' and 'sources')."
                    },
                    "filter": {
                        "type": "object",
                        "description": "Recursive QMD metadata filter AST (operators: and, or, not, eq, ne, gt, gte, lt, lte, in, nin, all, contains, prefix, suffix, exists, type)."
                    },
                    "intent": {
                        "type": "string",
                        "description": "Optional disambiguation context."
                    },
                    "limit": {
                        "type": "integer",
                        "default": 10
                    },
                    "min_score": {
                        "type": "number",
                        "default": 0.0
                    },
                    "rerank": {
                        "type": "boolean",
                        "default": true
                    },
                    "explain": {
                        "type": "boolean",
                        "default": false
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_get".to_string(),
            description: "Retrieve a single document by its relative path (e.g. 'wiki/entities/rust.md', 'docs/flow/lib.md'), file stem, or QMD #docid (e.g. '#a1b2c3'). Supports `:from:count` suffix or explicit `from_line`/`max_lines`.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["file"],
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Path, #docid, or `path:from:count`."
                    },
                    "from_line": {
                        "type": "integer",
                        "description": "1-indexed start line."
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Maximum number of lines to return."
                    },
                    "line_numbers": {
                        "type": "boolean",
                        "default": true,
                        "description": "Prefix lines with 1-indexed line numbers."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_multi_get".to_string(),
            description: "Batch retrieve multiple documents by glob pattern (e.g. 'wiki/entities/*.md') or comma-separated list of paths/#docids.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern or comma-separated list of file paths / #docids."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "default": 65536,
                        "description": "Skip files larger than N bytes."
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Maximum lines per file."
                    },
                    "line_numbers": {
                        "type": "boolean",
                        "default": true
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_metadata".to_string(),
            description: "Discover available YAML frontmatter metadata keys, value types, and document counts to construct accurate search filters.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "collections": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["wiki", "sources"] }
                    },
                    "match": {
                        "type": "object",
                        "description": "Filter condition over metadata entries (`field` = 'key' or 'value')."
                    },
                    "filter": {
                        "type": "object",
                        "description": "Filter narrowing which documents are counted."
                    },
                    "key_limit": { "type": "integer", "default": 50 },
                    "value_limit": { "type": "integer", "default": 10 },
                    "min_count": { "type": "integer", "default": 1 }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_status".to_string(),
            description: "Return index health, collection document counts, Obsidian Graph link counts, path contexts, and top metadata keys.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
        LlmToolSpec {
            name: "wiki_write_file".to_string(),
            description: "Create or overwrite a Markdown file inside `wiki` or `sources`. Automatically creates any nested parent folders (e.g. 'docs/flow/lib.md' creates `docs` and `docs/flow`). Indexes the file into SQLite FTS5, chunks, and Obsidian Graph View.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["scope", "rel_path", "content"],
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["wiki", "sources"],
                        "description": "Target collection folder ('wiki' or 'sources')."
                    },
                    "rel_path": {
                        "type": "string",
                        "description": "Relative path inside the collection (e.g. 'docs/flow/lib.md' or 'entities/rust.md')."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full Markdown content (with YAML frontmatter and [[WikiLinks]])."
                    },
                    "update_index_md": {
                        "type": "boolean",
                        "default": true,
                        "description": "If scope is 'wiki', automatically refresh `wiki/index.md`."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_patch_file".to_string(),
            description: "Modify an existing Markdown file in `wiki` or `sources` by replacing a target text block or appending a new section.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["scope", "rel_path"],
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["wiki", "sources"]
                    },
                    "rel_path": {
                        "type": "string"
                    },
                    "target_text": {
                        "type": "string",
                        "description": "Exact text substring to replace (if performing replacement)."
                    },
                    "replacement_text": {
                        "type": "string",
                        "description": "Replacement text."
                    },
                    "append_text": {
                        "type": "string",
                        "description": "Text to append to the end of the file."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_ingest_source".to_string(),
            description: "Ingest a raw source file from `sources/` into `wiki/`. Strictly preserves the EXACT filename from `sources/` (e.g. 'docs/flow/lib.md' creates 'wiki/sources/docs/flow/lib.md', never an auto-generated ID), links them for Obsidian Graph View, updates `wiki/index.md`, and logs to `wiki/log.md`.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["source_rel_path", "title", "one_line_summary", "wiki_markdown_body"],
                "properties": {
                    "source_rel_path": {
                        "type": "string",
                        "description": "Relative path of the source inside `sources/` (e.g. 'docs/flow/lib.md')."
                    },
                    "target_wiki_path": {
                        "type": "string",
                        "description": "Optional: existing wiki path (e.g. 'sources/docs/flow/lib.md' or 'entities/rust.md') to update in-place instead of creating a new wiki file. When omitted, defaults to preserving the exact source filename."
                    },
                    "raw_source_content": {
                        "type": "string",
                        "description": "Optional: if provided, writes/creates the raw source file in `sources/` first before ingesting."
                    },
                    "title": {
                        "type": "string",
                        "description": "Human-readable title of the wiki summary page."
                    },
                    "one_line_summary": {
                        "type": "string",
                        "description": "Concise one-line summary registered in `wiki/index.md`."
                    },
                    "wiki_markdown_body": {
                        "type": "string",
                        "description": "Synthesized Markdown body with Obsidian [[WikiLinks]]."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "related_entities": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "related_concepts": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_graph_neighbors".to_string(),
            description: "Inspect Obsidian Graph View connections (backlinks and outgoing [[WikiLinks]]) for a specific document, or retrieve the full graph summary.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "rel_path": {
                        "type": "string",
                        "description": "Optional document path or stem to inspect backlinks and outgoing links. If omitted, returns overall graph summary."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_lint".to_string(),
            description: "Health-check the wiki: find un-ingested raw sources, broken [[WikiLinks]], orphan wiki pages with no inbound links, unlinked entity/concept mentions, and missing frontmatter.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
        LlmToolSpec {
            name: "wiki_log".to_string(),
            description: "Read recent chronological entries from `wiki/log.md` or append a new entry (`## [YYYY-MM-DD HH:MM] action | title`).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["read", "append"],
                        "default": "read"
                    },
                    "limit": {
                        "type": "integer",
                        "default": 10,
                        "description": "Number of recent log entries to read when mode='read'."
                    },
                    "action": {
                        "type": "string",
                        "description": "Action label ('ingest', 'query', 'lint', 'update') when mode='append'."
                    },
                    "title": {
                        "type": "string",
                        "description": "Log entry title when mode='append'."
                    },
                    "details": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Bullet point details when mode='append'."
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_file_synthesis".to_string(),
            description: "File a valuable query answer, comparison table, analysis, or Marp slide deck back into `wiki/syntheses/` as a permanent compounding wiki page, updating `wiki/index.md` and appending a `query` entry to `wiki/log.md`.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["rel_path", "query", "title", "one_line_summary", "markdown_body"],
                "properties": {
                    "rel_path": {
                        "type": "string",
                        "description": "Target filename or relative path inside `wiki/syntheses/` (e.g. 'rag-vs-llm-wiki.md')."
                    },
                    "query": {
                        "type": "string",
                        "description": "The user question or research topic that produced this synthesis."
                    },
                    "title": {
                        "type": "string",
                        "description": "Human-readable title of the synthesis page."
                    },
                    "one_line_summary": {
                        "type": "string",
                        "description": "Concise one-line summary registered in `wiki/index.md`."
                    },
                    "markdown_body": {
                        "type": "string",
                        "description": "Synthesized Markdown content with [[WikiLinks]] citations."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            }),
        },
        LlmToolSpec {
            name: "wiki_schema".to_string(),
            description: "Read or update `SCHEMA.md` (the key configuration file defining wiki conventions, categories, frontmatter rules, and workflows co-evolved by the user and LLM).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["read", "write"],
                        "default": "read"
                    },
                    "content": {
                        "type": "string",
                        "description": "New content for `SCHEMA.md` when mode='write'."
                    }
                }
            }),
        },
    ]
}
