---
name: wiki-lint
description: "Universal structural + semantic health-check, orphan/broken-link repair, non-destructive [[WikiLink]] enrichment, and deduplication workflow for LLM Wiki."
---

# Universal Wiki Lint, Repair & Enrich Skill (`wiki-lint`)

Use this skill periodically or after batch ingests to keep the knowledge base healthy, richly interlinked, and free of contradictions or duplicates.

## Step 1: Run Structural Health-Check (`wiki_lint`)
Call `wiki_lint` to inspect deterministic structural issues across the vault:
1. **`uningested_sources`**: Raw files in `sources/` that do not yet have a corresponding summary page in `wiki/sources/`.
   - *Fix*: Read the raw source and ingest it via `wiki_ingest_source` (following `wiki-ingest`).
2. **`broken_links`**: `[[WikiLinks]]` pointing to non-existent pages.
   - *Fix*: Either create the missing target page via `wiki_write_file` (if `Page-worthy: yes`) or fix the broken link slug in the source page via `wiki_patch_file`.
3. **`orphan_wiki_pages`**: Pages in `wiki/` with zero inbound `[[WikiLinks]]`.
   - *Fix*: Add inbound `[[WikiLinks]]` from relevant `wiki/sources/`, `wiki/entities/`, or `wiki/syntheses/` pages via `wiki_patch_file`, or merge/delete redundant pages.
4. **`unlinked_mentions`**: Pages that mention an existing entity or concept title in plain text without wrapping it in `[[WikiLinks]]`.
   - *Fix*: Use `wiki_patch_file` (`target_text` -> `replacement_text`) to wrap the first mention in `[[entities/slug|Term]]` or `[[concepts/slug|Term]]` without altering surrounding prose or YAML frontmatter.
5. **`missing_frontmatter_pages`**: Pages lacking `title`, `category`, `summary`, or `tags`.
   - *Fix*: Repair the YAML frontmatter via `wiki_write_file` or `wiki_patch_file`.

## Step 2: Semantic Quality Audit & Deduplication
1. Inspect `wiki/index.md` and `wiki_tree(scope="wiki")` for **synonymous duplicate pages** (e.g., two entity/concept pages covering the same subject under different slugs).
   - *Fix*: Merge all unique claims, code blocks, and `sources` into the canonical page (`wiki_write_file`), update inbound links across affected pages (`wiki_patch_file`), and delete the duplicate file.
2. Check cross-referenced pages for **unflagged contradictions or stale claims**:
   - *Fix*: When two sources disagree, ensure the affected entity/concept page documents both perspectives and highlights the tension in a `> [!WARNING] Contradiction / Caveat` callout block citing both `[[sources/...]]` files.

## Step 3: Verify Clean State
- Re-run `wiki_lint` after applying fixes until `is_healthy: true` (`uningested_sources = 0`, `broken_links = 0`, `orphan_wiki_pages = 0`) and log the maintenance pass via `wiki_log(mode="append", action="lint", ...)`.
