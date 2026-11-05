//! End-to-end retrieval tests over the CLI: the `--json` result envelope
//! snapshots, the byte-identity contract, the cold-miss exit path, and
//! the open/verify/versions surfaces (docs/implementation/08 §CLI
//! contract, docs/implementation/13 §M4 test plan).
//!
//! Tests ingest offline through `add --fixture`, then drive the CLI the
//! way an agent would.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{indexed_doc, indexed_env, output};
use devdocs_model::ResultEnvelope;
use serde_json::Value;

/// The bytes of one search run, with the `served_at` entry cut at the
/// string level — the wall-clock field is the only run-varying value in
/// the envelope (docs/implementation/13 §M4: "byte-identity excludes the
/// `served_at` wall-clock field"). The entry and the comma before it go,
/// so the rest stays valid JSON.
fn strip_served_at(raw: &str) -> String {
    let json = raw.trim();
    // The entry is the envelope's last field, so the last occurrence of
    // the key is the top-level one.
    let key = "\"served_at\":\"";
    let at = json.rfind(key).expect("envelope carries served_at");
    let value_starts = at + key.len();
    let value_ends = value_starts + json[value_starts..].find('"').expect("value closes");
    let head = json[..at].strip_suffix(',').unwrap_or(&json[..at]);
    format!("{head}{}", &json[value_ends + 1..])
}

/// One fixture query snapshots to the exact `--json` envelope
/// (docs/implementation/13 §M4: "Snapshot the exact JSON for one fixture
/// query").
#[test]
fn search_snapshots_the_result_envelope() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "search failed: {stderr}");
    let mut envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    envelope["served_at"] = Value::String(String::from("<served_at>"));
    insta::assert_json_snapshot!("search_envelope", envelope, {
        ".results[].confidence" => "<confidence>"
    });
}

/// Running the same query twice against the same DB produces byte-identical
/// output: the raw stdout strings match after the `served_at` entry is cut,
/// so key order, whitespace, and value drift all fail
/// (docs/implementation/08 §CLI contract, adjusted by the issue #8
/// comparison rule).
#[test]
fn identical_db_state_gives_byte_identical_output() {
    let (env, config) = indexed_env();
    let args = [
        "search",
        "client",
        "--json",
        "--config",
        config.to_str().expect("config path"),
    ];
    let (code1, out1, _e1) = output(env.cmd(), &args);
    let (code2, out2, _e2) = output(env.cmd(), &args);
    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    assert_eq!(
        strip_served_at(&out1),
        strip_served_at(&out2),
        "the envelope must be byte-identical except served_at"
    );
}

/// A query that matches nothing exits 2 with the miss envelope: an empty
/// index names `PKG_NOT_INDEXED`; a matching-nothing query on an indexed
/// corpus names `NO_RESULT` with `low_confidence`.
#[test]
fn cold_misses_exit_two() {
    let (env, config) = indexed_env();
    let cfg = ["--config", config.to_str().expect("config path")];

    // A query the corpus cannot match.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &["search", "zzz-nonexistent-thing", "--json", cfg[0], cfg[1]],
    );
    assert_eq!(code, 2, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "NO_RESULT");
    assert_eq!(error["miss_reason"], "low_confidence");
}

/// A search for an absent library exits 2 with `PKG_NOT_INDEXED` and the
/// add hint (docs/implementation/08 §Error codes).
#[test]
fn an_absent_library_exits_two_with_the_add_hint() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--library",
            "missing-lib",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 2, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "PKG_NOT_INDEXED");
    assert_eq!(error["miss_reason"], "not_indexed");
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("devdocs add")
    );
}

/// `open` bounds the output by `max_tokens`: a tiny budget truncates and
/// carries the resume cursor.
#[test]
fn open_under_a_tiny_budget_truncates_and_carries_a_cursor() {
    let (env, config) = indexed_env();
    let doc_ref = indexed_doc(&env, &config).doc_ref;
    let cfg = ["--config", config.to_str().expect("config path")];

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "open",
            &doc_ref,
            "--max-tokens",
            "1",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "open failed: {stderr}");
    let open1: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    assert!(open1.truncated, "a tiny budget truncates");
    open1.next_cursor.expect("cursor present");
}

