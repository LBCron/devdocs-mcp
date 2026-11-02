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
        // The path is machine-specific and the user agent carries the crate
        // version, which release-plz bumps under this snapshot; both are
        // normalized so a version bump never red-eyes this test again.
        ".index.path" => "[DB_PATH]",
        ".fetch.user_agent" => "[USER_AGENT]",
    });
}
