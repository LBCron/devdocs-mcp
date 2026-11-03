//! End-to-end harness tests through the library API: a seeded fixture
//! database, a tiny suite, and the real `devdocs` binary for the
//! subprocess pass.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use devdocs_eval::gates::GateStatus;
use devdocs_eval::run::{RunOptions, run};

/// Runs the fixture suite once and returns the outcome with its temp dir
/// and the fixture database path.
fn run_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    devdocs_eval::run::RunOutcome,
) {
    let dir = tempfile::tempdir().unwrap();
    let (db, suite, runs) = common::fixture(dir.path());
    let outcome = run(&RunOptions {
        suite: suite.display().to_string(),
        db: Some(db.clone()),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    })
    .unwrap();
    (dir, db, outcome)
}

#[test]
fn a_run_scores_and_writes_parseable_artifacts() {
    use sha2::Digest as _;
    let (_dir, db, outcome) = run_fixture();

    for artifact in ["results.json", "report.md", "manifest.json"] {
        assert!(outcome.run_dir.join(artifact).is_file(), "{artifact}");
    }

    // The answered question passes; the unanswerable abstains; the absent
    // ref fails (partial counts as wrong).
    let results: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.run_dir.join("results.json")).unwrap(),
    )
    .unwrap();
    let rows = results["questions"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["score"]["passed"], serde_json::json!(true));
    assert_eq!(rows[0]["score"]["hit_at_5"], serde_json::json!(true));
    assert_eq!(rows[1]["score"]["passed"], serde_json::json!(true));
    assert_eq!(rows[2]["score"]["passed"], serde_json::json!(false));
    assert_ne!(
        rows[0]["in_process"]["hits"].as_array().unwrap().len(),
        0,
        "the answered question has hits"
    );

    // The manifest reconstructs the denominator.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.run_dir.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["questions"], serde_json::json!(3));
    assert_eq!(manifest["suite"], serde_json::json!("fixture"));
    assert_eq!(manifest["db_hash"].as_str().unwrap().len(), 64);
    // db_hash is the SHA-256 of the database file itself, hashed once.
    let expected = {
        let bytes = std::fs::read(&db).unwrap();
        let digest = sha2::Sha256::digest(&bytes);
        devdocs_eval::suite::hex_digest(&digest)
    };
    assert_eq!(manifest["db_hash"], serde_json::json!(expected));
}

#[test]
fn a_run_renders_the_report_and_judges_the_gates() {
    let (_dir, _db, outcome) = run_fixture();

    // The report carries the gate table, the breakdown, and the stub note.
    let md = std::fs::read_to_string(outcome.run_dir.join("report.md")).unwrap();
    assert!(md.contains("## Gate table"));
    assert!(md.contains("exact_api"));
    assert!(
        md.contains("not recorded"),
        "no sidecar: minutes unrecorded"
    );
    assert!(md.contains("Phase 0 stub"));

    // Deterministic gates: hit@5 is 1/2 (the absent ref misses), symbol
    // recall is 1/1, and version correctness is 2/3 (the abstention has no
    // top hit). The absent ref keeps the table honest: not all PASS.
    let status = |name: &str| {
        outcome
            .gates
            .iter()
            .find(|g| g.gate == name)
            .unwrap()
            .status
    };
    assert_eq!(status("hit@5"), GateStatus::Fail);
    assert_eq!(status("Symbol recall"), GateStatus::Pass);
    assert_eq!(status("Version correctness"), GateStatus::Fail);
    assert_eq!(status("Cost per 1,000 queries"), GateStatus::Pass);
    assert!(!outcome.all_gates_pass);
}

#[test]
fn an_invalid_suite_is_input_error_exit_one() {
    let dir = tempfile::tempdir().unwrap();
    let suite_path = dir.path().join("bad.jsonl");
    std::fs::write(
        &suite_path,
        r#"{"id":"x","library":"next.js","version":"^1.0","task_type":"exact_api","question":"q","expect_refs":["docs://next.js/v1.0/proxy"],"grader":"ref_match","notes":"n"}"#,
    )
    .unwrap();
    let (db, _suite, runs) = common::fixture(dir.path());
    let outcome = run(&RunOptions {
        suite: suite_path.display().to_string(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    });
    let err = outcome.unwrap_err();
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("not an exact pin"), "{err}");
}

