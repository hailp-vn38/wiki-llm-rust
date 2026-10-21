---
name: karpathy-wiki-taxonomy
description: "Canonical taxonomy, anti-fragmentation rules, and Obsidian [[WikiLink]] linking conventions for Karpathy LLM Wiki."
---

# Karpathy Wiki Taxonomy & Anti-Fragmentation Skill

Follow these strict rules to maintain a clean, compounding, non-fragmented knowledge base:

## 1. Always Inspect Existing Wiki Pages First
Before creating or referencing any `entities/`, `concepts/`, or `wiki/sources/` page:
1. Call `wiki_get` with `file="wiki/index.md"` (or `wiki_tree` with `scope="wiki"`).
2. Check every existing file path and title in `wiki/sources/youtube/`, `wiki/entities/`, and `wiki/concepts/`.
3. **REUSE existing slugs** whenever a concept or entity already exists in `wiki/index.md`!

## 2. Strict Anti-Fragmentation Rules (Chống Phân Mảnh)
- **One Canonical Slug per Entity or Concept**:
  - NEVER create synonymous duplicates. Examples:
    - Use `entities/metatrader-5.md` (with `aliases: ["MT5", "MetaTrader", "MetaTrader 5"]` in frontmatter) — NEVER create separate `mt5.md`, `metatrader.md`, and `metatrader-5.md`.
    - Use `entities/mql5.md` (with `aliases: ["MQL", "MQL5"]`) — NEVER create both `mql.md` and `mql5.md`.
    - Use `concepts/dca-strategy.md` — NEVER create 5 separate DCA concept files (`chien-luoc-dca.md`, `phuong-phap-dca.md`, `cong-thuc-dca.md`, `dollar-cost-averaging.md`).
- **Clear Boundary Between `entities/` and `concepts/`**:
  - NEVER place the same topic in both `entities/` and `concepts/`.
  - **`wiki/entities/<slug>.md` (`category: "entity"`)**: Concrete proper nouns — People (e.g., `nick-shawn`), Platforms/Tools (e.g., `metatrader-5`), Languages/Libraries (e.g., `mql5`, `expert-advisor`), Specific Indicators (`atr-indicator`), Brokers/Datasets (`ic-markets`, `tick-data`).
  - **`wiki/concepts/<slug>.md` (`category: "concept"`)**: Methodologies, strategies, mechanisms, and engineering patterns (e.g., `dca-strategy`, `martingale-grid`, `support-and-resistance`, `multi-symbol-oop`, `risk-and-drawdown-management`).
- **Keep Taxonomy Compact & High-Density**:
  - Across a playlist of 3–6 videos, maintain **4 to 7 canonical `entities/`** and **4 to 7 canonical `concepts/`** so that every page accumulates links from multiple videos (true compounding artifact).

## 3. Link to Existing Video Wiki Pages Immediately
- When ingesting Video $N$ and `wiki/index.md` shows that Video $1 \dots N-1$ already exist in `wiki/sources/youtube/`, you MUST:
  1. Include inline `[[wiki/sources/youtube/<existing-stem>|<Title>]]` links inside the new video's wiki page explaining how this video builds on, tests, or refines the existing video(s).
  2. Call `wiki_patch_file` on the existing video wiki page(s) to add a forward/backward `[[wiki/sources/youtube/<new-stem>|<New Title>]]` link.
  3. Call `wiki_write_file` or `wiki_patch_file` on each referenced `entities/<slug>.md` and `concepts/<slug>.md` so it defines the entity/concept properly and lists all videos where it appears.

