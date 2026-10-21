---
name: wiki-ingest
description: "Universal 4-stage source ingestion workflow (Analysis -> Source Summary -> Entity/Concept Compounding & Merge -> Cross-Reference Linking) for LLM Wiki."
---

# Universal Wiki Ingest Skill (`wiki-ingest`)

Execute this disciplined 4-stage workflow whenever ingesting one or more raw source documents (`sources/<source_rel_path>`) into `wiki/`.

## ⚠️ Mandatory Language & Technical Preservation Rule
- Write all surrounding natural-language prose, explanations, and section headings in the user's requested output language (or match the primary language of the source/wiki if unspecified).
- **Preserve proper nouns and technical identifiers in their standard original form**: organization/product/model/tool/library names, acronyms, code identifiers, SQL/API signatures, file paths, and URLs.

---

## Stage 1: Inspect Existing Wiki, Topic Folders, Tags & Analyze the Source (3-Pass Long Markdown Strategy)
1. Call `wiki_get(file="wiki/index.md")`, `wiki_tree(scope="wiki")` (and `wiki_metadata` or `wiki_schema(mode="read")` if needed) to inspect existing pages, canonical slugs, `aliases`, **existing topic folders**, and **existing tag prefixes**.
2. **3-Pass Line-Bounded Reading (`from_line` / `max_lines` — `javis-os` Discipline)**:
   - **Pass 1 (Initial Window & Structure Map)**: Call `wiki_get(file="sources/<source_rel_path>", from_line=1, max_lines=110)`.
   - **Pass 2 (Sequential Line Windows for Long Sources)**: If the output ends with a continuation footer (`… [Đã đọc dòng X-Y trên tổng Z dòng... gọi lại wiki_get với from_line=Y+1]`), call `wiki_get` with `from_line=Y+1` sequentially until all sections are covered. **Never** call `wiki_get` twice with the same `from_line`.
   - **Pass 3 (Coverage Check)**: Ensure every major section/heading of the source is represented before finalizing pages.
3. Perform a structured analysis covering:
   - **Topic & Folder Classification (Derived from Source)**:
     - Identify the primary `domain` (e.g., `algo-trading`, `ai-engineering`, `distributed-systems`) and `topic` (e.g., `grid-dca`, `hybrid-search`, `volatility-filtering`) of the source.
     - Determine target folder routing: reuse an existing topic folder from `wiki_tree` first, or propose a clean `kebab-case` topic folder (max 2 levels deep) if `SCHEMA.md` or user preferences organize pages by topic subfolders.
   - **3-Facet Topic Tags (Derived from Source)**:
     - Prepare hierarchical Obsidian + QMD tags:
       1. Category tag (`"wiki/source"`, `"wiki/entity"`, `"wiki/concept"`)
       2. Hierarchical topic tags (`"topic/<domain>/<subtopic>"`, e.g., `"topic/trading/grid-dca"`)
       3. Specific subject/technology tags (`"<tech-or-keyword>"`, e.g., `"mql5"`, `"atr-indicator"`)
   - **Key Entities (`Page-worthy: yes | no`)**: Identify candidate named things. Only mark `Page-worthy: yes` if central, recurring, or carrying substantive claims. Reuse existing slugs from `wiki/index.md`!
   - **Key Concepts (`Page-worthy: yes | no`)**: Identify core methods, algorithms, strategies, or frameworks.
   - **Main Arguments, Findings & Verbatim Structured Data**:
     - Keep claims, metrics, and limitations strictly attached to the exact subject they describe (never transfer claims between different models/methods just because they share keywords).
     - Preserve SQL DDL (`CREATE TABLE`), code snippets, formulas, API signatures, configuration parameters, and benchmark tables **verbatim** in fenced code blocks or Markdown tables.
   - **Connections & Contradictions**: Note which existing wiki pages are strengthened, extended, or contradicted (`> [!WARNING] Contradiction / Caveat`).

---

