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

## 4. Mandatory YAML Frontmatter & Obsidian `[[WikiLink]]` Rules
- Every wiki page MUST start with `---` on line 1 (no outer code fence) and include:
  - `title`, `category`, `summary`, `aliases`, `sources` (linking to `[[sources/...|filename.md]]`), `tags`, and `qmd.metadata` (`category`, `tags`).
- Always use Obsidian `[[WikiLink]]` syntax (`[[entities/slug|Label]]`, `[[concepts/slug|Label]]`, `[[wiki/sources/...|Label]]`) in the Markdown body — never use standard Markdown links `[label](path.md)` for internal wiki references.
