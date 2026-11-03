//! The shipped pilot suite passes harness validation and meets the Phase 0
//! sizing rules (docs/implementation/14 §Pilot).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use devdocs_eval::suite::{Suite, TaskType};

fn pilot_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/golden/pilot.jsonl")
}

#[test]
fn the_pilot_suite_is_valid_and_meets_the_sizing_rules() {
    let suite = Suite::load(&pilot_path(), "pilot").unwrap();

    assert_eq!(suite.questions.len(), 20);
    let libraries: BTreeSet<_> = suite.questions.iter().map(|q| q.library.as_str()).collect();
    assert_eq!(libraries, BTreeSet::from(["next.js", "serde", "zod"]));

    let unanswerable = suite
        .questions
        .iter()
        .filter(|q| q.task_type == TaskType::Unanswerable)
        .count();
    assert!(unanswerable >= 2, "at least two unanswerable questions");

    let task_types: BTreeSet<_> = suite
        .questions
        .iter()
        .map(|q| q.task_type.as_str())
        .collect();
    assert_eq!(task_types.len(), 8, "all eight categories are covered");

    let meta = suite.meta.as_ref().expect("the sidecar records the effort");
    assert!(meta.curation_minutes > 0);
    assert_eq!(suite.hash.len(), 64);
}

#[test]
fn the_phase0_baseline_record_parses() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/baseline/phase0.json");
    let (baseline, hash) = devdocs_eval::report::load_baseline(&path)
        .unwrap()
        .expect("the baseline record exists");
    assert_eq!(baseline.schema_rent.band_tokens_per_tool, [550, 1400]);
    assert!(baseline.per_query_cost.hosted_api_usd_per_1000_queries > 0.0);
    assert_eq!(hash.len(), 64);
}
