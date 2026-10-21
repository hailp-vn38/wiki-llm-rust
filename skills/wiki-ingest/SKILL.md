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

## Stage 1: Inspect Existing Wiki & Analyze the Source
1. Call `wiki_get(file="wiki/index.md")` (and `wiki_schema(mode="read")` if needed) to inspect existing pages and canonical slugs.
2. Read the raw source via `wiki_get(file="sources/<source_rel_path>")` (or chunk-by-chunk using `from_line` / `max_lines` for very long sources).
3. Perform a structured analysis covering:
   - **Key Entities (`Page-worthy: yes | no`)**: Identify candidate named things. Only mark `Page-worthy: yes` if central, recurring, or carrying substantive claims. Reuse existing slugs from `wiki/index.md`!
   - **Key Concepts (`Page-worthy: yes | no`)**: Identify core methods, algorithms, strategies, or frameworks.
   - **Main Arguments, Findings & Verbatim Structured Data**:
     - Keep claims, metrics, and limitations strictly attached to the exact subject they describe (never transfer claims between different models/methods just because they share keywords).
     - Preserve SQL DDL (`CREATE TABLE`), code snippets, formulas, API signatures, configuration parameters, and benchmark tables **verbatim** in fenced code blocks or Markdown tables.
   - **Connections & Contradictions**: Note which existing wiki pages are strengthened, extended, or contradicted (`> [!WARNING] Contradiction / Caveat`).

---

## Stage 2: Compile the Source Summary Page (`wiki_ingest_source`)
Call `wiki_ingest_source` for `source_rel_path` (do NOT invent a random filename; `wiki_ingest_source` automatically preserves the exact source filename under `wiki/sources/<source_rel_path>`):
- `source_rel_path`: `"<source_rel_path>"`
- `title`: Human-readable title
- `one_line_summary`: Concise 1-sentence summary for `wiki/index.md`
- `tags`: Relevant domain tags
- `related_entities`: Canonical `entities/<slug>` stems (`Page-worthy: yes` only)
- `related_concepts`: Canonical `concepts/<slug>` stems (`Page-worthy: yes` only)
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
   - The opening paragraph MUST define what the entity/concept is, followed by detailed synthesis from the source, and a backlink section citing `[[wiki/sources/<source_rel_path>|<Title>]]` and `[[sources/<source_rel_path>|<filename>]]`.
2. **If the page ALREADY exists in `wiki/index.md` (Page Merge Rules)**:
   - Call `wiki_get` to read the existing page first.
   - Merge the new source-backed material into the existing page via `wiki_write_file` or `wiki_patch_file`:
     - Preserve every factual claim from both the existing page and the new source (never drop prior source contributions).
     - Union the `sources`, `tags`, and `aliases` frontmatter lists while keeping the original `title` and `category` locked.
     - Keep subject/source boundaries exact; if the new source contradicts an existing claim, keep both claims and add a `> [!WARNING] Contradiction / Caveat` callout citing both `[[sources/...]]` files.
     - Never refer to "old version" vs "new version" in prose or headings — write one coherent, unified article.

---

## Stage 4: Bidirectional Cross-Linking Across Related Sources
- If `wiki/index.md` contains existing source summary pages in `wiki/sources/` that directly relate to this new source, call `wiki_patch_file` on those existing pages (`append_text`) to add a bidirectional `[[wiki/sources/<source_rel_path>|<Title>]]` link.
