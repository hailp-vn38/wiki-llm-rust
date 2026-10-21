//! Complete Ingest Prompt Suite & Batch Block Parser (`---FILE:...---` / `---REVIEW:...---`).
//!
//! Ported from `nashsu/llm_wiki` (`src/lib/ingest.ts`, `src/lib/page-merge.ts`,
//! `src/lib/research-query-grounding.ts`) and adapted for `llm-wiki-rs` (`WikiEngine`).
//!
//! Supports **both** execution paradigms:
//! 1. **Tool-Calling Agent Mode**: Stage 1 structured analysis + Stage 2 tool-calling instructions
//!    for agents (e.g. `orchestrator-agent`) using `wiki_ingest_source`, `wiki_write_file`, `wiki_patch_file`.
//! 2. **Batch Block Mode (`---FILE: ... ---` & `---REVIEW: ... ---`)**: Stage 1 analysis + Stage 2
//!    deterministic block generation, plus CommonMark-fence-aware parsers (`parse_file_blocks`,
//!    `parse_review_blocks`), Page Merge prompt (`build_page_merge_system_prompt`), and Truncated
//!    File Repair prompt (`build_truncated_file_repair_prompt`).

use crate::prompts::language::{build_language_directive, OutputLanguage};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Entity significance & anti-fragmentation rules for Stage 1 analysis.
pub const ENTITY_ANALYSIS_RULES: &[&str] = &[
    "Apply a standalone-page significance threshold: page-worthy subjects are central to the source, recur meaningfully, carry substantive claims, or are reusable across the wiki.",
    "Do not mark passing mentions, ordinary story props, unnamed or generic-role people, chapter/part labels, or internal workflow artifacts as page-worthy unless the source makes them independently important knowledge subjects.",
    "For every candidate, output `Page-worthy: yes` or `Page-worthy: no` plus one short justification. Keep non-page-worthy candidates only as context for the source summary.",
    "Canonical naming priority: reuse the exact title and slug of an existing wiki page first; otherwise preserve the source spelling. Widely used localized names may be recorded as aliases, but never invent or phonetically guess a name.",
];

/// Entity page creation rules for Stage 2 generation.
pub const ENTITY_PAGE_RULES: &[&str] = &[
    "Create a standalone entity page only for a candidate explicitly marked `Page-worthy: yes`. Keep other mentions as plain prose in the source summary, or as wikilinks only when the target already exists in the current wiki index.",
    "Canonical naming priority: reuse the exact title and slug of an existing wiki page first; otherwise preserve the source spelling. A widely used localized name may be an alias, but never invent or phonetically guess a name.",
    "The opening paragraph of every entity page must answer what the subject is, not merely describe its role in this source.",
    "Prefer an identity supported by the source or existing wiki. If one short identity sentence must use stable, high-confidence public knowledge to identify a well-known subject, clearly label it as independently verifiable background in the mandatory output language so it is not attributed to the imported source. Never add uncertain details; when neither the source nor existing wiki establishes an unambiguous identity, do not create the standalone page.",
];

/// Long-source entity accumulation rules across chunks.
pub const LONG_SOURCE_ENTITY_RULES: &[&str] = &[
    "In each Chunk Analysis, record named-subject mentions and identity evidence without making a final standalone-page decision from one chunk alone.",
    "In Updated Global Digest > Entities, accumulate recurrence across chunks and output for each candidate: canonical name, identity, supporting chunks, significance evidence, and `Page-worthy: yes` or `Page-worthy: no`.",
    "Apply a standalone-page significance threshold: page-worthy subjects are central to the source, recur meaningfully, carry substantive claims, or are reusable across the wiki.",
    "Do not mark passing mentions, ordinary story props, unnamed or generic-role people, chapter/part labels, or internal workflow artifacts as page-worthy unless the source makes them independently important knowledge subjects.",
    "Canonical naming priority: reuse the exact title of an existing wiki page first; otherwise preserve the source spelling.",
];

/// Grounding rules for generating web/research search queries inside REVIEW blocks.
pub const PROJECT_LOCAL_TERM_QUERY_RULES: &[&str] = &[
    "Treat ambiguous names, acronyms, character names, module names, and ordinary-looking labels as project-local vocabulary unless the supplied context clearly identifies a public meaning.",
    "Never emit a bare ambiguous term as a web query. Preserve its exact spelling and add domain, project, entity, or mechanism keywords supported by the supplied context.",
    "Do not substitute a popular public meaning for a project-local meaning.",
    "If an internal term has no likely public documentation, search for the underlying real-world concepts or comparisons needed to fill the gap.",
];

/// Context parameters for building Ingest prompts.
#[derive(Debug, Clone, Default)]
pub struct IngestPromptContext<'a> {
    /// Content of `SCHEMA.md`.
    pub schema: &'a str,
    /// Optional content of `purpose.md`.
    pub purpose: &'a str,
    /// Current content of `wiki/index.md`.
    pub index: &'a str,
    /// Relative path of the raw source file inside `sources/` (e.g. `"docs/flow/lib.md"`).
    pub source_rel_path: &'a str,
    /// Target wiki summary path (e.g. `"wiki/sources/docs/flow/lib.md"`).
    /// In `llm-wiki-rs`, this strictly preserves the source filename.
    pub target_wiki_summary_path: &'a str,
    /// Sample or full text of the source document (used for language auto-detection).
    pub source_content_sample: &'a str,
    /// Optional folder context hint (e.g. `"papers/energy"` or `"youtube/mql5-ea"`).
    pub folder_context: Option<&'a str>,
    /// Output language preference.
    pub output_language: OutputLanguage,
}

