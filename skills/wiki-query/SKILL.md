---
name: wiki-query
description: "Universal QMD Hybrid Search, Obsidian Graph traversal, retrieval modes (Standard, Smart, Faithful), and Synthesis filing workflow for LLM Wiki."
---

# Universal Wiki Query & Synthesis Skill (`wiki-query`)

Use this skill when answering questions, exploring connections, or synthesizing multi-source research across the knowledge base.

## 1. Retrieval Strategy (`WikiEngine` + QMD Tools)
Choose the right tool for the question:
1. **Index-First Navigation (`wiki_get(file="wiki/index.md")`)**:
   - Inspect `wiki/index.md` first to see available `wiki/sources/`, `wiki/entities/`, `wiki/concepts/`, and `wiki/syntheses/` pages and their one-line summaries.
2. **QMD Hybrid Search (`wiki_query`)**:
   - Use typed sub-queries for high-precision retrieval:
     - `lex`: BM25 exact keyword/identifier match (e.g., function names, error codes, metrics).
     - `vec`: Semantic similarity search.
     - `hyde`: Hypothetical document passage matching the ideal answer.
   - Put the highest-signal sub-query first (it receives $2\times$ RRF weight).
   - Use `filter` (QMD metadata AST) when narrowing by `category`, `tags`, or `source_file` (discover available keys first with `wiki_metadata` if unsure).
3. **Graph Relationship Traversal (`wiki_graph_neighbors`)**:
   - When the user asks how two or more entities/concepts relate, depend on each other, or evolve across sources, call `wiki_graph_neighbors(rel_path="...")` to inspect incoming backlinks and outgoing `[[WikiLinks]]`.
4. **Full Page & Line-Range Reading (`wiki_get` / `wiki_multi_get`)**:
   - Read the relevant wiki pages (and raw `sources/` files when exact quotes or code details are needed) before synthesizing your answer.

## 2. Three Retrieval Discipline Modes
- **Standard Mode**: Combine compiled `wiki/` pages and `sources/` excerpts with `[[WikiLink]]` citations.
- **Smart Mode (Bounded Evidence Loop)**:
  - After each tool call, identify the exact unresolved evidence gap.
  - Prefer following `[[WikiLinks]]` from already discovered pages (`wiki_get` / `wiki_graph_neighbors`) before issuing broader search queries.
  - Stop searching as soon as the gathered evidence supports a complete, well-cited answer.
- **Faithful Mode (Strict Raw-Source Grounding)**:
  - Search and read ONLY raw files under `sources/` (`collections: ["sources"]`).
  - Quote key passages verbatim and cite `[[sources/path/to/file.md|file.md]]` beside every factual claim. Do not rely on unverified claims or external assumptions.

## 3. Compounding Explorations: File Valuable Answers Back (`wiki_file_synthesis`)
Karpathy's core rule: *"Good answers shouldn't disappear into chat history."*
When a query produces a deep comparison, architectural synthesis, multi-source table, or thesis:
1. Call `wiki_file_synthesis` to save it permanently under `wiki/syntheses/<slug>.md` with:
   - Executive thesis & core conclusions
   - Markdown comparison table across sources/entities/methods
   - Contradictions & caveats (`> [!WARNING] Contradiction / Caveat`)
   - Mermaid diagram (`flowchart LR` or `flowchart TD` with double-quoted labels `A["..."]`)
   - Rich `[[entities/...]]`, `[[concepts/...]]`, `[[wiki/sources/...]]`, and `[[sources/...]]` links
2. Call `wiki_patch_file` on at least one related `wiki/sources/` or `wiki/entities/` page to link to `[[wiki/syntheses/<slug>|<Title>]]` so the synthesis page is connected in Obsidian's Graph View (not an orphan).