/// Opens `doc_ref` under a one-token budget and follows `next_cursor`
/// to the end, returning the joined snippets and the page count. The
/// cap keeps a cursor that never settles from hanging the test.
fn tiled_tiny_budget_pages(
    env: &common::Env,
    doc_ref: &str,
    config: &std::path::Path,
) -> (String, usize) {
    let cfg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "open",
            doc_ref,
            "--max-tokens",
            "1",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "open failed: {stderr}");
    let mut page: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let mut tiled = String::new();
    let mut pages = 0;
    loop {
        assert!(pages < 32, "the cursor never settles: {pages} pages served");
        let item = page.results.first().expect("page serves a result");
        tiled.push_str(&item.snippet);
        pages += 1;
        let Some(cursor) = &page.next_cursor else {
            break;
        };
        let (code, stdout, stderr) = output(
            env.cmd(),
            &[
                "open",
                doc_ref,
                "--max-tokens",
                "1",
                "--cursor",
                cursor,
                "--json",
                cfg[0],
                cfg[1],
            ],
        );
        assert_eq!(code, 0, "open resume failed: {stderr}");
        page = serde_json::from_str(stdout.trim()).expect("envelope");
    }
    (tiled, pages)
}

/// Resuming through the cursor tiles the chunk: following `next_cursor`
/// to the end joins to the exact served text of the default-budget open,
/// with no page repeated and no page missing.
#[test]
fn cursor_resume_tiles_the_whole_chunk() {
    let (env, config) = indexed_env();
    let doc_ref = indexed_doc(&env, &config).doc_ref;
    let cfg = ["--config", config.to_str().expect("config path")];

    // The whole chunk under the default budget, for the tiling comparison.
    let (code, stdout, stderr) = output(env.cmd(), &["open", &doc_ref, "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "open failed: {stderr}");
    let full: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let whole = full.results.first().expect("full page").snippet.clone();

    let (tiled, pages) = tiled_tiny_budget_pages(&env, &doc_ref, &config);
    assert!(pages > 1, "a tiny budget must cut the chunk into pages");
    assert_eq!(tiled, whole, "the pages tile the whole chunk exactly");
}

/// `verify` re-derives the stored hash: it matches the search result's
/// `content_hash`.
#[test]
fn verify_re_derives_the_search_hash() {
    let (env, config) = indexed_env();
    let item = indexed_doc(&env, &config);
    let doc_ref = item.doc_ref.clone();
    let cfg = ["--config", config.to_str().expect("config path")];

    let (code, stdout, stderr) = output(env.cmd(), &["verify", &doc_ref, "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "verify failed: {stderr}");
    let verified: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let v = verified.results.first().expect("verify result");
    assert_eq!(v.content_hash, item.content_hash);
}

/// `verify` re-derives the SHA-256 and reports `HASH_MISMATCH` when the
/// stored text drifted (docs/implementation/06 §Hashing policy).
#[test]
fn verify_reports_hash_mismatch_on_drift() {
    let (env, config) = indexed_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let doc_ref = indexed_doc(&env, &config).doc_ref;

    // Tamper with the stored text behind the CLI's back: the index
    // drifted. DEVDOCS_HOME pins the data root, so the index lives at
    // <root>/data/docs.db.
    devdocs_testkit::corrupt_one_chunk_text(&env.root.join("data/docs.db")).expect("tamper");

    let (code, stdout, stderr) = output(env.cmd(), &["verify", &doc_ref, "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 3, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "HASH_MISMATCH");
}

/// `versions` lists the exact available versions.
#[test]
fn versions_list_the_exact_available_versions() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "versions",
            "acme",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "versions failed: {stderr}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    let versions: Vec<&str> = envelope["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|r| r["version"].as_str())
        .collect();
    assert!(versions.contains(&"1.0"), "versions: {versions:?}");
    assert!(versions.contains(&"2.0"), "versions: {versions:?}");
}

/// A requested version that no source carries never silently serves
/// `latest`: without a library, an exact-request hit verifies and every
/// other hit labels `fallback` with the request riding in the envelope.
#[test]
fn an_absent_version_serves_nearest_as_fallback() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--version",
            "9.9.9",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    let results = envelope["results"].as_array().expect("results");
    assert_ne!(results.len(), 0);
    assert_eq!(results[0]["requested_version"], "9.9.9");
    assert_ne!(results[0]["served_version"], "9.9.9");
    assert_eq!(results[0]["version_status"], "fallback");

    // Without any version request, the alias default labels the results.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    let results = envelope["results"].as_array().expect("results");
    assert_eq!(results[0]["version_status"], "alias");
}

/// Without `--json`, `search` prints one human-readable line per result in
/// fusion order (docs/implementation/08 §CLI contract).
#[test]
fn search_without_json_prints_summary_lines() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_ne!(lines.len(), 0, "stdout: {stdout}");
    for line in &lines {
        // `<ref>  <title>  [<version> · <status> · <n> tokens]`
        assert!(line.contains("  ["), "line: {line}");
        assert!(line.contains(" · "), "line: {line}");
        assert!(line.ends_with(" tokens]"), "line: {line}");
    }
}

/// Under a tiny token budget, the non-JSON `search` output carries the
/// truncation notice naming the resume cursor.
#[test]
fn search_without_json_prints_the_truncation_notice_under_a_tiny_budget() {
    let (mut env, config) = indexed_env();
    env.set("DEVDOCS_SEARCH_MAX_TOKENS_DEFAULT", "1");
    // "upgrade installer" matches chunks in both versions, so rank 2 must
    // fall off the one-token budget.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "upgrade installer",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let notice_line = stdout
        .lines()
        .find(|line| line.starts_with("…truncated; resume with --cursor "))
        .expect("truncation notice");
    assert!(
        !notice_line.ends_with("--cursor "),
        "a cursor is named: {notice_line:?}"
    );
}

/// Without `--json`, `open` prints the served text; under a tiny budget it
/// appends the continuation notice.
#[test]
fn open_without_json_prints_the_text_and_the_truncation_notice() {
    let (env, config) = indexed_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let doc_ref = indexed_doc(&env, &config).doc_ref;

    let (code, stdout, stderr) = output(
        env.cmd(),
        &["open", &doc_ref, "--max-tokens", "1", cfg[0], cfg[1]],
    );
    assert_eq!(code, 0, "open failed: {stderr}");
    let notice_line = stdout
        .lines()
        .last()
        .expect("notice line")
        .trim()
        .to_owned();
    assert!(
        notice_line.starts_with("…truncated; resume with --cursor "),
        "stdout: {stdout:?}"
    );
    let cursor = notice_line
        .rsplit_once("--cursor ")
        .expect("cursor present")
        .1
        .trim()
        .to_owned();
    assert_ne!(cursor, "");

    // A full budget prints the served text with no notice.
    let (code, stdout, stderr) = output(env.cmd(), &["open", &doc_ref, cfg[0], cfg[1]]);
    assert_eq!(code, 0, "open failed: {stderr}");
    assert!(!stdout.contains("…truncated"), "stdout: {stdout:?}");
    assert!(stdout.contains("Intro"), "served text: {stdout:?}");
}

/// Without `--json`, `verify` prints the ref and its re-derived hash.
#[test]
fn verify_without_json_prints_the_verified_line() {
    let (env, config) = indexed_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let item = indexed_doc(&env, &config);
    let doc_ref = item.doc_ref.clone();
    let hash = item.content_hash.clone();

    let (code, stdout, stderr) = output(env.cmd(), &["verify", &doc_ref, cfg[0], cfg[1]]);
    assert_eq!(code, 0, "verify failed: {stderr}");
    assert_eq!(
        stdout.trim(),
        format!("{doc_ref} verified ({hash})"),
        "stdout: {stdout:?}"
    );
}

/// Without `--json`, `versions` prints one discovery line per version.
#[test]
fn versions_without_json_print_the_discovery_lines() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "versions",
            "acme",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "versions failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.contains(&"acme 1.0 (llms_txt · approved_index · active)"),
        "stdout: {stdout:?}"
    );
    assert!(
        lines.contains(&"acme 2.0 (llms_txt · approved_index · active)"),
        "stdout: {stdout:?}"
    );
}