/// **Stage 1 Prompt (Single-Pass Analysis)**:
/// AI reads the raw source document and produces a structured research analysis before writing wiki pages.
pub fn build_ingest_analysis_prompt(ctx: &IngestPromptContext<'_>) -> String {
    let lang_directive = build_language_directive(&ctx.output_language, ctx.source_content_sample);
    let mut lines: Vec<String> = vec![
        "You are an expert research analyst. Read the source document and produce a structured analysis.".to_string(),
        "Do not output chain-of-thought, hidden reasoning, or a thinking transcript. Reason internally and write only the concise final analysis.".to_string(),
        String::new(),
        lang_directive,
        String::new(),
        "Your analysis should cover:".to_string(),
        String::new(),
        "## Topic Classification, Folder Routing & Hierarchical Tags".to_string(),
        "- Primary `domain` and `topic` (in `kebab-case`, e.g., `domain: \"algo-trading\"`, `topic: \"grid-dca\"`) derived from the source's core subject.".to_string(),
        "- Recommended topic folder routing: reuse an existing topic folder from `wiki/index.md` / `SCHEMA.md` first; if the wiki organizes pages by topic subfolders, specify `entities/<topic>/<slug>.md`, `concepts/<topic>/<slug>.md`, or `sources/<domain>/<topic>/<filename>.md` (max 2 folder levels).".to_string(),
        "- 3-Facet Hierarchical Tags (`kebab-case`) for Obsidian & QMD filtering:".to_string(),
        "  1. Page category tag (`\"wiki/source\"`, `\"wiki/entity\"`, `\"wiki/concept\"`)".to_string(),
        "  2. Hierarchical topic tags (`\"topic/<domain>/<subtopic>\"`, e.g. `\"topic/trading/grid-dca\"`)".to_string(),
        "  3. Specific technology/method keywords (e.g. `\"mql5\"`, `\"atr-indicator\"`, `\"martingale\"`)".to_string(),
        String::new(),
        "## Key Entities".to_string(),
        "List candidate named people, organizations, products, platforms, datasets, and tools. For each:".to_string(),
        "- Name, canonical kebab-case slug (`entities/<slug>.md`), and type".to_string(),
        "- Role and significance in the source".to_string(),
        "- Whether it likely already exists in the wiki (check the index)".to_string(),
    ];
    for rule in ENTITY_ANALYSIS_RULES {
        lines.push(format!("- {rule}"));
    }

    lines.extend([
        String::new(),
        "## Key Concepts".to_string(),
        "List theories, methods, algorithms, techniques, phenomena, and architectural patterns. For each:".to_string(),
        "- Name, canonical kebab-case slug (`concepts/<slug>.md`), and brief definition".to_string(),
        "- Why it matters in this source".to_string(),
        "- Whether it likely already exists in the wiki".to_string(),
        String::new(),
        "## Main Arguments & Findings".to_string(),
        "- What are the core claims, technical rules, parameters, or empirical results?".to_string(),
        "- What evidence or metrics support them?".to_string(),
        "- How strong is the evidence?".to_string(),
        "- Which named subject is each claim about? Do not transfer claims, limits, or evaluations from one entity/model/product/method to another just because they share keywords.".to_string(),
        "- Preserve structured source data verbatim in the analysis when present: include SQL DDL / CREATE TABLE statements, schema definitions, code snippets, API signatures, configuration parameters, and tables in fenced code blocks or Markdown tables. Do not reduce exact field names, types, constraints, keys, formulas, or metrics to prose.".to_string(),
        String::new(),
        "## Connections to Existing Wiki".to_string(),
        "- What existing pages in `wiki/index.md` does this source relate to?".to_string(),
        "- Does it strengthen, challenge, or extend existing knowledge?".to_string(),
        String::new(),
        "## Contradictions & Tensions".to_string(),
        "- Does anything in this source conflict with existing wiki content?".to_string(),
        "- Are there internal tensions, edge cases, or caveats (`> [!WARNING] Contradiction / Caveat`)?".to_string(),
        String::new(),
        "## Recommendations".to_string(),
        "- What wiki pages should be created or updated?".to_string(),
        "- If the project schema (below) defines page types beyond entity/concept, and the source genuinely contains matching content, recommend pages of those types — name the type explicitly. Only when the source actually supports it; never invent records that aren't in the source.".to_string(),
        "- What should be emphasized vs. de-emphasized?".to_string(),
        "- Any open questions worth flagging for the user?".to_string(),
        String::new(),
        "Be thorough but concise. Focus on what's genuinely important.".to_string(),
        "If a folder context is provided, use it as a hint for categorization — the folder structure often reflects organizational intent.".to_string(),
    ]);

    if let Some(folder) = ctx.folder_context.filter(|f| !f.trim().is_empty()) {
        lines.push(String::new());
        lines.push(format!("## Folder Context\n{folder}"));
    }
    if !ctx.schema.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "## Project Schema (page types available — map source content to schema-defined types when it fits)\n{}",
            ctx.schema.trim()
        ));
    }
    if !ctx.purpose.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Wiki Purpose (for context)\n{}", ctx.purpose.trim()));
    }
    if !ctx.index.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "## Current Wiki Index (for checking existing content)\n{}",
            ctx.index.trim()
        ));
    }

    lines.join("\n")
}

