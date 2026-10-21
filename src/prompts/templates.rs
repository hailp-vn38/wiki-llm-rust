//! Standard Wiki Templates (`SCHEMA.md` & `purpose.md`) for LLM Wiki.
//!
//! Ported from `nashsu/llm_wiki` (`src/lib/templates.ts`) and adapted to `llm-wiki-rs`'s
//! Obsidian + QMD Metadata architecture (`sources/`, `wiki/sources/`, `wiki/entities/`,
//! `wiki/concepts/`, `wiki/syntheses/`, `wiki/index.md`, `wiki/log.md`).

use serde::{Deserialize, Serialize};

/// Template preset identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WikiTemplateId {
    Research,
    Reading,
    Personal,
    Business,
    General,
}

impl WikiTemplateId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Reading => "reading",
            Self::Personal => "personal",
            Self::Business => "business",
            Self::General => "general",
        }
    }
}

/// A domain-specific Wiki schema and purpose preset.
#[derive(Debug, Clone, Serialize)]
pub struct WikiTemplate {
    pub id: WikiTemplateId,
    pub name: &'static str,
    pub description: &'static str,
    pub extra_dirs: &'static [&'static str],
    pub schema_md: String,
    pub purpose_md: String,
}

const BASE_SCHEMA_TYPES: &str = "\
| source_summary | wiki/sources/ | Deep technical/factual summary preserving the exact source filename |
| entity | wiki/entities/ | Named things (people, tools, platforms, organizations, datasets, models) |
| concept | wiki/concepts/ | Ideas, algorithms, strategies, techniques, phenomena, frameworks |
| synthesis | wiki/syntheses/ | Cross-cutting comparisons, syntheses, query answers, and conclusions |
| index | wiki/index.md | Master content-oriented catalog of all wiki pages (auto-maintained) |
| log | wiki/log.md | Chronological append-only operation log (`init`, `ingest`, `query`, `lint`, `update`) |";

const BASE_NAMING: &str = "\
- Files: `kebab-case.md` (or preserve CJK characters for Chinese/Japanese/Korean titles)
- Source summaries: MUST preserve the exact relative path and filename of the raw source in `sources/` (e.g., `sources/docs/flow/lib.md` -> `wiki/sources/docs/flow/lib.md`)
- Entities: match canonical official name where possible (e.g., `entities/openai.md`, `entities/metatrader-5.md`) — never create synonymous duplicates
- Concepts: descriptive noun phrases (e.g., `concepts/chain-of-thought.md`, `concepts/dca-strategy.md`)
- Syntheses: descriptive comparison or question slug (e.g., `syntheses/rag-vs-llm-wiki.md`)";

const BASE_FRONTMATTER: &str = "\
All wiki pages MUST start with YAML frontmatter on line 1 (`---`) compatible with Obsidian & QMD metadata indexing:

```yaml
---
title: \"Human-readable title\"
category: \"source_summary\" | \"entity\" | \"concept\" | \"synthesis\"
summary: \"Concise one-line description for wiki/index.md\"
aliases: []
tags:
  - \"wiki/entity\"
sources:
  - \"[[sources/path/to/file.md|file.md]]\"
updated_at: \"YYYY-MM-DDTHH:MM:SSZ\"
qmd:
  metadata:
    category: \"entity\"
    tags:
      - \"wiki/entity\"
---
```";

const BASE_CROSSREF: &str = "\
- Always use Obsidian `[[WikiLink]]` syntax (`[[wiki/entities/slug|Display]]`, `[[entities/slug|Display]]`, or `[[slug]]`) in the Markdown body — never use standard Markdown links `[text](path.md)` for internal wiki links
- Every entity and concept page must link back to the `[[wiki/sources/...]]` and `[[sources/...]]` pages where it appears
- Synthesis pages must cite all contributing source summaries, entities, and concepts via `[[WikiLinks]]`";

const BASE_CONTRADICTION: &str = "\
When sources contradict each other or a claim has critical caveats:
1. Preserve both claims with exact source attribution (never silently drop or overwrite conflicting evidence)
2. Add an explicit Obsidian callout block in the relevant concept, entity, or source summary page:
   ```markdown
   > [!WARNING] Contradiction / Caveat
   > Explain the conflicting evidence and cite both sources via `[[sources/...]]`.
   ```
3. Resolve or compare the tension in a `wiki/syntheses/` page once sufficient evidence exists";

