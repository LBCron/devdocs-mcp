//! End-to-end CLI tests through the real binary: exit codes 0-4, the
//! usage remap, stdout purity, and the config subcommands
//! (docs/implementation/04 §CLI end-to-end).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;

use common::{Env, output};
use serde_json::Value;

/// One `assert_cmd` test per exit code: 0 for a completed operation.
#[test]
fn exit_zero_when_config_show_succeeds() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("\"fetch\""), "stdout: {stdout}");
}

/// Exit 1 with the USAGE envelope when a flag does not parse; clap's
/// default exit 2 is never observable (docs/05 §Error model).
#[test]
fn exit_one_on_usage_error() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["--nope"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout must stay clean: {stdout:?}");
    let error: Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr is not JSON: {stderr:?}: {e}"));
    assert_eq!(error["error"], "USAGE");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("--nope")
    );
}

/// Exit 2 for a valid query that serves nothing. The cold miss carries
/// `PKG_NOT_INDEXED` with the `not_indexed` reason; the plain
/// `NO_RESULT` code serves a valid-query miss
/// (docs/implementation/08 §Error codes).
#[test]
fn exit_two_on_no_result() {
    let mut env = Env::new();
    env.inject("PKG_NOT_INDEXED");
    let (code, stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "PKG_NOT_INDEXED");
    assert_eq!(error["miss_reason"], "not_indexed");
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("devdocs add")
    );

    let mut env = Env::new();
    env.inject("NO_RESULT");
    let (code, _stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 2);
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "NO_RESULT");
    assert_eq!(error["miss_reason"], "low_confidence");
}

/// Exit 3 for an upstream failure.
#[test]
fn exit_three_on_upstream_error() {
    let mut env = Env::new();
    env.inject("FETCH_BLOCKED");
    let (code, _stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 3);
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "FETCH_BLOCKED");
}

/// Exit 4 when the budget is consumed with no deliverable content.
#[test]
fn exit_four_on_budget_exceeded() {
    let mut env = Env::new();
    env.inject("BUDGET_EXCEEDED");
    let (code, _stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 4);
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "BUDGET_EXCEEDED");
}

/// `--help` exits 0 and registers every docs/08 command on one screen.
#[test]
fn help_registers_every_command() {
    let env = Env::new();
    let (code, stdout, _stderr) = output(env.cmd(), &["--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.lines().count() <= 30,
        "help fits one screen:\n{stdout}"
    );
    for name in [
        "add", "sync", "list", "search", "symbol", "open", "verify", "versions", "source", "skill",
        "mcp", "db", "config", "eval",
    ] {
        assert!(
            stdout.contains(&format!("  {name}")),
            "missing command {name}"
        );
    }
}

/// `--version` exits 0 and prints the crate version.
#[test]
fn version_prints_the_crate_version() {
    let env = Env::new();
    let (code, stdout, _stderr) = output(env.cmd(), &["--version"]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        format!("devdocs {}", env!("CARGO_PKG_VERSION"))
    );
}

/// `--json` keeps stdout to the single envelope line; stderr stays empty.
#[test]
fn json_stdout_stays_clean() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "stderr must stay empty: {stderr:?}");
    let value: Value = serde_json::from_str(stdout.trim()).expect("stdout is one JSON line");
    assert_eq!(value["results"], serde_json::json!([]));
    assert_eq!(value["truncated"], false);
    assert_eq!(value["next_cursor"], Value::Null);
    assert!(value["served_at"].is_string());
}

/// Non-TTY runs carry no ANSI escapes.
#[test]
fn non_tty_output_has_no_ansi() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["--help"]);
    assert_eq!(code, 0);
    assert!(!stdout.contains('\u{1b}'), "no ANSI in stdout");
    assert!(!stderr.contains('\u{1b}'), "no ANSI in stderr");
}