/// **Stage 1b Prompt (Long-Source Chunk Analysis System Prompt)**:
/// Used when a source document is split into semantic chunks with overlap and an evolving Global Digest.
pub fn build_chunk_analysis_system_prompt(ctx: &IngestPromptContext<'_>) -> String {
    let lang_directive = build_language_directive(&ctx.output_language, ctx.source_content_sample);
    let mut lines: Vec<String> = vec![
        "You are analyzing a long source document for a personal wiki.".to_string(),
        "Do not output chain-of-thought, hidden reasoning, or a thinking transcript.".to_string(),
        "Analyze only the current MAIN CHUNK. Use overlap and digest for context only.".to_string(),
        "Keep stable names consistent with the existing wiki and prior digest.".to_string(),
        String::new(),
        lang_directive,
        String::new(),
        "Output exactly two markdown sections:".to_string(),
        String::new(),
        "## Chunk Analysis".to_string(),
        "- Concise summary of the main chunk".to_string(),
        "- New or updated entities".to_string(),
        "- New or updated concepts".to_string(),
        "- Any schema-defined page types beyond entity/concept that the main chunk genuinely supports".to_string(),
        "- Claims, findings, evidence, contradictions".to_string(),
        "- Exact structured data from this chunk, when present: preserve SQL DDL / CREATE TABLE statements, schema definitions, code blocks, API signatures, configuration, and tables verbatim in fenced code blocks or Markdown tables".to_string(),
        "- Open questions or research gaps".to_string(),
        String::new(),
        "## Updated Global Digest".to_string(),
        "A compact document-level digest that incorporates this chunk and preserves prior cross-chunk context.".to_string(),
        "Keep this digest structured under: Summary, Entities, Concepts, Schema-Typed Candidates, Claims, Evidence, Contradictions, Open Questions, Cross-Chunk Relations.".to_string(),
        "Entity handling rules:".to_string(),
    ];
    for rule in LONG_SOURCE_ENTITY_RULES {
        lines.push(format!("- {rule}"));
    }
    lines.push("Use schema-defined types only when the source actually supports them; never invent records that are not present in the source.".to_string());

    if !ctx.purpose.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Wiki Purpose\n{}", ctx.purpose.trim()));
    }
    if !ctx.schema.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Wiki Schema\n{}", ctx.schema.trim()));
    }
    if !ctx.index.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Current Wiki Index\n{}", ctx.index.trim()));
    }

    lines.join("\n")
}

/// Build the user message for analyzing one chunk of a long source document.
pub fn build_chunk_analysis_user_prompt(
    source_rel_path: &str,
    folder_context: Option<&str>,
    chunk_index: usize,
    chunk_total: usize,
    heading_path: Option<&str>,
    global_digest: &str,
    overlap_before: &str,
    main_chunk: &str,
) -> String {
    let mut lines = vec![format!("Source file: {source_rel_path}")];
    if let Some(folder) = folder_context.filter(|f| !f.trim().is_empty()) {
        lines.push(format!("Folder context: {folder}"));
    }
    lines.push(format!("Chunk: {chunk_index}/{chunk_total}"));
    if let Some(hp) = heading_path.filter(|h| !h.trim().is_empty()) {
        lines.push(format!("Heading path: {hp}"));
    }
    lines.push(String::new());
    lines.push("## Current Global Digest".to_string());
    lines.push(if global_digest.trim().is_empty() {
        "(No prior digest yet.)".to_string()
    } else {
        global_digest.trim().to_string()
    });
    if !overlap_before.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Previous Overlap Context\n{}", overlap_before.trim()));
    }
    lines.push(String::new());
    lines.push("## MAIN CHUNK TO ANALYZE".to_string());
    lines.push(main_chunk.to_string());
    lines.push(String::new());
    lines.push("Return only the two requested sections. Do not repeat overlap-only facts unless the main chunk supports them.".to_string());
    lines.join("\n")
}

