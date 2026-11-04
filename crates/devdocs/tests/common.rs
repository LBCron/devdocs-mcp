//! Shared end-to-end CLI test helpers: the isolated per-test environment
//! and the output helper. Each integration test file compiles this module
//! separately (`mod common;`), so items stay public and helpers a file
//! does not use stay.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::must_use_candidate)]
#![allow(dead_code)]

use std::path::PathBuf;

use assert_cmd::Command;

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
