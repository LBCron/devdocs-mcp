//! End-to-end CLI tests for the git branch of `devdocs add` and the
//! `commit_sha` surface of `add --json` / `list --json`, offline against
//! the testkit fixture repositories (docs/implementation/08 §CLI contract,
//! docs/implementation/07 §Git adapter).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use common::{Env, output};
use devdocs_testkit::git_repo::docs_repo;
use serde_json::Value;

/// The `file://` URL of a local repository path — the exact string the
/// gate matches against the registry entry and the adapter plans with.
fn repo_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Writes a config with the `acme` git source approved at the index stage.
fn write_git_config(env: &Env, url: &str) -> std::path::PathBuf {
    let config = env.root.join("devdocs.toml");
    std::fs::write(
        &config,
        format!(
            "[sources.acme]\n\
             kind = \"git\"\n\
             url = \"{url}\"\n\
             license = \"MIT\"\n\
             status = \"approved_index\"\n"
        ),
    )
    .expect("config written");
    config
}

/// A tag-pin `add --kind git` against a local fixture repo publishes, and
/// both the `add --json` report and `list --json` carry the tag's commit
/// SHA.
#[test]
fn add_git_tag_pin_publishes_and_list_shows_commit_sha() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            &url,
            "--version",
            "v1.0.0",
            "--kind",
            "git",
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let added: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(added["library"], "acme");
    assert_eq!(added["version"], "v1.0.0");
    assert!(
        added["chunks"].as_u64().expect("chunks") > 0,
        "the fixture docs ingest: {added}"
    );
    assert_eq!(
        added["commit_sha"], repo.tag_sha,
        "the report carries the tag's commit: {added}"
    );

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "list",
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let list: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    let results = list["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], "git");
    assert_eq!(results[0]["version"], "v1.0.0");
    assert_eq!(
        results[0]["commit_sha"], repo.tag_sha,
        "list shows the capture commit: {results:?}"
    );
}

/// A git add without `--version` has no pin to resolve: usage error, no
/// database.
#[test]
fn add_git_without_a_version_is_a_usage_error() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            &url,
            "--kind",
            "git",
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    assert!(stderr.contains("--version"), "stderr: {stderr}");
    assert!(!db.exists(), "a rejected add must not create the database");
}

/// `--fixture` replays HTTP recordings; a git add refuses it instead of
/// silently running against the network.
#[test]
fn add_git_refuses_a_fixture_replay() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");
    let fixture = env.root.join("fixture");
    std::fs::create_dir_all(&fixture).expect("fixture dir");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            &url,
            "--version",
            "v1.0.0",
            "--kind",
            "git",
            "--fixture",
            fixture.to_str().expect("fixture path"),
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    assert!(stderr.contains("--fixture"), "stderr: {stderr}");
    assert!(!db.exists(), "a rejected add must not create the database");
}

/// With no `git` executable on PATH the add fails as `UPSTREAM_ERROR`
/// (exit 3) with the documented `GIT_BINARY_MISSING` detail
/// (docs/implementation/08 §Error codes).
#[test]
fn add_git_without_the_git_binary_fails_upstream() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    // An empty directory as PATH: the binary lookup fails, everything else
    // (the absolute-path spawn of devdocs itself) keeps working.
    let empty_bin = env.root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).expect("empty bin dir");
    let mut env_with_path = env;
    env_with_path.set("PATH", empty_bin.to_str().expect("bin path"));

    let (code, _stdout, stderr) = output(
        env_with_path.cmd(),
        &[
            "add",
            &url,
            "--version",
            "v1.0.0",
            "--kind",
            "git",
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("GIT_BINARY_MISSING"), "stderr: {stderr}");
}