/// **Stage 2A Prompt (Batch Block Generation Mode: `---FILE: ... ---` & `---REVIEW: ... ---`)**:
/// Takes the Stage 1 analysis and generates deterministic `---FILE: wiki/...---` and `---REVIEW: ...---` blocks.
///
/// Mirrors `buildGenerationPrompt()` from `nashsu/llm_wiki` while enforcing `llm-wiki-rs`'s
/// filename preservation and Obsidian + QMD frontmatter conventions.
pub fn build_ingest_batch_generation_prompt(ctx: &IngestPromptContext<'_>) -> String {
    let lang_directive = build_language_directive(&ctx.output_language, ctx.source_content_sample);
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let source_rel = ctx.source_rel_path.trim().trim_start_matches("sources/");
    let vault_source_rel = format!("sources/{source_rel}");
    let source_file_name = std::path::Path::new(source_rel)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(source_rel);
    let summary_path = if ctx.target_wiki_summary_path.trim().is_empty() {
        format!("wiki/sources/{source_rel}")
    } else if ctx.target_wiki_summary_path.starts_with("wiki/") {
        ctx.target_wiki_summary_path.trim().to_string()
    } else {
        format!("wiki/{}", ctx.target_wiki_summary_path.trim())
    };

    let mut lines: Vec<String> = vec![
        "You are a disciplined Karpathy LLM Wiki maintainer. Based on the analysis provided, generate wiki files.".to_string(),
        "Do not output chain-of-thought, hidden reasoning, or explanatory preamble. Reason internally and output only the requested FILE/REVIEW blocks.".to_string(),
        String::new(),
        lang_directive.clone(),
        String::new(),
        "## IMPORTANT: Source File & Filename Preservation".to_string(),
        format!("The original raw source file in the vault is: **`{vault_source_rel}`** (filename: `{source_file_name}`)."),
        format!("1. The source summary page MUST be written at **`{summary_path}`** (strictly preserving the exact source filename `{source_file_name}`, never an auto-generated ID)."),
        format!("2. All wiki pages generated from this source MUST include `\"[[{vault_source_rel}|{source_file_name}]]\"` in their frontmatter `sources` list."),
        format!("3. Today's date is **{today}**."),
    ];

    if !ctx.schema.trim().is_empty() {
        lines.extend([
            String::new(),
            "## Project Schema and Routing (AUTHORITATIVE)".to_string(),
            ctx.schema.trim().to_string(),
            String::new(),
            "Use this schema as the primary routing rule for page categories and directories.".to_string(),
            "Use `wiki/entities/` and `wiki/concepts/` unless the schema provides a more specific destination.".to_string(),
        ]);
    }

    lines.extend([
        String::new(),
        "## What to Generate".to_string(),
        String::new(),
        format!("1. **Source Summary Page** at **`{summary_path}`** (MUST use this exact path)"),
        "2. **Entity Pages** (`wiki/entities/<slug>.md`) for key named things marked `Page-worthy: yes` in the analysis.".to_string(),
        "3. **Concept Pages** (`wiki/concepts/<slug>.md`) for key ideas, methods, algorithms, and techniques.".to_string(),
    ]);
    for rule in ENTITY_PAGE_RULES {
        lines.push(format!("- {rule}"));
    }
    lines.extend([
        "- Do not generate `wiki/index.md` or `wiki/log.md`. `WikiEngine` maintains aggregate navigation and logs automatically.".to_string(),
        String::new(),
        "## Frontmatter Rules (CRITICAL — Obsidian + QMD Parser)".to_string(),
        String::new(),
        "Every generated page MUST begin with a YAML frontmatter block on line 1 (`---`):".to_string(),
        "1. The VERY FIRST line of the file MUST be `---` (do NOT wrap the file in a ```yaml code fence).".to_string(),
        "2. Required fields:".to_string(),
        "   - `title`: Human-readable title (quote with `\"...\"`)".to_string(),
        "   - `category`: `\"source_summary\"` | `\"entity\"` | `\"concept\"` | `\"synthesis\"`".to_string(),
        "   - `domain` & `topic`: Primary domain and topic in `kebab-case` derived from the source (e.g. `domain: \"algo-trading\"`, `topic: \"grid-dca\"`)".to_string(),
        "   - `summary`: Concise one-line description for `wiki/index.md`".to_string(),
        format!("   - `sources`: YAML list containing `\"[[{vault_source_rel}|{source_file_name}]]\"`"),
        "   - `tags`: 3-Facet YAML list of strings: (1) `\"wiki/<category>\"`, (2) hierarchical topic tags `\"topic/<domain>/<subtopic>\"`, and (3) specific subject/tech keywords".to_string(),
        "   - `qmd.metadata`: Nested block mirroring `category`, `domain`, `topic`, and `tags` for QMD hybrid search filtering".to_string(),
        String::new(),
        "Concrete example of a valid page:".to_string(),
        "    ---".to_string(),
        "    title: \"Example Entity\"".to_string(),
        "    category: \"entity\"".to_string(),
        "    domain: \"ai-engineering\"".to_string(),
        "    topic: \"hybrid-search\"".to_string(),
        "    summary: \"One-line summary of the entity for wiki/index.md\"".to_string(),
        "    aliases: []".to_string(),
        "    sources:".to_string(),
        format!("      - \"[[{vault_source_rel}|{source_file_name}]]\""),
        "    tags:".to_string(),
        "      - \"wiki/entity\"".to_string(),
        "      - \"topic/ai-engineering/hybrid-search\"".to_string(),
        "      - \"rust\"".to_string(),
        "    qmd:".to_string(),
        "      metadata:".to_string(),
        "        category: \"entity\"".to_string(),
        "        domain: \"ai-engineering\"".to_string(),
        "        topic: \"hybrid-search\"".to_string(),
        "        tags:".to_string(),
        "          - \"wiki/entity\"".to_string(),
        "          - \"topic/ai-engineering/hybrid-search\"".to_string(),
        "          - \"rust\"".to_string(),
        "    ---".to_string(),
        String::new(),
        "    # Example Entity".to_string(),
        String::new(),
        "    Body content using Obsidian [[entities/related-slug|Related Entity]] and [[concepts/concept-slug|Concept]] links.".to_string(),
        String::new(),
        "## Body & Cross-Referencing Rules".to_string(),
        "- Use Obsidian `[[WikiLink]]` syntax (`[[entities/slug|Label]]`, `[[concepts/slug|Label]]`, `[[wiki/sources/...|Label]]`) in the body. NEVER use standard Markdown links `[label](path.md)` for internal wiki references.".to_string(),
        "- Preserve subject boundaries: when a source discusses multiple entities/models/methods, keep claims, metrics, and limitations attached to the exact subject they describe.".to_string(),
        "- Preserve structured source data verbatim: copy SQL DDL, code blocks, formulas, API signatures, configuration parameters, and tables into fenced code blocks or Markdown tables.".to_string(),
        "- Flag contradictions or critical caveats with an Obsidian callout block (`> [!WARNING] Contradiction / Caveat`).".to_string(),
        String::new(),
        "## Review Block Types (Optional, after all FILE blocks)".to_string(),
        "Emit REVIEW blocks only for genuine knowledge gaps needing human review or Deep Research:".to_string(),
        "- `contradiction`: conflicts with existing wiki content".to_string(),
        "- `duplicate`: an entity/concept might already exist under a different name in the index".to_string(),
        "- `missing-page`: an important concept is referenced but has no dedicated page".to_string(),
        "- `suggestion`: high-value follow-up questions or sources worth researching".to_string(),
        "Allowed OPTIONS line: `OPTIONS: Create Page | Skip`".to_string(),
        "For `suggestion` and `missing-page` reviews, include `SEARCH: query 1 | query 2 | query 3` with 2-3 context-grounded search queries:".to_string(),
    ]);
    for rule in PROJECT_LOCAL_TERM_QUERY_RULES {
        lines.push(format!("- {rule}"));
    }

    if !ctx.purpose.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Wiki Purpose\n{}", ctx.purpose.trim()));
    }
    if !ctx.index.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "## Current Wiki Index (reuse existing slugs!)\n{}",
            ctx.index.trim()
        ));
    }

    lines.extend([
        String::new(),
        "## Output Format (MUST FOLLOW EXACTLY — parser is strict)".to_string(),
        String::new(),
        "Your ENTIRE response consists of FILE blocks followed by optional REVIEW blocks. Nothing else.".to_string(),
        String::new(),
        "FILE block template:".to_string(),
        "```".to_string(),
        "---FILE: wiki/path/to/page.md---".to_string(),
        "(complete file content starting with --- YAML frontmatter)".to_string(),
        "---END FILE---".to_string(),
        "```".to_string(),
        String::new(),
        "REVIEW block template (optional, after all FILE blocks):".to_string(),
        "```".to_string(),
        "---REVIEW: type | Title---".to_string(),
        "Description of what needs attention.".to_string(),
        "OPTIONS: Create Page | Skip".to_string(),
        "PAGES: wiki/page1.md, wiki/page2.md".to_string(),
        "SEARCH: query 1 | query 2 | query 3".to_string(),
        "---END REVIEW---".to_string(),
        "```".to_string(),
        String::new(),
        "## Output Requirements (STRICT)".to_string(),
        "1. The FIRST character of your response MUST be `-` (the opening of `---FILE:`).".to_string(),
        "2. DO NOT output any preamble (e.g. \"Here are the files:\") or trailing commentary.".to_string(),
        "3. Between blocks, use only blank lines — no prose.".to_string(),
        String::new(),
        "---".to_string(),
        String::new(),
        lang_directive,
    ]);

    lines.join("\n")
}