/// `config init` writes the commented user file and refuses to overwrite
/// without an explicit force flag.
#[test]
fn config_init_refuses_overwrite_without_force() {
    let env = Env::new();
    let (code, _stdout, stderr) = output(env.cmd(), &["config", "init"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let user = env.root.join("config").join("devdocs").join("config.toml");
    let written = std::fs::read_to_string(&user).expect("user config written");
    assert!(written.contains("[fetch]"));
    assert!(written.contains("[search]"));

    // The refusal comes from the exclusive create, so it holds even when
    // the file appears after the command starts.
    let (code, stdout, stderr) = output(env.cmd(), &["config", "init"]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "USAGE");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("--force")
    );

    let (code, _stdout, stderr) = output(env.cmd(), &["config", "init", "--force"]);
    assert_eq!(code, 0, "stderr: {stderr}");
}

/// `config validate <path>` reports the failing key path and exits 1.
#[test]
fn config_validate_reports_the_failing_key_path() {
    let env = Env::new();
    let bad = env.root.join("bad.toml");
    std::fs::write(&bad, "[fetch]\nconkurrency = 4\n").expect("write bad config");
    let bad = bad.display().to_string();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "validate", &bad]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "CONFIG_INVALID");
    assert_eq!(error["key_path"], "fetch.conkurrency");
}

/// `config validate` names the entry and the key for a bad source key.
#[test]
fn config_validate_names_the_source_entry_path() {
    let env = Env::new();
    let bad = env.root.join("sources.toml");
    std::fs::write(&bad, "[sources.nextjs]\nurll = \"https://x\"\n").expect("write bad config");
    let bad = bad.display().to_string();
    let (code, _stdout, stderr) = output(env.cmd(), &["config", "validate", &bad]);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "CONFIG_INVALID");
    assert_eq!(error["key_path"], "sources.nextjs.urll");
}

/// `config validate` rejects a malformed source URL and names the entry.
#[test]
fn config_validate_rejects_a_bad_source_url() {
    let env = Env::new();
    let bad = env.root.join("urls.toml");
    std::fs::write(&bad, "[sources.nextjs]\nurl = \"not a url\"\n").expect("write bad config");
    let bad = bad.display().to_string();
    let (code, _stdout, stderr) = output(env.cmd(), &["config", "validate", &bad]);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "CONFIG_INVALID");
    assert_eq!(error["key_path"], "sources.nextjs.url");
}

/// A missing explicit `--config` file fails instead of silently running on
/// defaults; a discovered devdocs.toml stays optional.
#[test]
fn missing_explicit_config_fails() {
    let env = Env::new();
    let missing = env.root.join("missing.toml");
    let missing = missing.display().to_string();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show", "--config", &missing]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "CONFIG_INVALID");
    assert_eq!(error["key_path"], missing);
}

/// `mcp --transport` accepts only the documented `stdio` value.
#[test]
fn mcp_transport_rejects_unknown_values() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["mcp", "--transport", "http"]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "USAGE");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("stdio")
    );

    let (code, stdout, stderr) = output(env.cmd(), &["mcp"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(value["results"], serde_json::json!([]));
}

/// `config validate` passes a well-formed file and exits 0.
#[test]
fn config_validate_accepts_a_valid_file() {
    let env = Env::new();
    let good = env.root.join("good.toml");
    std::fs::write(&good, "[search]\nlimit_default = 5\n").expect("write good config");
    let good = good.display().to_string();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "validate", &good]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("\"valid\":true"));
}

/// The precedence chain over real files: user < project < env < flags.
#[test]
fn config_precedence_user_project_env_flags() {
    // The user layer: limit 5, concurrency 7.
    let mut env = Env::new();
    let user = env.root.join("config").join("devdocs");
    std::fs::create_dir_all(&user).expect("create user config dir");
    std::fs::write(
        user.join("config.toml"),
        "[search]\nlimit_default = 5\n\n[fetch]\nconcurrency = 7\n",
    )
    .expect("write user config");

    // The project layer: limit 3, concurrency 2.
    std::fs::write(
        env.root.join("devdocs.toml"),
        "[search]\nlimit_default = 3\n\n[fetch]\nconcurrency = 2\n",
    )
    .expect("write project config");

    // Project beats user.
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(shown["search"]["limit_default"], 3, "project beats user");
    assert_eq!(shown["fetch"]["concurrency"], 2, "project beats user");

    // Env beats project.
    env.set("DEVDOCS_SEARCH_LIMIT_DEFAULT", "9");
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(shown["search"]["limit_default"], 9, "env beats project");
    assert_eq!(
        shown["fetch"]["concurrency"], 2,
        "untouched key keeps its value"
    );

    // Flags beat env.
    env.set("DEVDOCS_FETCH_TIMEOUT_MS", "4242");
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show", "--timeout-ms", "99"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(shown["fetch"]["timeout_ms"], 99, "flags beat env");
    assert_eq!(shown["fetch"]["timeout_ms"], 99);
}

