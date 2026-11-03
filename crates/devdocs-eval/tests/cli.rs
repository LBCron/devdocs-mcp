//! CLI-level tests through the real harness binary (the repo's `assert_cmd`
//! convention, as in crates/devdocs/tests/cli.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use assert_cmd::Command;

use common::devdocs_bin;

/// The harness command for one run of the fixture suite.
fn eval_run(json: bool, suite: &str, dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devdocs-eval"));
    if json {
        cmd.arg("--json");
    }
    cmd.arg("run")
        .arg(suite)
        .arg("--db")
        .arg(dir.join("fixture.db"))
        .arg("--golden-dir")
        .arg(dir.join("golden"))
        .arg("--runs-dir")
        .arg(dir.join("runs"))
        .arg("--devdocs-bin")
        .arg(devdocs_bin());
    cmd
}

#[test]
fn the_cli_prints_the_gate_table_and_exits_two_on_gate_failure() {
    let dir = tempfile::tempdir().unwrap();
    common::fixture(dir.path());
    let suite = dir.path().join("fixture.jsonl");
    let output = eval_run(true, suite.to_str().unwrap(), dir.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["all_gates_pass"], serde_json::json!(false));
    assert_eq!(parsed["gates"].as_array().unwrap().len(), 6);
    assert_eq!(
        parsed["metrics"]["failure_rate"]["total"],
        serde_json::json!(3)
    );
}

#[test]
fn the_cli_exits_one_on_an_invalid_suite() {
    let dir = tempfile::tempdir().unwrap();
    common::fixture(dir.path());
    let suite = dir.path().join("bad.jsonl");
    std::fs::write(
        &suite,
        r#"{"id":"x","library":"next.js","version":"^1.0","task_type":"exact_api","question":"q","expect_refs":["docs://next.js/v1.0/proxy"],"grader":"ref_match","notes":"n"}"#,
    )
    .unwrap();
    let output = eval_run(true, suite.to_str().unwrap(), dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not an exact pin"), "{stderr}");
    assert!(output.stdout.is_empty(), "errors stay off stdout");
}

#[test]
fn the_plain_run_prints_the_gate_table_and_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    common::fixture(dir.path());
    let suite = dir.path().join("fixture.jsonl");
    let output = eval_run(false, suite.to_str().unwrap(), dir.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("| Gate | Measured | Threshold | Phase | Status |"),
        "{stdout}"
    );
    assert!(stdout.contains("hit@5"), "{stdout}");
    // The fixture keeps the absent-ref gate failing, so the run verdict
    // names the gate failure and never claims a phase transition.
    assert!(stdout.contains("gate failure"), "{stdout}");
    assert!(!stdout.contains("all gates PASS"), "{stdout}");
}