/// **Stage 2B Prompt (Tool-Calling Agent Mode)**:
/// Instructs an LLM Agent with `WikiEngine` tools (`wiki_ingest_source`, `wiki_write_file`,
/// `wiki_patch_file`, `wiki_get`) how to execute the Ingest workflow directly via tool calls.
pub fn build_ingest_tool_agent_prompt(ctx: &IngestPromptContext<'_>) -> String {
    let lang_directive = build_language_directive(&ctx.output_language, ctx.source_content_sample);
    let source_rel = ctx.source_rel_path.trim().trim_start_matches("sources/");
    let vault_source_rel = format!("sources/{source_rel}");
    let expected_wiki_path = format!("wiki/sources/{source_rel}");

    let mut lines = vec![
        "You are an autonomous Karpathy LLM Wiki Maintainer Agent operating via `WikiEngine` tools.".to_string(),
        "Your task is to ingest the raw source document into a rich, interlinked set of Obsidian-compatible Wiki pages.".to_string(),
        String::new(),
        lang_directive.clone(),
        String::new(),
        "## Target Source".to_string(),
        format!("- Raw Source Path: `{vault_source_rel}` (`source_rel_path`: `\"{source_rel}\"`)."),
        format!("- Expected Wiki Summary Page: `{expected_wiki_path}` (strictly preserves the exact source filename)."),
        String::new(),
        "## Mandatory 4-Step Tool Execution Workflow".to_string(),
        String::new(),
        "### Step 1: Inspect Existing Wiki State & Taxonomy (`wiki_get`)".to_string(),
        "- Call `wiki_get(file=\"wiki/index.md\")` (if not already provided below) to inspect all existing `wiki/sources/`, `wiki/entities/`, and `wiki/concepts/` slugs.".to_string(),
        "- **Anti-Fragmentation Rule**: Always reuse existing canonical slugs in `wiki/entities/<slug>.md` and `wiki/concepts/<slug>.md`. Never create synonymous duplicate files.".to_string(),
        String::new(),
        "### Step 2: Internal Research Analysis (Before Writing)".to_string(),
        "- Identify Page-Worthy Entities (`Page-worthy: yes` only — concrete proper nouns: people, platforms, tools, models, datasets, indicators) vs. Concepts (algorithms, strategies, mechanisms, architectures).".to_string(),
        "- Extract all technical rules, parameters, code/SQL/formulas verbatim, empirical findings/metrics, connections to existing wiki pages, and contradictions/caveats.".to_string(),
        String::new(),
        "### Step 3: Create the Source Summary Page (`wiki_ingest_source`)".to_string(),
        format!("- Call `wiki_ingest_source` with `source_rel_path: \"{source_rel}\"`, `title`, `one_line_summary`, `tags`, `related_entities` (canonical slugs), `related_concepts` (canonical slugs), and a comprehensive `wiki_markdown_body`."),
        "- Inside `wiki_markdown_body`:".to_string(),
        "  1. Preserve structured technical details, parameters, code blocks, and comparison/metric tables verbatim.".to_string(),
        "  2. Embed inline Obsidian `[[entities/<slug>|Label]]`, `[[concepts/<slug>|Label]]`, and `[[wiki/sources/...|Label]]` links (never use standard Markdown links `[text](path.md)`).".to_string(),
        "  3. Include a `> [!WARNING] Contradiction / Caveat` callout block if the source contains internal tensions, limitations, or conflicts with existing wiki claims.".to_string(),
        "  4. Include a valid multi-line `flowchart TD` or `flowchart LR` Mermaid diagram with double-quoted node labels (`A[\"... \"] --> B[\"... \"]`) when illustrating an algorithm, architecture, or process.".to_string(),
        String::new(),
        "### Step 4: Compound Entity & Concept Pages + Cross-Link Existing Sources (`wiki_write_file` / `wiki_patch_file`)".to_string(),
        "- For each canonical slug in `related_entities` (`entities/<slug>.md`) and `related_concepts` (`concepts/<slug>.md`):".to_string(),
        "  - If the page **does not exist**, call `wiki_write_file(scope=\"wiki\", rel_path=\"entities/<slug>.md\", content=...)` with full YAML frontmatter (`title`, `category: \"entity\"` or `\"concept\"`, `summary`, `sources`, `tags`, `qmd.metadata`), a clear domain definition in the opening paragraph, technical details, and a backlink to `[[wiki/sources/...]]` and `[[sources/...]]`.".to_string(),
        "  - If the page **already exists**, call `wiki_get` to read it, then call `wiki_patch_file` (or `wiki_write_file` merging all existing facts + new insights) so knowledge compounds across sources without losing prior claims.".to_string(),
        "- If related source pages already exist in `wiki/sources/`, call `wiki_patch_file` on them to add a forward/backward `[[wiki/sources/...]]` cross-reference.".to_string(),
    ];

    if !ctx.schema.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Project Schema (`SCHEMA.md`)\n{}", ctx.schema.trim()));
    }
    if !ctx.index.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("## Current Wiki Index (`wiki/index.md`)\n{}", ctx.index.trim()));
    }
    lines.push(String::new());
    lines.push(lang_directive);

    lines.join("\n")
}