/// `DEVDOCS_HOME` overrides the index path and the data root.
#[test]
fn devdocs_home_overrides_the_data_root() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "path"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let paths: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    let expected = env.root.join("data").join("docs.db");
    assert_eq!(
        paths["index_db"].as_str().expect("index_db is a string"),
        expected.display().to_string()
    );
    assert_eq!(
        paths["data_root"].as_str().expect("data_root is a string"),
        env.root.join("data").display().to_string()
    );
}

/// `config path` and `config show` agree on the index database when the
/// config files set `[index] path`.
#[test]
fn config_path_agrees_with_config_show_on_the_index_db() {
    let mut env = Env::new();
    env.without_home();
    let db = env.root.join("project.db");
    // A TOML literal string (single quotes) never processes escapes, so the
    // Windows backslash separators survive the round trip.
    std::fs::write(
        env.root.join("devdocs.toml"),
        format!("[index]\npath = '{}'\n", db.display()),
    )
    .expect("write project config");

    let (code, stdout, stderr) = output(env.cmd(), &["config", "path"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let paths: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(
        paths["index_db"].as_str().expect("index_db is a string"),
        db.display().to_string()
    );

    let (code, stdout, stderr) = output(env.cmd(), &["config", "show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(
        shown["index"]["path"].as_str().expect("path is a string"),
        db.display().to_string()
    );
}

/// `XDG_DATA_HOME` sets the data root when `DEVDOCS_HOME` is unset.
#[test]
fn xdg_data_home_sets_the_data_root() {
    let mut env = Env::new();
    env.without_home();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "path"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let paths: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(
        paths["data_root"].as_str().expect("data_root is a string"),
        env.root.join("data").join("devdocs").display().to_string()
    );
}

/// `config show` prints the merged configuration as JSON (snapshot).
#[test]
fn config_show_snapshot() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["config", "show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    insta::assert_json_snapshot!(shown, {
        // The path is machine-specific and the user agent carries the crate
        // version, which release-plz bumps under this snapshot; both are
        // normalized so a version bump never red-eyes this test again.
        ".index.path" => "[DB_PATH]",
        ".fetch.user_agent" => "[USER_AGENT]",
    });
}

/// The index and page bodies the fixture replay serves. The index links
/// the printable HTML page only: its `Link` header names the markdown
/// twin, which is what gets ingested (llmstxt.org v2).
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.html): Start here\n"
);

const INTRO: &str = concat!(
    "# Intro\n\n",
    "The Acme SDK ships a client for every runtime.\n\n",
    "## Install\n\n",
    "Run the installer and export the key.\n"
);

/// Writes the fixture directory and returns its path.
fn write_fixture(env: &Env) -> PathBuf {
    let dir = env.root.join("fixture");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        // The printable HTML page names its markdown twin in a `Link`
        // header (llmstxt.org v2), so the twin is what gets ingested.
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": "<https://docs.acme.test/intro.md>; rel=\"alternate\"; type=\"text/markdown\"" }, "body": "<!doctype html><html><body>printable</body></html>" },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
    ]);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string(&responses).expect("manifest serializes"),
    )
    .expect("manifest written");
    dir
}

/// Writes the config file, with or without the acme registry entry.
fn write_config(env: &Env, with_source: bool) -> PathBuf {
    let body = if with_source {
        concat!(
            "[sources.acme]\n",
            "kind = \"llms_txt\"\n",
            "url = \"https://docs.acme.test/llms.txt\"\n",
            "license = \"MIT\"\n"
        )
    } else {
        ""
    };
    let path = env.root.join("devdocs.toml");
    std::fs::write(&path, body).expect("config written");
    path
}

