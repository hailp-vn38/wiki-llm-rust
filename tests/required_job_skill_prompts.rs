use llm_wiki::{WIKI_CURATOR_PROMPT, WIKI_INGESTOR_PROMPT};

#[test]
fn wiki_ingestor_uses_preloaded_required_skills_and_keeps_first_turn_reads_parallel() {
    for required_skill in ["wiki-ingest", "wiki-taxonomy", "wiki-page-format"] {
        assert!(
            !WIKI_INGESTOR_PROMPT.contains(&format!("skill(id=\"{required_skill}\")")),
            "wiki_ingestor must not spend a turn loading preloaded Required Job Skill {required_skill}"
        );
    }
    assert!(WIKI_INGESTOR_PROMPT.contains("already loaded below this prompt"));
    assert!(WIKI_INGESTOR_PROMPT.contains("wiki_get(file=\"wiki/index.md\")"));
    assert!(WIKI_INGESTOR_PROMPT.contains("wiki_get(file=\"sources/<source_rel_path>\")"));
    assert!(WIKI_INGESTOR_PROMPT.contains("together in one turn"));
}

#[test]
fn wiki_curator_uses_preloaded_required_skills() {
    for required_skill in ["wiki-lint", "wiki-query"] {
        assert!(
            !WIKI_CURATOR_PROMPT.contains(&format!("skill` with `id=\"{required_skill}\"`")),
            "wiki_curator must not spend a turn loading preloaded Required Job Skill {required_skill}"
        );
    }
    assert!(WIKI_CURATOR_PROMPT.contains("already loaded below this prompt"));
    assert!(WIKI_CURATOR_PROMPT.contains("wiki_get(file=\"wiki/index.md\")"));
}
