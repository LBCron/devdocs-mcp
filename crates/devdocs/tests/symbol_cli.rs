//! End-to-end `devdocs symbol` tests: the exact-lookup envelope, the miss
//! paths (`not_indexed`, `version_absent`, `symbol_absent`), and the
//! byte-identity contract (docs/implementation/08 §CLI contract,
//! docs/implementation/13 §M5).
//!
//! Tests ingest offline through `add --fixture`, then drive the CLI the
//! way an agent would.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{output, symbol_env};
use devdocs_model::{ErrorEnvelope, ResultEnvelope};
use serde_json::Value;

/// The bytes of one run, with the `served_at` entry cut at the string
/// level — the wall-clock field is the only run-varying value in the
/// envelope (docs/implementation/08 §CLI contract).
fn strip_served_at(raw: &str) -> String {
    let json = raw.trim();
    let key = "\"served_at\":\"";
    let at = json.rfind(key).expect("envelope carries served_at");
    let value_starts = at + key.len();
    let value_ends = value_starts + json[value_starts..].find('"').expect("value closes");
    let head = json[..at].strip_suffix(',').unwrap_or(&json[..at]);
    format!("{head}{}", &json[value_ends + 1..])
}

/// The exact lookup serves the seeded symbol through the standard result
/// envelope: the pinned version serves, the ref carries the symbol
/// fragment, and the request echo names the pinned version.
#[test]
fn symbol_lookup_serves_the_seeded_symbol() {
    let (env, config) = symbol_env();
    let args = ["symbol", "acme@1.0", "makeWidget()", "--json"];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            args.as_slice(),
            &["--config", config.to_str().expect("config path")],
        ]
        .concat(),
    );
    assert_eq!(code, 0, "symbol failed: {stderr}");
    let envelope: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(envelope.results.len(), 1, "{stdout}");
    let item = &envelope.results[0];
    assert_eq!(item.served_version, "1.0");
    assert_eq!(item.requested_version.as_deref(), Some("1.0"));
    assert_eq!(item.version_status, devdocs_model::VersionStatus::Verified);
    assert!(
        item.doc_ref.contains("#makewidget"),
        "ref carries the symbol fragment: {}",
        item.doc_ref
    );
}

/// A bare library target keeps the library scope and still serves.
#[test]
fn bare_library_target_serves_across_versions() {
    let (env, config) = symbol_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "symbol",
            "acme",
            "makeWidget",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 0, "symbol failed: {stderr}");
    let envelope: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(envelope.results.len(), 1, "{stdout}");
    assert_eq!(envelope.results[0].served_version, "1.0");
}

/// The symbol output is byte-identical across runs except `served_at`.
#[test]
fn identical_db_state_gives_byte_identical_output() {
    let (env, config) = symbol_env();
    let args = [
        "symbol",
        "acme@1.0",
        "makeWidget",
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

/// The `--json` envelope carries exactly the search envelope's field set —
/// the same top-level keys and the same per-item keys as a `search` run
/// over the same store (docs/implementation/08 §Result envelope).
#[test]
fn symbol_envelope_matches_the_search_shape() {
    let (env, config) = symbol_env();
    let config_arg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            ["symbol", "acme@1.0", "makeWidget", "--json"].as_slice(),
            &config_arg,
        ]
        .concat(),
    );
    assert_eq!(code, 0, "{stderr}");
    let symbol_value: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[["search", "widget", "--json"].as_slice(), &config_arg].concat(),
    );
    assert_eq!(code, 0, "{stderr}");
    let search_value: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");

    let keys = |value: &Value| {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("envelope is an object")
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(keys(&symbol_value), keys(&search_value));
    let item_keys = |value: &Value| {
        let mut keys: Vec<String> = value["results"][0]
            .as_object()
            .expect("result is an object")
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(item_keys(&symbol_value), item_keys(&search_value));
}

/// A library the index lacks is the cold miss: exit 2, `PKG_NOT_INDEXED`,
/// `miss_reason: not_indexed`.
#[test]
fn symbol_cold_miss_is_pkg_not_indexed() {
    let (env, config) = symbol_env();
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "symbol",
            "ghost@1.0",
            "makeWidget",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 2, "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.is_empty(), "a miss prints nothing on stdout");
    let envelope: ErrorEnvelope = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(envelope.error, "PKG_NOT_INDEXED");
    assert_eq!(
        envelope.miss_reason,
        Some(devdocs_model::MissReason::NotIndexed)
    );
}

/// A version the library does not carry is the other cold miss: exit 2,
/// `PKG_NOT_INDEXED`, `miss_reason: version_absent`.
#[test]
fn symbol_version_absent_is_pkg_not_indexed() {
    let (env, config) = symbol_env();
    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "symbol",
            "acme@9.9",
            "makeWidget",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 2, "{stderr}");
    let envelope: ErrorEnvelope = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(envelope.error, "PKG_NOT_INDEXED");
    assert_eq!(
        envelope.miss_reason,
        Some(devdocs_model::MissReason::VersionAbsent)
    );
}

/// A resolved target whose identifier is absent is the symbol miss: exit
/// 2, `NO_RESULT`, `miss_reason: symbol_absent`, hint naming `search`.
#[test]
fn symbol_absent_is_no_result_with_the_search_hint() {
    let (env, config) = symbol_env();
    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "symbol",
            "acme@1.0",
            "noSuchSymbol",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 2, "{stderr}");
    let envelope: ErrorEnvelope = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(envelope.error, "NO_RESULT");
    assert_eq!(
        envelope.miss_reason,
        Some(devdocs_model::MissReason::SymbolAbsent)
    );
    assert_eq!(
        envelope.hint.as_deref(),
        Some("run: devdocs search <symbol>")
    );
}

/// A malformed target is a usage error: exit 1.
#[test]
fn malformed_target_is_a_usage_error() {
    let (env, config) = symbol_env();
    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "symbol",
            "@1.0",
            "makeWidget",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ],
    );
    assert_eq!(code, 1, "{stderr}");
    let envelope: ErrorEnvelope = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(envelope.error, "USAGE");
}