/// `add --fixture` runs the whole vertical slice offline: the store then
/// lists the source, and the chunks carry their hashes and breadcrumbs.
#[test]
fn add_with_a_fixture_ingests_offline() {
    let env = Env::new();
    let fixture = write_fixture(&env);
    let config = write_config(&env, true);
    let db = env.root.join("docs.db");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            "https://docs.acme.test/llms.txt",
            "--fixture",
            fixture.to_str().expect("fixture path"),
            "--version",
            "1.0",
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
    assert_eq!(added["version"], "1.0");
    assert_eq!(
        added["events"]["markdown_twins_used"], 1,
        "the printable page ingested through its markdown twin: {added}"
    );
    assert!(added["chunks"].as_u64().expect("chunks") > 0);

    assert_list_names_acme(&env, &config, &db);
    assert_chunks_recorded(&db);
}

/// `list --json` names exactly the acme source, active at the index stage.
fn assert_list_names_acme(env: &Env, config: &std::path::Path, db: &std::path::Path) {
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
    assert_eq!(list["results"].as_array().expect("results").len(), 1);
    assert_eq!(list["results"][0]["library"], "acme");
    assert_eq!(list["results"][0]["version"], "1.0");
    assert_eq!(list["results"][0]["kind"], "llms_txt");
    assert_eq!(list["results"][0]["license_status"], "approved_index");
    assert_eq!(list["results"][0]["active"], true);
}

/// The store-level assertions for the published intro page: chunks, hashes,
/// breadcrumbs, and the contextual header.
fn assert_chunks_recorded(db: &std::path::Path) {
    let chunks = devdocs_testkit::read_chunks(db).expect("chunks read");
    assert!(chunks.len() > 1, "index intro page yields several chunks");
    assert!(chunks.iter().all(|chunk| chunk.library == "acme"));
    for chunk in &chunks {
        assert_eq!(chunk.content_hash.len(), 32);
        // The stored text starts with the contextual header.
        assert!(
            chunk.text.starts_with("[acme 1.0 › "),
            "text: {}",
            chunk.text
        );
        // The breadcrumb is a JSON array of strings rooted at the page H1.
        let path: Vec<String> = serde_json::from_str(&chunk.section_path).expect("breadcrumb json");
        assert_eq!(path.first().map(String::as_str), Some("Intro"));
        // The header is excluded from the hash: strip it and re-derive.
        let body = chunk
            .text
            .split_once("\n\n")
            .map_or(chunk.text.as_str(), |(_, body)| body);
        assert_eq!(
            chunk.content_hash,
            devdocs_store::content_hash(body).to_vec()
        );
    }
    // The install section of the intro page carries its breadcrumb.
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.text.contains("installer") && chunk.section_path.contains("Install"))
    );
}

/// A source with no `[sources]` entry refuses with `FETCH_BLOCKED` before
/// anything moves: exit 3, no database file, no bytes.
#[test]
fn add_without_a_registry_entry_refuses_fetch_blocked() {
    let env = Env::new();
    let fixture = write_fixture(&env);
    let config = write_config(&env, false);
    let db = env.root.join("docs.db");
    assert!(!db.exists());

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            "https://docs.acme.test/llms.txt",
            "--fixture",
            fixture.to_str().expect("fixture path"),
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 3, "stdout: {stdout} stderr: {stderr}");
    assert!(stderr.contains("FETCH_BLOCKED"), "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout stays clean: {stdout}");
    assert!(!db.exists(), "a refused add must not create the database");
}

/// An unsupported `--kind` is a usage error, never a silent flag drop: the
/// llms.txt pipeline must not run for a registered URL with an unknown
/// kind (`openapi` has no adapter yet).
#[test]
fn add_rejects_an_unsupported_kind() {
    let env = Env::new();
    let fixture = write_fixture(&env);
    let config = write_config(&env, true);
    let db = env.root.join("kind.db");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "add",
            "https://docs.acme.test/llms.txt",
            "--kind",
            "openapi",
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
    assert!(stderr.contains("not supported yet"), "stderr: {stderr}");
    assert!(!db.exists(), "a rejected add must not create the database");
}

