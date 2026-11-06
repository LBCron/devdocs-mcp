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
/// `Url::from_file_path` normalizes the separators, so the TOML string
/// stays parseable on Windows too (a raw `\U` reads as a unicode escape).
fn repo_url(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("local path is a file url")
        .to_string()
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

/// Asserts the `add --json` report of a successful tag-pin add: the acme
/// library ingests chunks and carries the tag's commit SHA.
fn assert_added_report(added: &Value, tag_sha: &str) {
    assert_eq!(added["library"], "acme");
    assert_eq!(added["version"], "v1.0.0");
    assert!(
        added["chunks"].as_u64().expect("chunks") > 0,
        "the fixture docs ingest: {added}"
    );
    assert_eq!(
        added["commit_sha"], tag_sha,
        "the report carries the tag's commit: {added}"
    );
}

/// Asserts the `list --json` results of a successful tag-pin add: exactly
/// one git source at the pinned version carrying the tag's commit SHA.
fn assert_list_report(list: &Value, tag_sha: &str) {
    let results = list["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], "git");
    assert_eq!(results[0]["version"], "v1.0.0");
    assert_eq!(
        results[0]["commit_sha"], tag_sha,
        "list shows the capture commit: {results:?}"
    );
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
    assert_added_report(&added, &repo.tag_sha);

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
    assert_list_report(&list, &repo.tag_sha);
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

/// An option-shaped pin (leading `-`) reaches the adapter and fails as
/// `UPSTREAM_ERROR` with the documented `GIT_INVALID_PIN` detail — the
/// argv-vector rule rejects it before any git runs
/// (docs/implementation/09 §Git subprocess safety).
#[test]
fn add_git_with_an_option_shaped_pin_fails_upstream() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            &url,
            "--version=-upload-pack=evil",
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
    assert!(stderr.contains("GIT_INVALID_PIN"), "stderr: {stderr}");
}

/// A tag the fixture repository does not carry fails the clone as
/// `UPSTREAM_ERROR` with the `GIT_COMMAND_FAILED` detail
/// (docs/implementation/08 §Error codes).
#[test]
fn add_git_with_a_missing_tag_fails_upstream() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            &url,
            "--version",
            "no-such-tag",
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
    assert!(stderr.contains("GIT_COMMAND_FAILED"), "stderr: {stderr}");
}

/// A `git` entry on PATH that exists but cannot execute fails as
/// `UPSTREAM_ERROR` with the `GIT_SPAWN_FAILED` detail — the spawn arm
/// beside the not-found arm (docs/implementation/08 §Error codes).
#[test]
fn add_git_with_an_unrunnable_git_binary_fails_spawn() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    // A non-executable file named `git` on PATH: the spawn fails with a
    // permission error, not a lookup miss.
    let fake_bin = env.root.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::write(fake_bin.join("git"), "not an executable").expect("fake git written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fake_bin.join("git"), std::fs::Permissions::from_mode(0o644))
            .expect("fake git mode set");
    }
    let mut env_with_path = env;
    env_with_path.set("PATH", fake_bin.to_str().expect("bin path"));

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
    assert!(stderr.contains("GIT_SPAWN_FAILED"), "stderr: {stderr}");
}

/// A `tmp` data root left at mode 0755 fails the private-dir contract as
/// `UPSTREAM_ERROR` with the `GIT_TEMP_DIR` detail
/// (docs/implementation/09 §Local storage controls).
#[test]
fn add_git_refuses_an_unprivate_tmp_dir() {
    let env = Env::new();
    let repo_dir = env.root.join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = docs_repo(&repo_dir).expect("fixture repo");
    let url = repo_url(&repo.work_dir);
    let config = write_git_config(&env, &url);
    let db = env.root.join("docs.db");

    // The adapter verifies the tmp root is 0700 before it clones into it.
    let tmp = env.root.join("tmp");
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .expect("tmp mode set");
    }

    let (code, _stdout, stderr) = output(
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
    #[cfg(unix)]
    {
        assert_eq!(code, 3, "stderr: {stderr}");
        assert!(stderr.contains("GIT_TEMP_DIR"), "stderr: {stderr}");
    }
}
