//! End-to-end tests for `devdocs skill install` and the bare `devdocs
//! skill` route: the pinned content, the force policy, the `--path` flag,
//! and the companion workflow over the live search/open surfaces
//! (docs/implementation/08 §The skill companion).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, indexed_env, output};
use devdocs::commands::SKILL_MD;
use devdocs_model::ResultEnvelope;
use serde_json::Value;

/// `skill install` writes the pinned content and reports the path.
#[test]
fn skill_install_writes_pinned_content() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["skill", "install"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("SKILL.md"), "stdout: {stdout}");
    let content = std::fs::read_to_string(env.root.join("SKILL.md")).expect("file written");
    insta::assert_snapshot!("skill_install_writes_pinned_content", content);
}

/// A bare `devdocs skill` writes byte-identical content and prints the
/// same line as `devdocs skill install`.
#[test]
fn bare_skill_routes_to_install() {
    let env = Env::new();
    let (bare_code, bare_stdout, bare_stderr) = output(env.cmd(), &["skill"]);
    assert_eq!(bare_code, 0, "stderr: {bare_stderr}");
    let bare = std::fs::read(env.root.join("SKILL.md")).expect("bare wrote the file");
    let (code, stdout, stderr) = output(env.cmd(), &["skill", "install", "--force"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let pinned = std::fs::read(env.root.join("SKILL.md")).expect("install wrote the file");
    assert_eq!(bare, pinned);
    assert_eq!(bare_stdout, stdout);
}

/// A second install refuses: exit 1 with USAGE naming --force, and the
/// file keeps its first content.
#[test]
fn skill_install_refuses_overwrite_without_force() {
    let env = Env::new();
    let (code, _stdout, stderr) = output(env.cmd(), &["skill", "install"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let before = std::fs::read(env.root.join("SKILL.md")).expect("file");
    let (code, stdout, stderr) = output(env.cmd(), &["skill", "install"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout must stay clean: {stdout:?}");
    assert!(stderr.contains("already exists"), "stderr: {stderr}");
    assert!(stderr.contains("--force"), "stderr: {stderr}");
    let after = std::fs::read(env.root.join("SKILL.md")).expect("file");
    assert_eq!(before, after);
}

/// `--force` overwrites a stale file with the pinned content.
#[test]
fn skill_install_force_overwrites() {
    let env = Env::new();
    std::fs::write(env.root.join("SKILL.md"), "stale").expect("stale written");
    let (code, _stdout, stderr) = output(env.cmd(), &["skill", "install", "--force"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let content = std::fs::read_to_string(env.root.join("SKILL.md")).expect("file");
    assert_eq!(content, SKILL_MD);
}

/// `--path` writes the file under created parents.
#[test]
fn skill_install_path_flag_writes_custom_file() {
    let env = Env::new();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &["skill", "install", "--path", "nested/AGENT-SKILL.md"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("AGENT-SKILL.md"), "stdout: {stdout}");
    assert!(env.root.join("nested/AGENT-SKILL.md").is_file());
}

/// A `--path` naming an existing directory refuses with USAGE.
#[test]
fn skill_install_path_directory_refused() {
    let env = Env::new();
    std::fs::create_dir_all(env.root.join("adir")).expect("dir");
    let (code, stdout, stderr) = output(env.cmd(), &["skill", "install", "--path", "adir"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout must stay clean: {stdout:?}");
    assert!(stderr.contains("is a directory"), "stderr: {stderr}");
}

/// The companion workflow the SKILL.md documents drives the live
/// surfaces: a hit search feeds an open, and a miss exits 2
/// (docs/implementation/08 §The skill companion, step 4).
#[test]
fn skill_companion_workflow_drives_live_search_open() {
    let (env, config) = indexed_env();
    let cfg = config.to_str().expect("config path");

    let (code, stdout, stderr) =
        output(env.cmd(), &["search", "client", "--json", "--config", cfg]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let envelope: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let doc_ref = envelope
        .results
        .first()
        .expect("one result")
        .doc_ref
        .clone();

    let (code, stdout, stderr) = output(env.cmd(), &["open", &doc_ref, "--json", "--config", cfg]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let opened: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let body = opened.results.first().expect("opened body");
    assert!(!body.snippet.is_empty(), "the open body must not be empty");

    // The miss exit the SKILL.md tells the agent to expect.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "zzz_no_such_concept_zzz",
            "--json",
            "--config",
            cfg,
        ],
    );
    assert_eq!(code, 2, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "NO_RESULT");
}