/// Writes a fixture manifest from raw entry objects and returns its path.
fn write_manifest(env: &Env, entries: &Value) -> PathBuf {
    let dir = env.root.join("fixture-custom");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    std::fs::write(dir.join("manifest.json"), entries.to_string()).expect("manifest written");
    dir
}

/// The `add` argument list for the acme fixture replay.
fn add_args<'a>(
    fixture: &'a std::path::Path,
    config: &'a std::path::Path,
    db: &'a std::path::Path,
    json: bool,
) -> Vec<&'a str> {
    let mut args = vec![
        "add",
        "https://docs.acme.test/llms.txt",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ];
    if json {
        args.push("--json");
    }
    args.extend([
        "--config",
        config.to_str().expect("config path"),
        "--db",
        db.to_str().expect("db path"),
    ]);
    args
}

/// The human (non-`--json`) output paths: `add` prints the indexed summary
/// and `list` prints one line per source.
#[test]
fn add_and_list_print_human_output_without_json() {
    let env = Env::new();
    let fixture = write_fixture(&env);
    let config = write_config(&env, true);
    let db = env.root.join("human.db");

    let (code, stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config, &db, false));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("indexed acme latest: 1 pages,"),
        "stdout: {stdout}"
    );

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "list",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            db.to_str().expect("db path"),
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("acme latest (llms_txt · approved_index · active)"),
        "stdout: {stdout}"
    );
}

