//! End-to-end retrieval tests over the CLI: the `--json` result envelope
//! snapshots, the byte-identity contract, the cold-miss exit path, and
//! the open/verify/versions surfaces (docs/implementation/08 §CLI
//! contract, docs/implementation/13 §M4 test plan).
//!
//! Tests ingest offline through `add --fixture`, then drive the CLI the
//! way an agent would.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;

use devdocs_model::ResultEnvelope;
use serde_json::Value;

/// The index and page bodies the fixture replay serves.
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.md): Start here\n"
);

const INTRO_V1: &str = concat!(
    "# Intro\n\n",
    "The Acme SDK ships a client for every runtime.\n\n",
    "## Install\n\n",
    "Run the installer and export the key. ",
    "The upgrade guide walks v1 users through the setup.\n"
);

const INTRO_V2: &str = concat!(
    "# Intro\n\n",
    "The Acme SDK v2 ships a widget for every runtime.\n\n",
    "## Install\n\n",
    "Upgrade from v1: run the installer and export the key.\n"
);

/// The bytes of one search run, with `served_at` removed — the byte-identity
/// comparison excludes the wall-clock field (docs/implementation/13 §M4:
/// "byte-identity excludes the `served_at` wall-clock field").
fn envelope_without_served_at(stdout: &str) -> Value {
    let mut envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    envelope
        .as_object_mut()
        .expect("envelope is an object")
        .remove("served_at");
    envelope
}

/// An indexed fixture corpus: acme at versions 1.0 and 2.0.
fn indexed_env() -> (common::Env, PathBuf) {
    let env = common::Env::new();
    let dir = env.root.join("fixture");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let responses_v1 = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO_V1 },
    ]);
    let responses_v2 = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO_V2 },
    ]);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string(&responses_v1).expect("manifest serializes"),
    )
    .expect("manifest written");
    let config = env.root.join("devdocs.toml");
    std::fs::write(
        &config,
        concat!(
            "[sources.acme]\n",
            "kind = \"llms_txt\"\n",
            "url = \"https://docs.acme.test/llms.txt\"\n",
            "license = \"MIT\"\n",
            "status = \"approved_index\"\n"
        ),
    )
    .expect("config written");

    let add = |version: &str, responses: &Value| {
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(responses).expect("manifest serializes"),
        )
        .expect("manifest written");
        let (code, _stdout, stderr) = common::output(
            env.cmd(),
            &[
                "add",
                "https://docs.acme.test/llms.txt",
                "--fixture",
                dir.to_str().expect("fixture path"),
                "--version",
                version,
                "--json",
                "--config",
                config.to_str().expect("config path"),
            ],
        );
        assert_eq!(code, 0, "add {version} failed: {stderr}");
    };
    add("1.0", &responses_v1);
    add("2.0", &responses_v2);
    (env, config)
}

/// One fixture query snapshots to the exact `--json` envelope
/// (docs/implementation/13 §M4: "Snapshot the exact JSON for one fixture
/// query").
#[test]
fn search_snapshots_the_result_envelope() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = common::output(
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
/// output, with the `served_at` wall-clock field excluded
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
    let (code1, out1, _e1) = common::output(env.cmd(), &args);
    let (code2, out2, _e2) = common::output(env.cmd(), &args);
    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    assert_eq!(
        envelope_without_served_at(&out1),
        envelope_without_served_at(&out2),
        "the envelope must be identical except served_at"
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
    let (code, stdout, stderr) = common::output(
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
    let (code, stdout, stderr) = common::output(
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

/// `open` bounds the output by `max_tokens` and continues through the
/// cursor; the served content hash matches `verify`'s re-derivation.
#[test]
fn open_bounds_output_and_verifies() {
    let (env, config) = indexed_env();
    let cfg = ["--config", config.to_str().expect("config path")];

    // Search for the ref.
    let (code, stdout, stderr) =
        common::output(env.cmd(), &["search", "client", "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "search failed: {stderr}");
    let envelope: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let item = envelope.results.first().expect("one result");
    let doc_ref = item.doc_ref.clone();

    // open with a tiny budget truncates and carries the cursor.
    let (code, stdout, stderr) = common::output(
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
    let cursor = open1.next_cursor.clone().expect("cursor present");

    // Resume through the cursor: the pages tile the chunk.
    let (code, stdout, stderr) = common::output(
        env.cmd(),
        &[
            "open",
            &doc_ref,
            "--max-tokens",
            "1",
            "--cursor",
            &cursor,
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "open resume failed: {stderr}");
    let open2: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let page1 = open1.results.first().expect("page 1").snippet.clone();
    let page2 = open2.results.first().expect("page 2").snippet.clone();
    let (code, stdout, _stderr) =
        common::output(env.cmd(), &["open", &doc_ref, "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0);
    let full: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let whole = full.results.first().expect("full page").snippet.clone();
    assert!(
        whole.starts_with(page1.trim_end_matches('\n')),
        "page 1 tiles the whole"
    );
    assert!(
        whole.contains(page2.trim_end_matches('\n')),
        "page 2 is the rest"
    );

    // verify re-derives the stored hash.
    let (code, stdout, stderr) =
        common::output(env.cmd(), &["verify", &doc_ref, "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "verify failed: {stderr}");
    let verified: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    let v = verified.results.first().expect("verify result");
    assert_eq!(v.content_hash, item.content_hash);
}

/// `versions` lists the exact available versions.
#[test]
fn versions_list_the_exact_available_versions() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = common::output(
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
    let (code, stdout, stderr) = common::output(
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
    let (code, stdout, stderr) = common::output(
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

/// The migration-question pattern retrieves the served version and the
/// nearest older version, labeled through their `served_version` fields.
#[test]
fn migration_questions_retrieve_both_versions() {
    let (env, config) = indexed_env();
    let (code, stdout, stderr) = common::output(
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