/// **Page Merge System Prompt**:
/// Ported from `buildPageMergeSystemPrompt()` in `nashsu/llm_wiki`.
/// Used when an entity/concept page already exists on disk and a new source contributes additional material.
pub fn build_page_merge_system_prompt(
    output_language: &OutputLanguage,
    sample_text: &str,
) -> String {
    let lang_directive = build_language_directive(output_language, sample_text);
    [
        "You are merging source-backed material for the same wiki page into one coherent document.".to_string(),
        "The inputs are internal containers, not revisions or competing versions.".to_string(),
        "Either input may mention additional subjects for comparison or context.".to_string(),
        String::new(),
        lang_directive,
        String::new(),
        "Output ONE merged version that:".to_string(),
        "- Preserves every factual claim, parameter, code snippet, and table from both inputs (do not drop content)".to_string(),
        "- Eliminates redundancy when both inputs state the exact same fact".to_string(),
        "- Preserves subject/source boundaries: if either input mentions other entities/models/products/methods for comparison, keep those comparisons attribution-exact and do not fold them into claims about the main page subject".to_string(),
        "- When claims conflict or apply to different subjects, keep them separated and add a `> [!WARNING] Contradiction / Caveat` callout attributing each claim to its `[[sources/...]]` file".to_string(),
        "- Reorganizes sections so the structure is logical for the merged topic, not just a raw concatenation of the two inputs".to_string(),
        "- Keeps all Obsidian `[[WikiLink]]` references intact".to_string(),
        "- Never describe the merge inputs as 'existing/new/original/old versions' in headings, prose, notes, or table columns".to_string(),
        "- Never invent URLs, citations, source names, or placeholder references".to_string(),
        String::new(),
        "Output requirements:".to_string(),
        "- The FIRST character of your response MUST be `-` (the opening of `---` YAML frontmatter)".to_string(),
        "- Output the COMPLETE file: YAML frontmatter + body".to_string(),
        "- No preamble (no \"Here is the merged version:\"), no analysis prose".to_string(),
    ]
    .join("\n")
}