/// A guard refusal from the replay surfaces as `FETCH_BLOCKED` through the
/// ingest error mapping.
#[test]
fn add_maps_a_guard_refusal_to_fetch_blocked() {
    let env = Env::new();
    let fixture = write_manifest(
        &env,
        &serde_json::json!([
            { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        ]),
    );
    // The recorded index body exceeds the 64-byte cap, so the replay
    // refuses with `BodyTooLarge` — a guard refusal.
    let config_path = env.root.join("tiny.toml");
    std::fs::write(
        &config_path,
        concat!(
            "[fetch]\n",
            "max_bytes = 64\n\n",
            "[sources.acme]\n",
            "kind = \"llms_txt\"\n",
            "url = \"https://docs.acme.test/llms.txt\"\n",
            "license = \"MIT\"\n"
        ),
    )
    .expect("config written");
    let db = env.root.join("guard.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config_path, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("FETCH_BLOCKED"), "stderr: {stderr}");
}

/// A recorded non-success status is a transport failure, not a refusal:
/// `UPSTREAM_ERROR`, matching the live path.
#[test]
fn add_maps_a_recorded_error_status_to_upstream_error() {
    let env = Env::new();
    let fixture = write_manifest(
        &env,
        &serde_json::json!([
            { "url": "https://docs.acme.test/llms.txt", "status": 503, "headers": {}, "body": INDEX },
        ]),
    );
    let config = write_config(&env, true);
    let db = env.root.join("status.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("UPSTREAM_ERROR"), "stderr: {stderr}");
}

/// An index without link lines fails `LLMS_TXT_EMPTY`.
#[test]
fn add_maps_llms_txt_empty() {
    let env = Env::new();
    let fixture = write_manifest(
        &env,
        &serde_json::json!([
            { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": "# Acme\n\nNo links here.\n" },
        ]),
    );
    let config = write_config(&env, true);
    let db = env.root.join("empty.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("LLMS_TXT_EMPTY"), "stderr: {stderr}");
}

/// A link that does not resolve fails with the parse error: an absolute
/// URL with an empty host cannot join.
#[test]
fn add_maps_an_unresolvable_link() {
    let env = Env::new();
    let fixture = write_manifest(
        &env,
        &serde_json::json!([
            { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": "# Acme\n\n- [Bad](http://:80/page.md)\n" },
        ]),
    );
    let config = write_config(&env, true);
    let db = env.root.join("link.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("does not resolve"), "stderr: {stderr}");
}

/// An index whose only page carries no markdown twin fails with no
/// ingestable pages: the HTML path falls back and skips.
#[test]
fn add_maps_no_ingestable_pages() {
    let env = Env::new();
    let fixture = write_manifest(
        &env,
        &serde_json::json!([
            { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": "# Acme\n\n- [Printable](https://docs.acme.test/intro.html)\n" },
            { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": {}, "body": "<!doctype html><html><body>printable</body></html>" },
        ]),
    );
    let config = write_config(&env, true);
    let db = env.root.join("nomd.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&fixture, &config, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("no ingestable pages"), "stderr: {stderr}");
}

/// A fixture directory without a manifest fails with the fixture error.
#[test]
fn add_maps_a_missing_manifest() {
    let env = Env::new();
    let dir = env.root.join("no-manifest");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let config = write_config(&env, true);
    let db = env.root.join("manifest.db");

    let (code, _stdout, stderr) = output(env.cmd(), &add_args(&dir, &config, &db, true));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("fixture"), "stderr: {stderr}");
}

/// A store path that is a directory fails through the store error mapping.
#[test]
fn list_fails_when_the_db_path_is_a_directory() {
    let env = Env::new();
    let config = write_config(&env, true);
    let db = env.root.join("as-dir.db");
    std::fs::create_dir_all(&db).expect("db dir");

    let (code, _stdout, stderr) = output(
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
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("UPSTREAM_ERROR"), "stderr: {stderr}");
}

/// A data directory the process cannot create fails before the store
/// opens.
#[cfg(unix)]
#[test]
fn list_fails_when_the_data_directory_cannot_be_created() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let config = write_config(&env, true);
    let sealed = env.root.join("sealed");
    std::fs::create_dir_all(&sealed).expect("sealed dir");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o500)).expect("mode set");

    // A root process ignores the permission bits, so the seal cannot hold
    // and the refusal never happens; skip loudly in that case.
    if std::fs::create_dir(sealed.join("probe")).is_ok() {
        std::fs::remove_dir(sealed.join("probe")).ok();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755))
            .expect("mode restored");
        eprintln!("skipping: the process ignores directory permission bits");
        return;
    }

    let (code, _stdout, stderr) = output(
        env.cmd(),
        &[
            "list",
            "--json",
            "--config",
            config.to_str().expect("config path"),
            "--db",
            sealed
                .join("nested")
                .join("docs.db")
                .to_str()
                .expect("db path"),
        ],
    );
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755))
        .expect("mode restored");
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("UPSTREAM_ERROR"), "stderr: {stderr}");
}

/// `db migrate` runs the startup migration and is idempotent.
#[test]
fn db_migrate_runs_and_is_idempotent() {
    let env = Env::new();
    let db = env.root.join("migrate.db");

    let (code, stdout, stderr) = output(
        env.cmd(),
        &["db", "migrate", "--db", db.to_str().expect("db path")],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let result: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(result["schema_version"], 2);

    let (code, stdout, stderr) = output(
        env.cmd(),
        &["db", "migrate", "--db", db.to_str().expect("db path")],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let result: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(result["schema_version"], 2);
}

/// `list` on an empty store prints the empty envelope under `--json`.
#[test]
fn list_on_an_empty_store_prints_the_empty_envelope() {
    let env = Env::new();
    let (code, stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let list: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(list["results"].as_array().expect("results").len(), 0);
    assert_eq!(list["truncated"], false);
    assert!(list["served_at"].is_string());
}

/// The `source` tests need an indexed corpus: the ingest bootstrap lands
/// at the publish floor (`approved_index`) and the operator walks the
/// progression from there (docs/implementation/07 §License gate).
fn source_env() -> (Env, PathBuf) {
    let (env, config) = common::indexed_env_at_index();
    (env, config)
}

/// `source approve` walks the full progression: each legal move exits 0
/// with the JSON transition report, and `source list` reports the final
/// state (AC3).
#[test]
fn source_approve_walks_the_full_progression() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    for stage in ["approved_embed", "approved_cache", "approved_snippet"] {
        let (code, stdout, stderr) = output(
            env.cmd(),
            &[
                "source",
                "approve",
                "acme",
                stage,
                "--operator",
                "test",
                "--json",
                cfg[0],
                cfg[1],
            ],
        );
        assert_eq!(code, 0, "approve {stage}: {stderr}");
        let report: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
        assert_eq!(report["library"], "acme");
        assert_eq!(report["to"], stage);
        assert_eq!(report["operator"], "test");
    }
    let (code, stdout, stderr) = output(env.cmd(), &["source", "list", "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let list: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(list["results"][0]["status"], "approved_snippet");
}

/// An out-of-order approve is a typed refusal: exit 1, the USAGE envelope
/// carries the illegal-transition detail, and the stored state stays
/// unchanged (AC3).
#[test]
fn an_out_of_order_approve_exits_one_without_a_state_change() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "acme",
            "approved_fetch",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout must stay clean: {stdout:?}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "USAGE");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("illegal license transition"),
        "stderr: {stderr}"
    );

    // The state never moved: the list still shows the index stage.
    let (code, stdout, stderr) = output(env.cmd(), &["source", "list", "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let list: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(list["results"][0]["status"], "approved_index");
}

/// An unknown stage name is a usage error, and so is approving a library
/// the store never indexed (AC8).
#[test]
fn unknown_stage_and_unknown_library_exit_one() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source", "approve", "acme", "bogus", "--json", cfg[0], cfg[1],
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown license stage"),
        "stderr: {stderr}"
    );

    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "ghost",
            "approved_index",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no indexed source"),
        "stderr: {stderr}"
    );
}

/// `source deny` moves the source onto `denied`; re-opening it as
/// `pending_review` is the one refusal exit and requires `--note`: exit 1
/// without, exit 0 with (AC8).
#[test]
fn denied_to_pending_review_needs_a_note() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "deny",
            "acme",
            "--note",
            "robots disallow",
            "--operator",
            "test",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "deny: {stderr}");
    let report: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(report["to"], "denied");
    assert_eq!(report["note"], "robots disallow");

    // Without the note the re-open is a typed usage refusal.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "acme",
            "pending_review",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("requires a non-empty note"),
        "stderr: {stderr}"
    );

    // With the note the re-open lands.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "acme",
            "pending_review",
            "--note",
            "re-review after robots fix",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(report["from"], "denied");
    assert_eq!(report["to"], "pending_review");
}

