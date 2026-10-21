---
name: wiki-taxonomy
description: "Universal Karpathy LLM Wiki taxonomy, page-worthiness threshold, anti-fragmentation rules, and Obsidian [[WikiLink]] + QMD frontmatter conventions."
---

# Universal Wiki Taxonomy & Anti-Fragmentation Skill

Use this skill whenever initializing a knowledge base, planning page categories, or deciding whether a subject in a source document deserves a standalone Wiki page.

## 1. Three-Layer Architecture (`llm-wiki-rs`)
1. **Raw Sources (`sources/`)**: Immutable source of truth (articles, papers, transcripts, docs, data). Never modify raw sources after creation.
2. **Compiled Wiki (`wiki/`)**:
   - `wiki/sources/<source_rel_path>`: Source summary page strictly preserving the exact relative path and filename of the raw source in `sources/`.
   - `wiki/entities/<slug>.md`: Named proper nouns (`category: "entity"`).
   - `wiki/concepts/<slug>.md`: Abstract ideas, methods, algorithms, techniques, and frameworks (`category: "concept"`).
   - `wiki/syntheses/<slug>.md`: Cross-cutting comparative analyses, query answers, and syntheses (`category: "synthesis"`).
   - `wiki/index.md` & `wiki/log.md`: Master catalog and chronological operation log automatically maintained by `WikiEngine`.
3. **The Schema (`SCHEMA.md`)**: Authoritative domain routing and convention file read/updated via `wiki_schema`.

## 2. Always Inspect Existing Wiki Pages First
Before creating or linking any page:
1. Call `wiki_get(file="wiki/index.md")` (or `wiki_tree(scope="wiki")`) and `wiki_schema(mode="read")`.
2. Check every existing file path, slug, and title in `wiki/sources/`, `wiki/entities/`, and `wiki/concepts/`.
3. **Canonical Naming Priority**: Reuse the exact slug and title of an existing wiki page first; otherwise preserve the standard original spelling. Record localized names or abbreviations in frontmatter `aliases: [...]`.

## 3. Standalone-Page Significance Threshold (`Page-worthy: yes | no`)
- **Do NOT create standalone pages for passing mentions**:
  - A subject is `Page-worthy: yes` ONLY if it is central to the source, recurs meaningfully across sources, carries substantive technical/empirical claims, or is a reusable building block across the wiki.
  - Keep passing mentions, generic roles, chapter labels, or minor props as plain prose in the source summary page.
- **Strict Boundary Between `entities/` and `concepts/`**:
  - Never place the same subject in both `entities/` and `concepts/`.
  - **`wiki/entities/<slug>.md` (`category: "entity"`)**: Concrete named things — People, Organizations, Products, Platforms, Programming Languages, Libraries, Models, Datasets, Specific Indicators/Tools (e.g., `entities/openai.md`, `entities/rust.md`, `entities/metatrader-5.md`).
  - **`wiki/concepts/<slug>.md` (`category: "concept"`)**: Ideas, algorithms, strategies, architectural patterns, protocols, and phenomena (e.g., `concepts/reciprocal-rank-fusion.md`, `concepts/smart-chunking.md`, `concepts/dca-strategy.md`).
  - If `SCHEMA.md` defines custom domain folders (e.g., `wiki/decisions/`, `wiki/methodology/`, `wiki/findings/`), route matching pages into those schema-defined folders.

## 4. Topic-Based Folder Routing & Organization (Derived from Source)
`WikiEngine` automatically creates nested parent directories on demand (`wiki_write_file`, `wiki_ingest_source`, and `apply_ingest_file_blocks`). Follow these rules when organizing folders by topic from a source document:

1. **Inspect Existing Folder Tree First (`wiki_tree`)**:
   - Before inventing a new topic folder, call `wiki_tree(scope="wiki")` and `wiki_tree(scope="sources")` to check existing topic directories and `SCHEMA.md` routing rules.
   - **Reuse Existing Topic Folders**: Always place related sources/pages into an existing matching topic folder before creating a new one.
