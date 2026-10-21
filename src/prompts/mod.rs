//! Standard Prompt Builders, Domain Templates, and Block Parsers for `llm-wiki-rs`.
//!
//! Ported from `nashsu/llm_wiki` and integrated with Andrej Karpathy's **LLM Wiki** pattern
//! and **QMD (Query Markup Documents)** hybrid search + **Obsidian Graph View**.

pub mod ingest;
pub mod language;
pub mod query_lint;
pub mod subagents;
pub mod templates;

pub use ingest::{
    build_chunk_analysis_system_prompt, build_chunk_analysis_user_prompt,
    build_ingest_analysis_prompt, build_ingest_batch_generation_prompt,
    build_ingest_tool_agent_prompt, build_page_merge_system_prompt, build_page_merge_user_prompt,
    build_truncated_file_repair_prompt, is_safe_ingest_wiki_path, parse_file_blocks,
    parse_review_blocks, IngestPromptContext, ParseFileBlocksResult, ParsedFileBlock,
    ParsedReviewBlock, ReviewBlockType, ENTITY_ANALYSIS_RULES, ENTITY_PAGE_RULES,
    LONG_SOURCE_ENTITY_RULES, PROJECT_LOCAL_TERM_QUERY_RULES,
};
pub use language::{
    build_language_directive, build_language_reminder, detect_language_prompt_name, OutputLanguage,
};
pub use query_lint::{
    apply_wikilink_substitutions, build_enrich_wikilinks_system_prompt,
    build_evidence_quality_gate_system_prompt, build_query_agent_system_prompt,
    build_research_synthesis_prompt, build_semantic_lint_prompt, parse_semantic_lint_blocks,
    ParsedSemanticLintBlock, SemanticLintIssueType, WikiRetrievalMode, WikilinkSubstitution,
};
pub use subagents::{
    ROOT_DISPATCHER_PROMPT, SOURCE_COLLECTOR_PROMPT, WIKI_CURATOR_PROMPT, WIKI_INGESTOR_PROMPT,
};
pub use templates::{all_wiki_templates, get_wiki_template, WikiTemplate, WikiTemplateId};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection_and_directive() {
        assert_eq!(
            detect_language_prompt_name("Chiến lược DCA và quản trị rủi ro trên MetaTrader 5"),
            "Vietnamese"
        );
        assert_eq!(
            detect_language_prompt_name("Scaling laws for large language models"),
            "English"
        );

        let directive = build_language_directive(&OutputLanguage::Vietnamese, "");
        assert!(directive.contains("MANDATORY OUTPUT LANGUAGE: Vietnamese"));
        assert!(directive.contains("proper nouns and technical identifiers"));
    }

    #[test]
    fn test_parse_file_blocks_handles_code_fences_and_security() {
        let raw = "---FILE: wiki/entities/rust.md---\n\
                   ---\n\
                   title: \"Rust\"\n\
                   category: \"entity\"\n\
                   ---\n\
                   # Rust\n\
                   ```markdown\n\
                   ---END FILE---\n\
                   ```\n\
                   Real body after fence.\n\
                   ---END FILE---\n\
                   \n\
                   ---FILE: ../../../etc/passwd---\n\
                   malicious\n\
                   ---END FILE---\n\
                   \n\
                   ---FILE: wiki/concepts/truncated.md---\n\
                   partial content without end marker";

        let res = parse_file_blocks(raw);
        assert_eq!(res.blocks.len(), 1);
        assert_eq!(res.blocks[0].path, "wiki/entities/rust.md");
        assert!(res.blocks[0].content.contains("Real body after fence."));
        assert_eq!(res.truncated_paths, vec!["wiki/concepts/truncated.md"]);
        assert_eq!(res.warnings.len(), 2);
    }

    #[test]
    fn test_parse_review_and_lint_blocks() {
        let review_raw = "---REVIEW: missing-page | ATR Volatility Filter---\n\
                          Referenced across 3 videos but lacks a dedicated concept page.\n\
                          OPTIONS: Create Page | Skip\n\
                          PAGES: wiki/sources/v1.md, wiki/sources/v2.md\n\
                          SEARCH: MQL5 iATR volatility filter | ATR grid spacing EA\n\
                          ---END REVIEW---";
        let reviews = parse_review_blocks(review_raw);
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].review_type, ReviewBlockType::MissingPage);
        assert_eq!(reviews[0].title, "ATR Volatility Filter");
        assert_eq!(reviews[0].affected_pages.len(), 2);
        assert_eq!(reviews[0].search_queries.len(), 2);

        let lint_raw = "---LINT: contradiction | warning | Conflicting winrate claims---\n\
                        Video 1 claims 99% winrate while Video 3 shows account crash.\n\
                        PAGES: wiki/sources/v1.md, wiki/sources/v3.md\n\
                        ---END LINT---";
        let lints = parse_semantic_lint_blocks(lint_raw);
        assert_eq!(lints.len(), 1);
        assert_eq!(lints[0].issue_type, SemanticLintIssueType::Contradiction);
        assert_eq!(lints[0].severity, "warning");
        assert_eq!(lints[0].affected_pages.len(), 2);
    }

    #[test]
    fn test_apply_wikilink_substitutions_preserves_frontmatter_and_existing_links() {
        let page = "---\ntitle: \"MetaTrader 5\"\n---\n\n# MetaTrader 5\n\nMetaTrader 5 uses [[entities/mql5|MQL5]] and ATR Indicator for grid trading.\n";
        let subs = vec![
            WikilinkSubstitution {
                term: "ATR Indicator".to_string(),
                target: "entities/atr-indicator".to_string(),
            },
            WikilinkSubstitution {
                term: "MQL5".to_string(),
                target: "entities/mql5".to_string(),
            },
        ];
        let enriched = apply_wikilink_substitutions(page, &subs);
        assert!(enriched.contains("[[entities/atr-indicator|ATR Indicator]]"));
        // Existing [[entities/mql5|MQL5]] must not be double-wrapped
        assert!(!enriched.contains("[[entities/mql5|[["));
    }

    #[test]
    fn test_engine_apply_ingest_file_blocks_and_prompt_builders() {
        let root = std::env::temp_dir().join(format!(
            "llm_wiki_prompt_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let engine = crate::WikiEngine::open(&root).unwrap();

        let tpl = get_wiki_template(WikiTemplateId::Research);
        let ctx = IngestPromptContext {
            schema: &tpl.schema_md,
            purpose: &tpl.purpose_md,
            index: "# Wiki Index",
            source_rel_path: "algo/dca_bot.md",
            target_wiki_summary_path: "wiki/sources/algo/dca_bot.md",
            source_content_sample: "Martingale DCA bot using ATR Indicator on MetaTrader 5.",
            folder_context: Some("algo"),
            output_language: OutputLanguage::Vietnamese,
        };

        let stage1 = build_ingest_analysis_prompt(&ctx);
        assert!(stage1.contains("MANDATORY OUTPUT LANGUAGE: Vietnamese"));
        assert!(stage1.contains("## Key Entities"));

        let stage2_batch = build_ingest_batch_generation_prompt(&ctx);
        assert!(stage2_batch.contains("wiki/sources/algo/dca_bot.md"));
        assert!(stage2_batch.contains("---FILE:"));

        let stage2_tool = build_ingest_tool_agent_prompt(&ctx);
        assert!(stage2_tool.contains("wiki_ingest_source"));

        let llm_batch_output = "\
---FILE: wiki/sources/algo/dca_bot.md---
---
title: \"DCA Bot Strategy\"
category: \"source\"
source_path: \"sources/algo/dca_bot.md\"
tags: [\"dca\", \"mt5\"]
---

# DCA Bot Strategy

Uses [[entities/atr-indicator|ATR Indicator]] on [[entities/metatrader-5|MetaTrader 5]].
---END FILE---

---FILE: wiki/entities/atr-indicator.md---
---
title: \"ATR Indicator\"
category: \"entity\"
tags: [\"indicator\", \"volatility\"]
---

# ATR Indicator

Average True Range indicator used in [[sources/algo/dca_bot|DCA Bot Strategy]].
---END FILE---
";

        let (parsed, written) = engine
            .apply_ingest_file_blocks("algo/dca_bot.md", llm_batch_output)
            .unwrap();
        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(written.len(), 2);
        assert!(parsed.truncated_paths.is_empty());

        let saved_source = std::fs::read_to_string(root.join("wiki/sources/algo/dca_bot.md")).unwrap();
        assert!(saved_source.contains("[[entities/atr-indicator|ATR Indicator]]"));

        let saved_entity = std::fs::read_to_string(root.join("wiki/entities/atr-indicator.md")).unwrap();
        assert!(saved_entity.contains("Average True Range"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
