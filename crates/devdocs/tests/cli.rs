//! End-to-end CLI tests through the real binary: exit codes 0-4, the
//! usage remap, stdout purity, and the config subcommands
//! (docs/implementation/04 §CLI end-to-end).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

/// Every `DEVDOCS_*` layer variable the host may carry; the tests remove
/// them so only the layers under test apply. `DEVDOCS_HOME` is absent: the
/// env always sets it to the temp data root itself.
const LAYER_VARS: [&str; 10] = [
    "DEVDOCS_FETCH_CONCURRENCY",
    "DEVDOCS_FETCH_TIMEOUT_MS",
    "DEVDOCS_FETCH_MAX_BYTES",
    "DEVDOCS_FETCH_MAX_REDIRECTS",
    "DEVDOCS_FETCH_USER_AGENT",
    "DEVDOCS_INDEX_PATH",
    "DEVDOCS_INDEX_EMBEDDINGS",
    "DEVDOCS_SEARCH_MAX_TOKENS_DEFAULT",
    "DEVDOCS_SEARCH_LIMIT_DEFAULT",
    "DEVDOCS_SEARCH_RRF_K",
];

/// The isolated per-test environment: a temp cwd, data root, and user
/// config dir. Build a fresh [`Command`] per run with [`Env::cmd`].
struct Env {
    /// The temp root everything lives under.
    root: PathBuf,
    /// The code for the M1 `DEVDOCS_INJECT_ERROR` seam, when set.
    inject: Option<String>,
    /// Extra environment entries applied on top of the isolation.
    extra: Vec<(String, String)>,
    /// Drop `DEVDOCS_HOME` so the `XDG_DATA_HOME` root applies.
    homeless: bool,
}

impl Env {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("temp dir").keep(),
            inject: None,
            extra: Vec::new(),
            homeless: false,
        }
    }

    /// Apply the M1 error-injection seam to every run in this env.
    fn inject(&mut self, code: &str) {
        self.inject = Some(code.to_owned());
    }

    /// Set one extra environment variable for every run in this env.
    fn set(&mut self, var: &str, value: &str) {
        self.extra.push((var.to_owned(), value.to_owned()));
    }

    /// Drop `DEVDOCS_HOME` so the `XDG_DATA_HOME` data root applies.
    fn without_home(&mut self) {
        self.homeless = true;
    }

    /// The command with the isolated environment applied.
    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("devdocs").expect("devdocs binary");
        cmd.env("XDG_CONFIG_HOME", self.root.join("config"));
        if self.homeless {
            cmd.env_remove("DEVDOCS_HOME")
                .env("XDG_DATA_HOME", self.root.join("data"));
        } else {
            cmd.env("DEVDOCS_HOME", self.root.join("data"));
        }
        for var in LAYER_VARS {
            cmd.env_remove(var);
        }
        match &self.inject {
            Some(code) => {
                cmd.env("DEVDOCS_INJECT_ERROR", code);
            }
            None => {
                cmd.env_remove("DEVDOCS_INJECT_ERROR");
            }
        }
        for (var, value) in &self.extra {
            cmd.env(var, value);
        }
        cmd.current_dir(&self.root);
        cmd
    }
}

/// Run and return (exit code, stdout, stderr).
fn output(mut cmd: Command, args: &[&str]) -> (i32, String, String) {
    let out = cmd.args(args).output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

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

/// Exit 2 for a valid query that serves nothing.
#[test]
fn exit_two_on_no_result() {
    let mut env = Env::new();
    env.inject("NO_RESULT");
    let (code, stdout, stderr) = output(env.cmd(), &["list", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(error["error"], "NO_RESULT");
    assert_eq!(error["miss_reason"], "not_indexed");
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
        ".index.path" => "[DB_PATH]",
    });
}

/// The index and page bodies the fixture replay serves.
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.md): Start here\n",
    "- [Printable](https://docs.acme.test/intro.html): The HTML twin\n"
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
            "license = \"MIT\"\n",
            "status = \"approved_index\"\n"
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
    assert_eq!(added["pages"], 1, "the HTML twin is skipped: {added}");
    assert!(added["chunks"].as_u64().expect("chunks") > 0);

    // `devdocs list --json` names the source and version.
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

    // Store-level assertion: chunks, hashes, breadcrumbs.
    let chunks = devdocs_testkit::read_chunks(&db).expect("chunks read");
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
/// llms.txt pipeline must not run for a registered URL with `--kind git`.
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
    assert!(stderr.contains("not supported yet"), "stderr: {stderr}");
    assert!(!db.exists(), "a rejected add must not create the database");
}

/// A terminal registry status withdraws the previously published
/// generation before the refusal returns: the revoked source deactivates
/// and leaves retrieval, while a refusal without a terminal status still
/// writes nothing (docs/09 §License gate enforcement points, point 3).
#[test]
fn add_refuses_a_revoked_source_and_withdraws_it() {
    let env = Env::new();
    let fixture = write_fixture(&env);
    let config = write_config(&env, true);
    let db = env.root.join("withdraw.db");

    // First run publishes the source.
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
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");

    // The registry status flips to revoked; the second run refuses.
    std::fs::write(
        &config,
        concat!(
            "[sources.acme]\n",
            "kind = \"llms_txt\"\n",
            "url = \"https://docs.acme.test/llms.txt\"\n",
            "license = \"MIT\"\n",
            "status = \"revoked\"\n"
        ),
    )
    .expect("config rewritten");
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

    // The withdrawal landed: the source is inactive with the recorded
    // status, so retrieval no longer serves it.
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
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    let source = &envelope["results"][0];
    assert_eq!(source["license_status"], "revoked");
    assert_eq!(source["active"], false);
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
    assert_eq!(result["schema_version"], 1);

    let (code, stdout, stderr) = output(
        env.cmd(),
        &["db", "migrate", "--db", db.to_str().expect("db path")],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let result: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(result["schema_version"], 1);
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