2. **Topic Folders for Raw Sources & Source Summaries**:
   - When saving a new raw source (`wiki_write_file(scope="sources")` or `raw_source_content` in `wiki_ingest_source`), organize it under a `kebab-case` topic path derived from the source's primary subject (max 2 levels deep):
     - `sources/<domain>/<topic>/<filename>.md` $\rightarrow$ automatically mirrors to `wiki/sources/<domain>/<topic>/<filename>.md` (preserving the exact filename).
   - If a raw source is stored flat at `sources/<filename>.md` but belongs to a specific topic folder in the wiki, pass `target_wiki_path="sources/<domain>/<topic>/<filename>.md"` to `wiki_ingest_source`.
3. **Topic Subfolders for Entities, Concepts & Syntheses**:
   - **Small / Medium Wikis (< 50 pages)**: Prefer flat `wiki/entities/<slug>.md` and `wiki/concepts/<slug>.md` so canonical slugs stay simple and unambiguous.
   - **Large / Multi-Domain Wikis or Schema-Driven Topic Routing**: Group pages into topic subfolders (`kebab-case`, max 1–2 levels under the category folder) when `SCHEMA.md` or user instructions call for topic folders:
     - `wiki/entities/<topic>/<slug>.md` (e.g., `wiki/entities/trading-platforms/metatrader-5.md`)
     - `wiki/concepts/<topic>/<slug>.md` (e.g., `wiki/concepts/risk-management/dca-strategy.md`)
     - `wiki/syntheses/<topic>/<slug>.md` (e.g., `wiki/syntheses/algo-trading/martingale-vs-grid.md`)
   - Always link using the full wiki-scoped path in Obsidian `[[WikiLink]]` syntax (e.g., `[[concepts/risk-management/dca-strategy|DCA Strategy]]`).

## 5. Topic-Based Hierarchical Tagging Standard (Obsidian + QMD Filter)
Every time you ingest or update a page from a source, extract **3 facets of `kebab-case` tags** directly from the source's subject matter and write them into both top-level `tags` (for Obsidian's nested tag tree) and `qmd.metadata` (for QMD hybrid search filtering):

1. **Facet 1 — Page Category Tag**:
   - `"wiki/source"` | `"wiki/entity"` | `"wiki/concept"` | `"wiki/synthesis"`
2. **Facet 2 — Hierarchical Domain/Topic Tags (`topic/<domain>/<subtopic>`)**:
   - Derive 1–3 hierarchical topic tags from the core subject of the source (e.g., `"topic/trading/grid-dca"`, `"topic/ai/rag"`, `"topic/rust/sqlite-fts5"`).
   - Check existing tags via `wiki_metadata` or `wiki/index.md` so you reuse established topic tag prefixes across related sources.
3. **Facet 3 — Technical / Method / Domain Tags**:
   - Include 2–5 specific subject keywords (e.g., `"mql5"`, `"atr-indicator"`, `"risk-management"`, `"martingale"`).
4. **Mirror Topic & Domain in `qmd.metadata`**:
   - Include `domain`, `topic`, `category`, and `tags` inside `qmd.metadata` so `wiki_query` can filter by exact topic or tag hierarchy:
   ```yaml
   ---
   title: "ATR Grid Spacing Strategy"
   category: "concept"
   domain: "algo-trading"
   topic: "volatility-filtering"
   summary: "Dynamic grid distance calculation using Average True Range (ATR) to prevent drawdown during high volatility."
   aliases:
     - "ATR Grid Filter"
   sources:
     - "[[sources/trading/mql5-ea/video-08-atr.md|video-08-atr.md]]"
   tags:
     - "wiki/concept"
     - "topic/trading/grid-dca"
     - "topic/trading/volatility"
     - "mql5"
     - "atr-indicator"
   qmd:
     metadata:
       category: "concept"
       domain: "algo-trading"
       topic: "volatility-filtering"
       tags:
         - "wiki/concept"
         - "topic/trading/grid-dca"
         - "topic/trading/volatility"
         - "mql5"
         - "atr-indicator"
   ---
   ```
5. **Always Use Obsidian `[[WikiLink]]` Syntax in Body**:
   - Use `[[entities/slug|Label]]`, `[[concepts/slug|Label]]`, `[[wiki/sources/...|Label]]` in the Markdown body — never use standard Markdown links `[label](path.md)` for internal wiki references.