/// `open` with a malformed ref is a usage error: exit 1 with the `USAGE`
/// envelope on stderr (docs/implementation/08 §Error codes).
#[test]
fn open_with_a_malformed_ref_exits_one() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "open",
            "not-a-docs-ref",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "USAGE");
}

/// A search against an unusable index file is an upstream failure: exit 3
/// with `UPSTREAM_ERROR` (docs/implementation/08 §Error codes).
#[test]
fn a_search_on_a_broken_index_exits_three() {
    let (mut env, config) = indexed_env();
    let broken = env.root.join("broken/docs.db");
    std::fs::create_dir_all(broken.parent().expect("parent")).expect("dir");
    std::fs::write(&broken, b"this is not a sqlite database").expect("garbage index");
    // `DEVDOCS_HOME` overrides `DEVDOCS_INDEX_PATH`, so the home layer
    // drops and the XDG data root applies while the index path points at
    // the garbage file.
    env.without_home();
    env.set("DEVDOCS_INDEX_PATH", broken.to_str().expect("index path"));

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "client",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 3, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "UPSTREAM_ERROR");
}

/// The migration-question pattern retrieves the served version and the
/// nearest older version, labeled through their `served_version` fields.
#[test]
fn migration_questions_retrieve_both_versions() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "search",
            "upgrade installer",
            "--library",
            "acme",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "search failed: {stderr}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    let served: Vec<&str> = envelope["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|r| r["served_version"].as_str())
        .collect();
    assert!(served.contains(&"2.0"), "both versions serve: {served:?}");
    assert!(served.contains(&"1.0"), "both versions serve: {served:?}");
}