/// `source list --json` joins the declarative config (url, license SPDX
/// id, attribution, expires) with the authoritative store state (status,
/// version, `fetched_at`) (AC3).
#[test]
fn source_list_json_shows_state_spdx_version_and_fetched_at() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];
    let (code, stdout, stderr) = output(env.cmd(), &["source", "list", "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let list: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    let entry = &list["results"][0];
    assert_eq!(entry["name"], "acme");
    assert_eq!(entry["license"], "MIT");
    assert_eq!(entry["attribution"], "© Acme Corp documentation");
    assert_eq!(entry["url"], "https://docs.acme.test/llms.txt");
    assert_eq!(entry["status"], "approved_index");
    let versions = entry["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 2, "stdout: {stdout}");
    assert!(
        versions
            .iter()
            .all(|version| version["fetched_at"].is_string() && version["status"].is_string()),
        "stdout: {stdout}"
    );
}

/// `source approve --expires` stamps the expiry date on the moved rows,
/// and a date that does not parse is a usage error
/// (docs/implementation/07 §License gate).
#[test]
fn source_approve_expires_stamps_the_expiry() {
    let (env, config) = source_env();
    let cfg = ["--config", config.to_str().expect("config path")];

    // A malformed date refuses at the flag edge.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "acme",
            "approved_embed",
            "--expires",
            "not-a-date",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 1, "stdout: {stdout} stderr: {stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert!(
        error["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("YYYY-MM-DD"),
        "stderr: {stderr}"
    );

    // A legal move with a well-formed date stamps every row.
    let (code, stdout, stderr) = output(
        env.cmd(),
        &[
            "source",
            "approve",
            "acme",
            "approved_embed",
            "--expires",
            "2999-01-01",
            "--json",
            cfg[0],
            cfg[1],
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(report["to"], "approved_embed");
    assert_eq!(report["expires"], "2999-01-01");

    // The store carries the stamp on every version row.
    let (code, stdout, stderr) = output(env.cmd(), &["versions", "acme", "--json", cfg[0], cfg[1]]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("2999-01-01"), "stdout: {stdout}");
}
