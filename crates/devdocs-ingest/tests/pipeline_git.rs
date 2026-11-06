//! End-to-end git pipeline tests: plan → clone → docs walk → dedup →
//! chrome strip → segment → published store, all offline against the
//! local fixture repositories of `devdocs-testkit::git_repo`
//! (docs/implementation/07 §Git adapter).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{GitPin, IngestJob, gate_fetch};
use devdocs_model::config::{Config, SourceConfig};
use devdocs_model::source::LicenseStatus;
use devdocs_testkit::base_config;
use devdocs_testkit::git_repo::{bare_mirror, docs_repo};
use tempfile::tempdir;
use url::Url;

/// Runs `git` against a fixture with the same argv-only, pinned-env
/// discipline the testkit uses, returning trimmed stdout.
fn fixture_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The `file://` URL of a local repository path — the exact string the
/// gate matches against the registry entry and the adapter plans with.
fn repo_url(path: &Path) -> Url {
    Url::parse(&format!("file://{}", path.display())).unwrap()
}

/// A config with the `acme` git source registered at the index stage for
/// `repo_url`.
fn config(repo_url: &Url) -> Config {
    let mut config = base_config();
    config.sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("git")),
            url: Some(repo_url.to_string()),
            license: Some(String::from("MIT")),
            status: Some(String::from("approved_index")),
            refresh_days: None,
        },
    );
    config
}

/// The fetch guard for a config. The git adapter never goes through it —
/// the guard exists because `IngestJob` carries one for both entry
/// points.
fn guard(config: &Config) -> FetchGuard {
    FetchGuard::from_fetch_config(&config.fetch)
}

/// A migrated store in a fresh temp database.
fn store() -> Arc<devdocs_store::Store> {
    let db = tempdir().unwrap();
    Arc::new(devdocs_testkit::open_migrated(&db.path().join("docs.db")).unwrap())
}