## Stage 2: Compile the Source Summary Page (`wiki_ingest_source`)
Call `wiki_ingest_source` for `source_rel_path` (do NOT invent a random filename; `wiki_ingest_source` automatically preserves the exact source filename under `wiki/sources/<source_rel_path>`, or under `target_wiki_path="sources/<domain>/<topic>/<filename>.md"` if routing flat sources into a topic subfolder):
- `source_rel_path`: `"<source_rel_path>"`
- `title`: Human-readable title
- `one_line_summary`: Concise 1-sentence summary for `wiki/index.md`
- `tags`: Hierarchical topic tags (`["topic/<domain>/<subtopic>", "<tech>", "<keyword>"]`; `"wiki/source"` is added automatically)
- `related_entities`: Canonical `entities/<slug>` (or `entities/<topic>/<slug>`) stems (`Page-worthy: yes` only)
- `related_concepts`: Canonical `concepts/<slug>` (or `concepts/<topic>/<slug>`) stems (`Page-worthy: yes` only)
- `wiki_markdown_body`: A comprehensive, deeply structured Markdown article containing:
  1. **Context & Core Thesis**: Purpose of the source and how it connects to existing `[[wiki/sources/...]]` pages.
  2. **Detailed Technical / Factual Breakdown**: All core claims, rules, parameters, code blocks, SQL/schemas, or formulas preserved verbatim, with inline `[[entities/<slug>|Label]]` and `[[concepts/<slug>|Label]]` links.
  3. **Empirical Findings / Data / Metrics Table**: Markdown table summarizing experiments, configurations, or comparisons when present.
  4. **Key Takeaways, Caveats & Contradictions**: Include a `> [!WARNING] Contradiction / Caveat` callout block if there are internal caveats or conflicts with other wiki pages.
  5. **Architecture / Process Diagram**: Valid multi-line `flowchart TD` or `flowchart LR` Mermaid diagram with double-quoted node labels (`A["..."] --> B["..."]`) when applicable.

---

## Stage 3: Create or Merge Entity & Concept Pages (`wiki_write_file` / `wiki_patch_file`)
For every canonical slug in `related_entities` (`entities/<slug>.md`) and `related_concepts` (`concepts/<slug>.md`):
1. **If the page does NOT exist in `wiki/index.md`**:
   - Call `wiki_write_file(scope="wiki", rel_path="entities/<slug>.md" | "concepts/<slug>.md", content=...)`.
   - **Frontmatter `aliases`**: Include `aliases: [...]` with abbreviations and English/Vietnamese synonyms so future ingests and QMD searches automatically deduplicate.
   - **Contextual Retrieval Positioning Rule**: The opening 1–2 sentences MUST be self-contained (stating what the entity/concept is, which domain/system it belongs to, and when it is used), followed by detailed synthesis from the source, and a backlink section citing `[[wiki/sources/<source_rel_path>|<Title>]]` and `[[sources/<source_rel_path>|<filename>]]`.
2. **If the page ALREADY exists in `wiki/index.md` (Page Merge Rules)**:
   - Under tight context budgets, prefer calling `wiki_patch_file` with `append_text` directly to add the new source's contribution without reloading the entire file into context.
   - When performing a full merge (`wiki_get` + `wiki_write_file`):
     - Preserve every factual claim from both the existing page and the new source (never drop prior source contributions).
     - Union the `sources`, `tags`, and `aliases` frontmatter lists while keeping the original `title` and `category` locked.
     - Keep subject/source boundaries exact; if the new source contradicts an existing claim, keep both claims and add a `> [!WARNING] Contradiction / Caveat` callout citing both `[[sources/...]]` files.
     - Never refer to "old version" vs "new version" in prose or headings — write one coherent, unified article.

---

## Stage 4: Bidirectional Cross-Linking Across Related Sources
- If `wiki/index.md` contains existing source summary pages in `wiki/sources/` that directly relate to this new source, call `wiki_patch_file` on those existing pages (`append_text`) to add a bidirectional `[[wiki/sources/<source_rel_path>|<Title>]]` link.
