//! Shared end-to-end CLI test helpers: the isolated per-test environment
//! and the output helper. Each integration test file compiles this module
//! separately (`mod common;`), so items stay public and helpers a file
//! does not use stay.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::must_use_candidate)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use devdocs_model::{ResultEnvelope, ResultItem};
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
pub struct Env {
    /// The temp root everything lives under.
    pub root: PathBuf,
    /// The code for the M1 `DEVDOCS_INJECT_ERROR` seam, when set.
    inject: Option<String>,
    /// Extra environment entries applied on top of the isolation.
    extra: Vec<(String, String)>,
    /// Drop `DEVDOCS_HOME` so the `XDG_DATA_HOME` root applies.
    homeless: bool,
}

impl Env {
    /// A fresh isolated environment.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("temp dir").keep(),
            inject: None,
            extra: Vec::new(),
            homeless: false,
        }
    }

    /// Apply the M1 error-injection seam to every run in this env.
    pub fn inject(&mut self, code: &str) {
        self.inject = Some(code.to_owned());
    }

    /// Set one extra environment variable for every run in this env.
    pub fn set(&mut self, var: &str, value: &str) {
        self.extra.push((var.to_owned(), value.to_owned()));
    }

    /// Drop `DEVDOCS_HOME` so the `XDG_DATA_HOME` data root applies.
    pub fn without_home(&mut self) {
        self.homeless = true;
    }

    /// The command with the isolated environment applied.
    pub fn cmd(&self) -> Command {
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
pub fn output(mut cmd: Command, args: &[&str]) -> (i32, String, String) {
    let out = cmd.args(args).output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

/// The index and page bodies the acme fixture replay serves.
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

/// The replay responses one acme version serves through `--fixture`.
fn acme_responses(intro: &str) -> Value {
    serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": intro },
    ])
}

/// Write one replay manifest: the responses the `--fixture` fetch serves.
fn write_manifest(dir: &Path, responses: &Value) {
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string(responses).expect("manifest serializes"),
    )
    .expect("manifest written");
}

/// Write the devdocs.toml pointing at the acme `llms.txt` source; return
/// its path.
fn write_config(root: &Path) -> PathBuf {
    let config = root.join("devdocs.toml");
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
    config
}

/// Run one versioned `add --fixture` against the replay manifest.
fn add_fixture_version(env: &Env, dir: &Path, config: &Path, version: &str, responses: &Value) {
    write_manifest(dir, responses);
    let (code, _stdout, stderr) = output(
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
}

/// An indexed fixture corpus: acme at versions 1.0 and 2.0. Returns the
/// environment and the config path every CLI run needs.
pub fn indexed_env() -> (Env, PathBuf) {
    let env = Env::new();
    let dir = env.root.join("fixture");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let config = write_config(&env.root);
    add_fixture_version(&env, &dir, &config, "1.0", &acme_responses(INTRO_V1));
    add_fixture_version(&env, &dir, &config, "2.0", &acme_responses(INTRO_V2));
    (env, config)
}

/// The intro page carrying a fenced JavaScript definition the symbol pass
/// extracts (docs/implementation/13 §M5): acme 1.0 defines `makeWidget`.
const INTRO_SYMBOL: &str = concat!(
    "# Intro\n\n",
    "The Acme SDK ships a client for every runtime.\n\n",
    "## Widgets\n\n",
    "Create a widget with the factory.\n\n",
    "```js\n",
    "export function makeWidget(name) {\n",
    "  return { name };\n",
    "}\n",
    "```\n"
);

/// An indexed fixture corpus whose page carries an extractable symbol:
/// acme at 1.0 defines `makeWidget`. Returns the environment and the
/// config path every CLI run needs.
pub fn symbol_env() -> (Env, PathBuf) {
    let env = Env::new();
    let dir = env.root.join("fixture");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let config = write_config(&env.root);
    add_fixture_version(&env, &dir, &config, "1.0", &acme_responses(INTRO_SYMBOL));
    (env, config)
}

/// A second symbol page: `makeWidget` declared again, in its own chunk.
const WIDGETS_SYMBOL: &str = concat!(
    "# Widgets\n\n",
    "The factory grows a second form.\n\n",
    "## Overloads\n\n",
    "```js\n",
    "export function makeWidget(name, zone) {\n",
    "  return { name, zone };\n",
    "}\n",
    "```\n"
);

/// The index listing both symbol pages.
const SYMBOL_INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.md): Start here\n",
    "- [Widgets](https://docs.acme.test/widgets.md): The widget factory\n"
);

/// The replay responses the two-symbol corpus serves.
fn symbol_responses() -> Value {
    serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": SYMBOL_INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO_SYMBOL },
        { "url": "https://docs.acme.test/widgets.md", "status": 200, "headers": {}, "body": WIDGETS_SYMBOL },
    ])
}

/// An indexed fixture corpus where the symbol query `makeWidget` matches
/// two chunks (the declaration appears on both pages): the shape the
/// cursor-resume tests need. Returns the environment and the config path.
pub fn two_symbol_env() -> (Env, PathBuf) {
    let env = Env::new();
    let dir = env.root.join("fixture");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let config = write_config(&env.root);
    add_fixture_version(&env, &dir, &config, "1.0", &symbol_responses());
    (env, config)
}

/// Search the indexed corpus and return the first result: the doc the
/// open/verify surfaces exercise.
pub fn indexed_doc(env: &Env, config: &Path) -> ResultItem {
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
    let envelope: ResultEnvelope = serde_json::from_str(stdout.trim()).expect("envelope");
    envelope.results.into_iter().next().expect("one result")
}