#[test]
fn a_manual_grader_suite_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let suite_path = dir.path().join("manual.jsonl");
    std::fs::write(
        &suite_path,
        r#"{"id":"m","library":"next.js","version":"16.2.9","task_type":"agent_task","question":"q","expect_refs":["docs://next.js/v16.2.9/proxy"],"grader":"manual","notes":"n"}"#,
    )
    .unwrap();
    let (db, _suite, runs) = common::fixture(dir.path());
    let outcome = run(&RunOptions {
        suite: suite_path.display().to_string(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    });
    let err = outcome.unwrap_err();
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("manual grader"), "{err}");
}

/// A stub `devdocs` that exits 1 immediately, standing in for a failing
/// CLI build.
#[cfg(unix)]
fn failing_bin(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("devdocs-stub.sh");
    std::fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn a_failing_subprocess_call_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (db, suite, runs) = common::fixture(dir.path());
    let outcome = run(&RunOptions {
        suite: suite.display().to_string(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(failing_bin(dir.path())),
    });
    let err = outcome.unwrap_err();
    assert_eq!(err.exit_code(), 3, "{err}");
    assert!(err.to_string().contains("exited 1"), "{err}");
}

#[test]
fn a_second_run_of_one_suite_gets_its_own_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (db, suite, _runs) = common::fixture(dir.path());
    let options = |db: std::path::PathBuf| RunOptions {
        suite: suite.display().to_string(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(dir.path().join("runs")),
        devdocs_bin: Some(common::devdocs_bin()),
    };
    let first = run(&options(db.clone())).unwrap();
    let second = run(&options(db)).unwrap();
    assert_ne!(first.run_dir, second.run_dir, "runs never share a dir");
    assert!(
        second.run_dir.to_string_lossy().ends_with("-2"),
        "the dedup suffix advances: {}",
        second.run_dir.display()
    );
}

#[test]
fn a_suite_resolves_by_name_from_the_golden_dir() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _suite, runs) = common::fixture(dir.path());
    std::fs::create_dir_all(dir.path().join("golden")).unwrap();
    std::fs::copy(
        dir.path().join("fixture.jsonl"),
        dir.path().join("golden").join("fixture.jsonl"),
    )
    .unwrap();
    let outcome = run(&RunOptions {
        suite: "fixture".to_owned(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    })
    .unwrap();
    assert_eq!(outcome.metrics.failure_rate.total, 3);
}

#[test]
fn a_direct_suite_path_without_the_jsonl_suffix_keeps_its_stem() {
    let dir = tempfile::tempdir().unwrap();
    let (db, suite, runs) = common::fixture(dir.path());
    let renamed = dir.path().join("fixture.suite");
    std::fs::rename(&suite, &renamed).unwrap();
    let outcome = run(&RunOptions {
        suite: renamed.display().to_string(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    })
    .unwrap();
    // The manifest names the suite after the file stem.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.run_dir.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["suite"], serde_json::json!("fixture"));
}

#[test]
fn a_baseline_record_reaches_the_manifest_and_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _suite, runs) = common::fixture(dir.path());
    std::fs::create_dir_all(dir.path().join("golden")).unwrap();
    std::fs::copy(
        dir.path().join("fixture.jsonl"),
        dir.path().join("golden").join("fixture.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("baseline")).unwrap();
    std::fs::write(
        dir.path().join("baseline").join("phase0.json"),
        r#"{"recorded":"2026-11-01","workflow":"hosted docs MCP","schema_rent":{"band_tokens_per_tool":[550,1400],"source":"s","notes":"n"},"per_query_cost":{"hosted_api_usd_per_1000_queries":1.66,"raw_mcp_usd_per_1000_queries":3.66,"assumptions":"a","source":"s"}}"#,
    )
    .unwrap();
    let outcome = run(&RunOptions {
        suite: "fixture".to_owned(),
        db: Some(db),
        golden_dir: Some(dir.path().join("golden")),
        runs_dir: Some(runs),
        devdocs_bin: Some(common::devdocs_bin()),
    })
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.run_dir.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["baseline_hash"].as_str().unwrap().len(),
        64,
        "the manifest carries the baseline digest"
    );
    let md = std::fs::read_to_string(outcome.run_dir.join("report.md")).unwrap();
    assert!(md.contains("## Phase 0 baseline"), "{md}");
    assert!(md.contains("hosted docs MCP"), "{md}");
    assert!(md.contains("Re-run the probe quarterly"), "{md}");
}