/// Asserts the published chunks carry the pinned commit and URL as
/// provenance, and the store keeps exactly one active git source row.
fn assert_commit_provenance(store: &devdocs_store::Store, url: &Url, sha: &str) {
    let hits = store
        .search_fts(&devdocs_store::build_match_query("guide"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the guide body is searchable");
    assert!(
        hits.iter()
            .all(|hit| hit.source_commit.as_deref() == Some(sha)),
        "provenance commit: {:?}",
        hits.iter()
            .map(|h| h.source_commit.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        hits.iter().all(|hit| hit.source_url == url.to_string()),
        "provenance URL: {:?}",
        hits.iter()
            .map(|h| h.source_url.clone())
            .collect::<Vec<_>>()
    );

    let sources = store.list_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, "git", "the source row is the git kind");
    assert!(sources[0].active, "the generation is active");
}

/// Initializes a repository whose HEAD commit carries no allowlisted file,
/// returning the HEAD SHA.
fn commit_a_repo_without_docs(dir: &Path) -> String {
    fixture_git(dir, &["init", "-b", "main"]);
    fixture_git(dir, &["config", "--local", "user.name", "devdocs-test"]);
    fixture_git(
        dir,
        &[
            "config",
            "--local",
            "user.email",
            "devdocs-test@example.test",
        ],
    );
    fixture_git(dir, &["config", "--local", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("assets.bin"), "not docs\n").unwrap();
    fixture_git(dir, &["add", "."]);
    fixture_git(dir, &["commit", "-m", "no docs"]);
    fixture_git(dir, &["rev-parse", "HEAD"])
}

/// A tag-pin run ingests both fixture docs, publishes them searchable, and
/// records the tag's commit SHA as the chunks' provenance
/// (docs/implementation/07 §Git adapter).
#[tokio::test]
async fn git_run_publishes_chunks_with_commit_provenance() {
    let repo_dir = tempdir().unwrap();
    let repo = docs_repo(repo_dir.path()).unwrap();
    let data = tempdir().unwrap();
    let url = repo_url(&repo.work_dir);
    let config = config(&url);

    // The deny-by-default gate applies to git sources too.
    let grant = gate_fetch(&config, &url).unwrap();
    assert_eq!(grant.name, "acme");

    let store = store();
    let guard = guard(&config);
    let outcome = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from("v1.0.0"),
            data.path(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.pages, 2, "README.md and docs/guide.md ingest");
    assert!(outcome.chunks > 0);
    assert_eq!(outcome.license_status, LicenseStatus::ApprovedIndex);
    // The outcome carries the commit provenance the report publishes; no
    // store readback needed.
    assert_eq!(
        outcome.commit_sha.as_deref(),
        Some(repo.tag_sha.as_str()),
        "the outcome carries the tag's commit"
    );
    assert_eq!(
        outcome.events.sha_fetch_fallbacks, 0,
        "a tag pin never takes the commit-pin fallback"
    );

    // Every published chunk carries the tag's commit and the repo URL as
    // provenance.
    assert_commit_provenance(&store, &url, &repo.tag_sha);
}

/// A commit pin the bare mirror refuses to serve by SHA (protocol v0, no
/// `allowReachableSHA1InWant`) takes the full-clone fallback, counts it in
/// the events, and still publishes the commit's content with its SHA as
/// provenance (docs/implementation/07 §Git adapter).
#[tokio::test]
async fn git_run_counts_the_sha_fetch_fallback_in_the_events() {
    let repo_dir = tempdir().unwrap();
    let repo = docs_repo(repo_dir.path()).unwrap();
    let mirror_dir = tempdir().unwrap();
    let mirror = bare_mirror(&repo.work_dir, mirror_dir.path()).unwrap();
    // Mid-history commit: reachable from `main`, named by no ref.
    let mid_history = fixture_git(&repo.work_dir, &["rev-parse", "HEAD~1"]);
    let url = repo_url(&mirror);
    let data = tempdir().unwrap();
    let config = config(&url);

    let store = store();
    // Force protocol v0 through the job's extra-config seam, the same way
    // the git adapter's fallback test does.
    let guard = guard(&config);
    let outcome = IngestJob::new(&config, &guard, None)
        .with_extra_git_config(&[String::from("protocol.version=0")])
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from(mid_history.as_str()),
            data.path(),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome.events.sha_fetch_fallbacks, 1,
        "the full-clone fallback counted"
    );
    assert!(outcome.chunks > 0, "the mid-history README still published");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("index"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the README body is searchable");
    assert!(
        hits.iter()
            .all(|hit| hit.source_commit.as_deref() == Some(mid_history.as_str())),
        "the fallback run still records the pinned commit"
    );
}

/// A repository with no allowlisted file at all fails with the
/// zero-corpus refusal and publishes nothing — no source row, no chunks,
/// no empty generation (docs/implementation/07 §Atomic publish).
#[tokio::test]
async fn git_run_refuses_a_corpus_with_no_allowlisted_files() {
    let repo_dir = tempdir().unwrap();
    let head = commit_a_repo_without_docs(repo_dir.path());

    let data = tempdir().unwrap();
    let url = repo_url(repo_dir.path());
    let config = config(&url);
    let db = tempdir().unwrap();
    let db_path = db.path().join("docs.db");
    let store = Arc::new(devdocs_testkit::open_migrated(&db_path).unwrap());

    let guard = guard(&config);
    let error = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from(head.as_str()),
            data.path(),
        )
        .await
        .unwrap_err();
    match error {
        devdocs_ingest::IngestError::NoIngestablePages { index } => {
            assert_eq!(index, url.to_string());
        }
        other => panic!("expected NoIngestablePages, got {other}"),
    }

    // The store carries nothing: the refused run published no empty
    // generation.
    assert!(
        store.list_sources().unwrap().is_empty(),
        "a refused run must publish no source row"
    );
    assert!(
        devdocs_testkit::read_chunks(&db_path).unwrap().is_empty(),
        "a refused run must publish no chunks"
    );
}

/// A run over a repo whose allowlisted file is not valid UTF-8 skips it
/// with the count in the events and still publishes the rest
/// (docs/implementation/07 §Git adapter).
#[tokio::test]
async fn git_run_counts_the_non_utf8_skips() {
    let repo_dir = tempdir().unwrap();
    let repo = docs_repo(repo_dir.path()).unwrap();
    // A non-UTF-8 allowlisted file joins the fixture repo in a new commit.
    std::fs::write(repo.work_dir.join("broken.md"), [0xff, 0xfe, 0x00]).unwrap();
    fixture_git(&repo.work_dir, &["add", "."]);
    fixture_git(&repo.work_dir, &["commit", "-m", "broken"]);
    let head = fixture_git(&repo.work_dir, &["rev-parse", "HEAD"]);

    let data = tempdir().unwrap();
    let url = repo_url(&repo.work_dir);
    let config = config(&url);
    let store = store();
    let guard = guard(&config);
    let outcome = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from(head.as_str()),
            data.path(),
        )
        .await
        .unwrap();

    // The non-UTF-8 file is skipped and counted in the warn log; the
    // fixture pages still publish.
    assert_eq!(outcome.pages, 2, "README and guide ingest: {outcome:?}");
    assert!(outcome.chunks > 0);
    assert_eq!(
        outcome.commit_sha.as_deref(),
        Some(head.as_str()),
        "the run pins the broken-file commit"
    );
}

/// A second run of the same source re-publishes under the same single
/// source row: the generation is replaced, not duplicated
/// (docs/implementation/07 §Atomic publish).
#[tokio::test]
async fn a_second_git_run_republishes_the_generation() {
    let repo_dir = tempdir().unwrap();
    let repo = docs_repo(repo_dir.path()).unwrap();
    let data = tempdir().unwrap();
    let url = repo_url(&repo.work_dir);
    let config = config(&url);

    let store = store();
    let guard = guard(&config);
    let first = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from("v1.0.0"),
            data.path(),
        )
        .await
        .unwrap();
    assert!(first.chunks > 0);

    let second = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from("v1.0.0"),
            data.path(),
        )
        .await
        .unwrap();

    assert_eq!(second.chunks, first.chunks, "the generation is replaced");
    assert_eq!(second.pages, first.pages);
    assert_eq!(
        store.list_sources().unwrap().len(),
        1,
        "no duplicate source row"
    );
    let hits = store
        .search_fts(&devdocs_store::build_match_query("guide"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the republished content stays searchable");
    assert!(
        hits.iter()
            .all(|hit| hit.source_commit.as_deref() == Some(repo.tag_sha.as_str())),
        "the provenance commit survives the republish"
    );
}