/// Build the user message for merging an existing wiki page with newly generated content.
pub fn build_page_merge_user_prompt(
    existing_content: &str,
    incoming_content: &str,
    new_source_rel_path: &str,
) -> String {
    [
        "## Previously collected material (from existing wiki page sources)".to_string(),
        String::new(),
        existing_content.trim().to_string(),
        String::new(),
        "---".to_string(),
        String::new(),
        format!("## Additional material (from source: `{new_source_rel_path}`)"),
        String::new(),
        incoming_content.trim().to_string(),
        String::new(),
        "---".to_string(),
        String::new(),
        "Now output the merged file. Start with `---` on the first line.".to_string(),
    ]
    .join("\n")
}

/// **Truncated File Repair Prompt**:
/// Ported from `buildTruncatedFileRepairPrompt()` in `nashsu/llm_wiki`.
/// Used when an LLM stream hits `max_tokens` and leaves one or more `---FILE: ...---` blocks unclosed.
pub fn build_truncated_file_repair_prompt(
    truncated_paths: &[String],
    source_rel_path: &str,
    schema: &str,
    analysis: &str,
    source_context: &str,
    output_language: &OutputLanguage,
) -> String {
    let lang_directive = build_language_directive(output_language, source_context);
    let requested_list = truncated_paths
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n");

    [
        "You are repairing truncated wiki FILE blocks from an earlier generation.".to_string(),
        "Return exactly one complete FILE block for each requested path and no other files.".to_string(),
        "Every block must start with `---FILE: <path>---` and end with `---END FILE---`. Do not output a preamble, REVIEW blocks, or trailing commentary.".to_string(),
        "Preserve the requested paths exactly and include the source file in each page's frontmatter `sources` field.".to_string(),
        String::new(),
        lang_directive,
        String::new(),
        "## Requested paths".to_string(),
        requested_list,
        String::new(),
        format!("## Source identity\n{source_rel_path}"),
        if schema.trim().is_empty() {
            String::new()
        } else {
            format!("\n## Project schema\n{}", schema.trim())
        },
        format!("\n## Stage 1 analysis\n{}", analysis.trim()),
        format!("\n## Source context\n{}", source_context.trim()),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

// =============================================================================
// BATCH BLOCK PARSERS (`---FILE: ... ---` & `---REVIEW: ... ---`)
// =============================================================================

/// One parsed `---FILE: path--- ... ---END FILE---` block from Stage 2 output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedFileBlock {
    /// Normalized path (e.g. `"wiki/entities/rust.md"`).
    pub path: String,
    /// Full Markdown content inside the block.
    pub content: String,
}

/// Result of parsing `---FILE: ... ---` blocks, including non-fatal warnings and truncated paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseFileBlocksResult {
    pub blocks: Vec<ParsedFileBlock>,
    pub warnings: Vec<String>,
    pub truncated_paths: Vec<String>,
}

/// Review item category emitted in `---REVIEW: type | title---` blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewBlockType {
    Contradiction,
    Duplicate,
    MissingPage,
    Suggestion,
    Confirm,
}

impl ReviewBlockType {
    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "contradiction" => Self::Contradiction,
            "duplicate" => Self::Duplicate,
            "missing-page" | "missing_page" => Self::MissingPage,
            "suggestion" => Self::Suggestion,
            _ => Self::Confirm,
        }
    }
}

/// One parsed `---REVIEW: type | title--- ... ---END REVIEW---` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedReviewBlock {
    pub review_type: ReviewBlockType,
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub affected_pages: Vec<String>,
    pub search_queries: Vec<String>,
}

/// Check if an LLM-emitted ingest file path is safe and scoped under `wiki/`.
///
/// Mirrors `isSafeIngestPath()` from `nashsu/llm_wiki`:
/// - Must start with `wiki/` (or be normalized to `wiki/`)
/// - Rejects control bytes, absolute paths, `..` traversal, and Windows reserved filenames.
pub fn is_safe_ingest_wiki_path(raw_path: &str) -> bool {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c <= '\u{1f}') {
        return false;
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return false;
    }
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        return false;
    }
    let normalized = trimmed.replace('\\', "/");
    if !normalized.starts_with("wiki/") {
        return false;
    }
    for seg in normalized.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
        if seg.ends_with(' ') || seg.ends_with('.') {
            return false;
        }
        if seg
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return false;
        }
        let stem = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
            return false;
        }
    }
    true
}

fn parse_opener_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.len() <= 6 || !trimmed.starts_with("---") || !trimmed.ends_with("---") {
        return None;
    }
    let inner = trimmed
        .strip_prefix("---")?
        .strip_suffix("---")?
        .trim();
    let rest = inner
        .strip_prefix("FILE:")
        .or_else(|| inner.strip_prefix("file:"))
        .or_else(|| inner.strip_prefix("File:"))?;
    Some(rest.trim().to_string())
}

fn is_closer_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() <= 6 || !trimmed.starts_with("---") || !trimmed.ends_with("---") {
        return false;
    }
    let inner = trimmed[3..trimmed.len() - 3].trim();
    let words: Vec<&str> = inner.split_whitespace().collect();
    words.len() == 2
        && words[0].eq_ignore_ascii_case("END")
        && words[1].eq_ignore_ascii_case("FILE")
}