/// Get a built-in `WikiTemplate` by its `WikiTemplateId`.
pub fn get_wiki_template(id: WikiTemplateId) -> WikiTemplate {
    match id {
        WikiTemplateId::Research => WikiTemplate {
            id,
            name: "Research Deep-Dive",
            description: "Deep-dive research with hypothesis tracking, methodology notes, and empirical findings",
            extra_dirs: &["wiki/methodology", "wiki/findings", "wiki/thesis"],
            schema_md: format!(
                "# Wiki Schema — Research Deep-Dive\n\n\
                 ## Page Types\n\n\
                 | Category | Directory | Purpose |\n\
                 |----------|-----------|---------|\n\
                 {BASE_SCHEMA_TYPES}\n\
                 | thesis | wiki/thesis/ | Working hypothesis and its evolution over time |\n\
                 | methodology | wiki/methodology/ | Research methods, protocols, and study designs |\n\
                 | finding | wiki/findings/ | Individual empirical results, benchmarks, or observations |\n\n\
                 ## Naming Conventions\n\n\
                 {BASE_NAMING}\n\
                 - Theses: hypothesis as slug (e.g., `scaling-improves-reasoning.md`)\n\
                 - Methodologies: method name (e.g., `ablation-study.md`)\n\
                 - Findings: descriptive slug (e.g., `larger-models-better-few-shot.md`)\n\n\
                 ## Frontmatter Rules\n\n\
                 {BASE_FRONTMATTER}\n\n\
                 ## Cross-referencing Rules\n\n\
                 {BASE_CROSSREF}\n\n\
                 ## Contradiction Handling\n\n\
                 {BASE_CONTRADICTION}\n\n\
                 ## Research-Specific Conventions\n\n\
                 - Keep thesis pages updated as evidence accumulates — they are living documents\n\
                 - Preserve structured technical data verbatim (SQL DDL, code blocks, mathematical formulas, benchmark tables)\n\
                 - Distinguish clearly between direct empirical evidence and author inference\n"
            ),
            purpose_md: "# Project Purpose — Research Deep-Dive\n\n\
                         ## Research Question\n\n\
                         > State the central question this research aims to answer.\n\n\
                         ## Hypothesis / Working Thesis\n\n\
                         > Current best hypothesis (update as evidence accumulates).\n\n\
                         ## Sub-questions\n\n\
                         1. \n\
                         2. \n\n\
                         ## Scope\n\n\
                         - **In scope:** \n\
                         - **Out of scope:** \n"
                .to_string(),
        },
        WikiTemplateId::Reading => WikiTemplate {
            id,
            name: "Reading & Book Companion",
            description: "Track a book or course's characters, themes, plot threads, and chapter notes",
            extra_dirs: &["wiki/characters", "wiki/themes", "wiki/plot-threads", "wiki/chapters"],
            schema_md: format!(
                "# Wiki Schema — Reading Companion\n\n\
                 ## Page Types\n\n\
                 | Category | Directory | Purpose |\n\
                 |----------|-----------|---------|\n\
                 {BASE_SCHEMA_TYPES}\n\
                 | character | wiki/characters/ | People and figures in the book/series |\n\
                 | theme | wiki/themes/ | Recurring ideas, motifs, and symbolic threads |\n\
                 | plot-thread | wiki/plot-threads/ | Storylines or narrative arcs being tracked |\n\
                 | chapter | wiki/chapters/ | Per-chapter notes and summaries |\n\n\
                 ## Naming Conventions\n\n\
                 {BASE_NAMING}\n\n\
                 ## Frontmatter Rules\n\n\
                 {BASE_FRONTMATTER}\n\n\
                 ## Cross-referencing Rules\n\n\
                 {BASE_CROSSREF}\n\n\
                 ## Contradiction Handling\n\n\
                 {BASE_CONTRADICTION}\n"
            ),
            purpose_md: "# Project Purpose — Reading Companion\n\n\
                         ## Work Details\n\n\
                         - **Title:** \n\
                         - **Author:** \n\n\
                         ## Key Themes & Questions to Track\n\n\
                         1. \n\
                         2. \n"
                .to_string(),
        },
        WikiTemplateId::Personal => WikiTemplate {
            id,
            name: "Personal Growth",
            description: "Track goals, habits, reflections, and journal entries for self-improvement",
            extra_dirs: &["wiki/goals", "wiki/habits", "wiki/reflections", "wiki/journal"],
            schema_md: format!(
                "# Wiki Schema — Personal Growth\n\n\
                 ## Page Types\n\n\
                 | Category | Directory | Purpose |\n\
                 |----------|-----------|---------|\n\
                 {BASE_SCHEMA_TYPES}\n\
                 | goal | wiki/goals/ | Specific outcomes you are working toward |\n\
                 | habit | wiki/habits/ | Recurring behaviours and their tracking |\n\
                 | reflection | wiki/reflections/ | Periodic reviews and lessons learned |\n\
                 | journal | wiki/journal/ | Freeform daily or session entries |\n\n\
                 ## Naming Conventions\n\n\
                 {BASE_NAMING}\n\n\
                 ## Frontmatter Rules\n\n\
                 {BASE_FRONTMATTER}\n\n\
                 ## Cross-referencing Rules\n\n\
                 {BASE_CROSSREF}\n\n\
                 ## Contradiction Handling\n\n\
                 {BASE_CONTRADICTION}\n"
            ),
            purpose_md: "# Project Purpose — Personal Growth\n\n\
                         ## Focus Areas\n\n\
                         1. \n\
                         2. \n\n\
                         ## Current Goals\n\n\
                         - [ ] \n"
                .to_string(),
        },
        WikiTemplateId::Business => WikiTemplate {
            id,
            name: "Business & Team Engineering",
            description: "Manage meetings, architecture decisions (ADRs), projects, and stakeholder context",
            extra_dirs: &["wiki/meetings", "wiki/decisions", "wiki/projects", "wiki/stakeholders"],
            schema_md: format!(
                "# Wiki Schema — Business & Engineering Team\n\n\
                 ## Page Types\n\n\
                 | Category | Directory | Purpose |\n\
                 |----------|-----------|---------|\n\
                 {BASE_SCHEMA_TYPES}\n\
                 | meeting | wiki/meetings/ | Meeting notes, agendas, and action items |\n\
                 | decision | wiki/decisions/ | Architectural or strategic decisions (ADR-style) |\n\
                 | project | wiki/projects/ | Project briefs, status, and retrospectives |\n\
                 | stakeholder | wiki/stakeholders/ | People, teams, and organisations involved |\n\n\
                 ## Naming Conventions\n\n\
                 {BASE_NAMING}\n\n\
                 ## Frontmatter Rules\n\n\
                 {BASE_FRONTMATTER}\n\n\
                 ## Cross-referencing Rules\n\n\
                 {BASE_CROSSREF}\n\n\
                 ## Contradiction Handling\n\n\
                 {BASE_CONTRADICTION}\n"
            ),
            purpose_md: "# Project Purpose — Business & Team\n\n\
                         ## Objectives\n\n\
                         1. \n\
                         2. \n\n\
                         ## Key Projects & Decisions\n\n\
                         - \n"
                .to_string(),
        },
        WikiTemplateId::General => WikiTemplate {
            id,
            name: "General Knowledge Base",
            description: "Minimal Karpathy LLM Wiki + QMD setup for any domain",
            extra_dirs: &[],
            schema_md: format!(
                "# Wiki Schema — General Knowledge Base\n\n\
                 ## Page Types\n\n\
                 | Category | Directory | Purpose |\n\
                 |----------|-----------|---------|\n\
                 {BASE_SCHEMA_TYPES}\n\n\
                 ## Naming Conventions\n\n\
                 {BASE_NAMING}\n\n\
                 ## Frontmatter Rules\n\n\
                 {BASE_FRONTMATTER}\n\n\
                 ## Cross-referencing Rules\n\n\
                 {BASE_CROSSREF}\n\n\
                 ## Contradiction Handling\n\n\
                 {BASE_CONTRADICTION}\n"
            ),
            purpose_md: "# Project Purpose\n\n\
                         ## Goal\n\n\
                         > What are you trying to understand, synthesize, or build?\n\n\
                         ## Key Questions\n\n\
                         1. \n\
                         2. \n"
                .to_string(),
        },
    }
}

/// Return all 5 built-in Wiki templates.
pub fn all_wiki_templates() -> Vec<WikiTemplate> {
    vec![
        get_wiki_template(WikiTemplateId::Research),
        get_wiki_template(WikiTemplateId::Reading),
        get_wiki_template(WikiTemplateId::Personal),
        get_wiki_template(WikiTemplateId::Business),
        get_wiki_template(WikiTemplateId::General),
    ]
}
