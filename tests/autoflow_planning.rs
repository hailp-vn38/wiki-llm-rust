//! Lập kế hoạch Auto-Flow cho vault nhiều nguồn: tra nguồn theo định danh, phát hiện ingest dở,
//! chia lô và cách ly nguồn lỗi.

use std::path::PathBuf;

use llm_wiki::{
    AutoflowPlanOptions, IngestSourceRequest, SourceIngestState, WikiEngine,
    WriteTopicSourceRequest,
};
use serde_json::json;

fn temp_vault(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("llm_wiki_autoflow_{name}_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save_video(engine: &WikiEngine, stem: &str, video_id: &str) {
    engine
        .write_topic_source(WriteTopicSourceRequest {
            topic_folder: "trading/smc".to_string(),
            file_stem: Some(stem.to_string()),
            title: format!("Video {stem}"),
            raw_body: format!("Transcript of {stem} with enough words."),
            body_heading: Some("Raw Transcript".to_string()),
            tags: vec![],
            extra_frontmatter: vec![("video_id".to_string(), json!(video_id))],
            header_bullets: vec![],
            preview_chars: None,
        })
        .unwrap();
}

fn ingest(engine: &WikiEngine, stem: &str, related_concepts: Vec<String>) {
    engine
        .ingest_source_to_wiki(IngestSourceRequest {
            source_rel_path: format!("trading/smc/{stem}.md"),
            target_wiki_path: None,
            raw_source_content: None,
            title: format!("Video {stem}"),
            one_line_summary: "Summary.".to_string(),
            wiki_markdown_body: "Body of the summary page.".to_string(),
            tags: vec![],
            related_entities: vec![],
            related_concepts,
        })
        .unwrap();
}

#[test]
fn finds_a_source_by_identity_regardless_of_its_file_name() {
    let engine = WikiEngine::open(temp_vault("identity")).unwrap();
    save_video(&engine, "renamed-after-restart", "vid-A");

    let found = engine
        .find_source_by_frontmatter("video_id", "vid-A")
        .unwrap()
        .expect("source saved under any stem must be found by video_id");
    assert_eq!(found.scoped_path, "trading/smc/renamed-after-restart.md");
    assert_eq!(found.ingest_state, SourceIngestState::NotIngested);

    assert!(engine
        .find_source_by_frontmatter("video_id", "vid-unknown")
        .unwrap()
        .is_none());
}

#[test]
fn a_summary_page_with_missing_concept_pages_is_not_a_finished_ingest() {
    let engine = WikiEngine::open(temp_vault("incomplete")).unwrap();
    save_video(&engine, "one", "vid-1");
    ingest(&engine, "one", vec!["order-flow".to_string()]);

    let found = engine
        .find_source_by_frontmatter("video_id", "vid-1")
        .unwrap()
        .unwrap();
    assert_eq!(found.ingest_state, SourceIngestState::Incomplete);

    let plan = engine
        .plan_next_autoflow(None, &AutoflowPlanOptions::default())
        .unwrap()
        .unwrap();
    assert!(plan.prompt.starts_with("[AUTO-FLOW: NEW SOURCES DETECTED]"));
    assert_eq!(plan.ingest_batch, vec!["sources/trading/smc/one.md"]);
    assert!(plan.prompt.contains("concepts/order-flow"));

    engine
        .write_wiki_file(
            "concepts/order-flow.md",
            "---\ntitle: Order Flow\ncategory: concept\n---\n\nOrder flow page.\n",
        )
        .unwrap();
    let found = engine
        .find_source_by_frontmatter("video_id", "vid-1")
        .unwrap()
        .unwrap();
    assert_eq!(found.ingest_state, SourceIngestState::Ingested);
}

#[test]
fn plans_a_bounded_batch_and_tells_the_root_not_to_run_the_curator() {
    let engine = WikiEngine::open(temp_vault("batch")).unwrap();
    for i in 0..5 {
        save_video(&engine, &format!("v{i}"), &format!("vid-{i}"));
    }

    let plan = engine
        .plan_next_autoflow(
            None,
            &AutoflowPlanOptions {
                max_sources: Some(2),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        plan.ingest_batch,
        vec!["sources/trading/smc/v0.md", "sources/trading/smc/v1.md"]
    );
    assert!(plan.prompt.contains("v1.md"));
    assert!(!plan.prompt.contains("v2.md"));
    assert!(plan.prompt.contains("Hệ thống còn 3 tài liệu"));
    assert!(!plan.prompt.contains("sau đó gọi `wiki_curator`"));
}

#[test]
fn quarantined_sources_do_not_block_the_queue_or_the_curator_stage() {
    let engine = WikiEngine::open(temp_vault("quarantine")).unwrap();
    save_video(&engine, "good", "vid-good");
    save_video(&engine, "poison", "vid-poison");
    let quarantined = vec!["sources/trading/smc/poison.md".to_string()];
    let options = AutoflowPlanOptions {
        exclude_sources: &quarantined,
        ..Default::default()
    };

    let first = engine.plan_next_autoflow(None, &options).unwrap().unwrap();
    assert_eq!(first.ingest_batch, vec!["sources/trading/smc/good.md"]);
    assert!(!first.prompt.contains("poison.md"));

    ingest(&engine, "good", vec![]);
    let second = engine.plan_next_autoflow(None, &options).unwrap().unwrap();
    assert!(
        second.prompt.starts_with("[AUTO-FLOW: WIKI LINT]"),
        "quarantine must not hold back synthesis: {}",
        second.prompt
    );
    assert!(second.ingest_batch.is_empty());
}

#[test]
fn updating_an_ingested_source_requests_a_reingest() {
    let engine = WikiEngine::open(temp_vault("reingest")).unwrap();
    save_video(&engine, "one", "vid-1");
    ingest(&engine, "one", vec![]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    save_video(&engine, "one", "vid-1");

    let plan = engine
        .plan_next_autoflow(None, &AutoflowPlanOptions::default())
        .unwrap()
        .unwrap();
    assert_eq!(plan.ingest_batch, vec!["sources/trading/smc/one.md"]);
    assert!(plan.prompt.contains("nguồn đã được cập nhật"));
}