fn parse_fence_marker(line: &str) -> Option<(char, usize)> {
    let leading_spaces = line.chars().take_while(|&c| c == ' ').count();
    if leading_spaces > 3 {
        return None;
    }
    let rest = &line[leading_spaces..];
    let first = rest.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run_len = rest.chars().take_while(|&c| c == first).count();
    if run_len >= 3 {
        Some((first, run_len))
    } else {
        None
    }
}

/// Parse `---FILE: wiki/path.md--- ... ---END FILE---` blocks from LLM output.
///
/// Ported from `parseFileBlocks()` in `nashsu/llm_wiki/src/lib/ingest.ts`:
/// - Normalizes CRLF (`\r\n`) to `\n`
/// - Tracks CommonMark code fences (```` ``` ```` and `~~~`) so literal `---END FILE---`
///   inside code examples never prematurely closes the outer file block
/// - Detects stream truncation (unclosed blocks) and surfaces them in `truncated_paths`
/// - Enforces `is_safe_ingest_wiki_path` security boundaries
pub fn parse_file_blocks(text: &str) -> ParseFileBlocksResult {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();

    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut truncated_paths = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let Some(raw_path) = parse_opener_line(lines[i]) else {
            i += 1;
            continue;
        };
        let path = raw_path.replace('\\', "/");
        i += 1;

        let mut content_lines = Vec::new();
        let mut fence_marker: Option<(char, usize)> = None;
        let mut closed = false;

        while i < lines.len() {
            let line = lines[i];
            if let Some((ch, len)) = parse_fence_marker(line) {
                match fence_marker {
                    None => fence_marker = Some((ch, len)),
                    Some((open_ch, open_len)) if ch == open_ch && len >= open_len => {
                        let after_fence = line.trim_start_matches(' ');
                        let after_ticks = &after_fence[len..];
                        if after_ticks.trim().is_empty() {
                            fence_marker = None;
                        }
                    }
                    _ => {}
                }
                content_lines.push(line);
                i += 1;
                continue;
            }

            if fence_marker.is_none() && is_closer_line(line) {
                closed = true;
                i += 1;
                break;
            }

            content_lines.push(line);
            i += 1;
        }

        if !closed {
            let label = if path.is_empty() { "(unnamed)" } else { &path };
            warnings.push(format!(
                "FILE block \"{label}\" was not closed before end of stream — likely truncation."
            ));
            if is_safe_ingest_wiki_path(&path) {
                truncated_paths.push(path);
            }
            continue;
        }

        if path.is_empty() {
            warnings.push("FILE block with empty path skipped.".to_string());
            continue;
        }

        if !is_safe_ingest_wiki_path(&path) {
            warnings.push(format!(
                "FILE block with unsafe path \"{path}\" rejected (must be under wiki/, no .., no absolute paths)."
            ));
            continue;
        }

        blocks.push(ParsedFileBlock {
            path,
            content: content_lines.join("\n").trim().to_string(),
        });
    }

    ParseFileBlocksResult {
        blocks,
        warnings,
        truncated_paths,
    }
}

/// Parse `---REVIEW: type | Title--- ... ---END REVIEW---` blocks from LLM output.
pub fn parse_review_blocks(text: &str) -> Vec<ParsedReviewBlock> {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut reviews = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let opener = if trimmed.starts_with("---") && trimmed.ends_with("---") && trimmed.len() > 6
        {
            let inner = trimmed[3..trimmed.len() - 3].trim();
            inner
                .strip_prefix("REVIEW:")
                .or_else(|| inner.strip_prefix("review:"))
                .and_then(|rest| rest.split_once('|'))
                .map(|(r_type, title)| (r_type.trim().to_string(), title.trim().to_string()))
        } else {
            None
        };

        let Some((raw_type, title)) = opener else {
            i += 1;
            continue;
        };
        i += 1;

        let mut body_lines = Vec::new();
        let mut closed = false;
        while i < lines.len() {
            let l_trim = lines[i].trim();
            if l_trim.starts_with("---") && l_trim.ends_with("---") && l_trim.len() > 6 {
                let inner = l_trim[3..l_trim.len() - 3].trim();
                let words: Vec<&str> = inner.split_whitespace().collect();
                if words.len() == 2
                    && words[0].eq_ignore_ascii_case("END")
                    && words[1].eq_ignore_ascii_case("REVIEW")
                {
                    closed = true;
                    i += 1;
                    break;
                }
            }
            body_lines.push(lines[i]);
            i += 1;
        }

        if !closed {
            continue;
        }

        let mut options = Vec::new();
        let mut affected_pages = Vec::new();
        let mut search_queries = Vec::new();
        let mut desc_lines = Vec::new();

        for b_line in body_lines {
            let t = b_line.trim();
            if let Some(opts) = t.strip_prefix("OPTIONS:") {
                options = opts
                    .split('|')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if let Some(pages) = t.strip_prefix("PAGES:") {
                affected_pages = pages
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if let Some(queries) = t.strip_prefix("SEARCH:") {
                search_queries = queries
                    .split('|')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else {
                desc_lines.push(b_line);
            }
        }

        if options.is_empty() {
            options = vec!["Create Page".to_string(), "Skip".to_string()];
        }

        reviews.push(ParsedReviewBlock {
            review_type: ReviewBlockType::from_raw(&raw_type),
            title,
            description: desc_lines.join("\n").trim().to_string(),
            options,
            affected_pages,
            search_queries,
        });
    }

    reviews
}
