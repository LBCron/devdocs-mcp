//! The CLI-subprocess pass (docs/implementation/14 §Harness flow): the
//! harness times `devdocs search` end-to-end. The p95 gate is judged on
//! these wall times, cold start excluded after one warmup call.
//!
//! # Security
//!
//! No shell is ever involved: every invocation is one
//! [`std::process::Command`] whose program and arguments travel as a
//! process argument vector (`shell=False` equivalent). No string is ever
//! concatenated into a command line, so no command-injection surface
//! exists; a question text with `; rm -rf /` is passed as one argument.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::EvalError;
use crate::suite::Question;

/// One timed subprocess call.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SubprocessSample {
    /// Wall time in milliseconds.
    pub wall_ms: f64,
    /// The process exit code; -1 when the process died to a signal.
    pub exit_code: i32,
}

/// Locates the `devdocs` binary: the explicit argument, then
/// `DEVDOCS_EVAL_DEVDOCS_BIN`, then a `target/{debug,release}/devdocs`
/// walk up from the working directory (honoring `CARGO_TARGET_DIR` and the
/// platform exe suffix), then `PATH`.
///
/// # Errors
/// [`EvalError::Subprocess`] when nothing resolves to an executable file.
pub fn locate_devdocs(explicit: Option<&Path>) -> Result<PathBuf, EvalError> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(EvalError::Subprocess(format!(
                "--devdocs-bin {} is not a file",
                path.display()
            )));
        }
        return Ok(path.to_path_buf());
    }
    let exe = format!("devdocs{}", std::env::consts::EXE_SUFFIX);
    from_env()
        .or_else(|| from_target_dir(&exe))
        .or_else(|| from_path(&exe))
        .ok_or_else(missing_binary_error)
}

/// The resolver-exhausted error: it names every workaround the caller has.
fn missing_binary_error() -> EvalError {
    EvalError::Subprocess(
        "cannot find the devdocs binary; pass --devdocs-bin, set \
         DEVDOCS_EVAL_DEVDOCS_BIN, or run `cargo build -p devdocs` first"
            .to_owned(),
    )
}

/// The `DEVDOCS_EVAL_DEVDOCS_BIN` override.
fn from_env() -> Option<PathBuf> {
    env_binary(std::env::var("DEVDOCS_EVAL_DEVDOCS_BIN").ok().as_deref())
}

/// [`from_env`] on an injected value, so the tests never touch the process
/// environment.
fn env_binary(value: Option<&str>) -> Option<PathBuf> {
    let from_env = value?;
    if from_env.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(from_env);
    candidate.is_file().then_some(candidate)
}

/// The workspace target directory: `CARGO_TARGET_DIR` when set, else a
/// `target/{debug,release}` walk up from the working directory.
fn from_target_dir(exe: &str) -> Option<PathBuf> {
    let target_dir = std::env::var("CARGO_TARGET_DIR").ok();
    let start = std::env::current_dir().ok()?;
    from_target_dir_in(target_dir.as_deref(), &start, exe)
}

/// [`from_target_dir`] on injected values, so the tests never touch the
/// process environment or working directory.
fn from_target_dir_in(target_dir: Option<&str>, start: &Path, exe: &str) -> Option<PathBuf> {
    let profiles = ["debug", "release"];
    if let Some(target_dir) = target_dir
        && !target_dir.is_empty()
    {
        for profile in profiles {
            let candidate = Path::new(target_dir).join(profile).join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let mut dir = start.to_path_buf();
    loop {
        for profile in profiles {
            let candidate = dir.join("target").join(profile).join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// A `devdocs` on `PATH`.
fn from_path(exe: &str) -> Option<PathBuf> {
    path_binary(&std::env::var_os("PATH")?, exe)
}

/// [`from_path`] on an injected `PATH`, so the tests never touch the
/// process environment.
fn path_binary(path_var: &std::ffi::OsStr, exe: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
}

/// The wall-time deadline one `devdocs search` call gets before the run
/// treats it as stalled.
const CALL_DEADLINE: Duration = Duration::from_secs(30);

/// The `try_wait` poll interval while a call runs.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Times one `devdocs search` invocation end-to-end: process start, flag
/// parse, and dispatch (docs/implementation/08 §Latency).
///
/// The program and every argument travel as an argument vector — no shell,
/// no interpolation (see the module security note). The call gets a fixed
/// deadline: a stalled child is killed and reported as an error instead of
/// blocking the run forever. The output is not captured — the harness
/// reads only the timing and the exit code.
///
/// # Errors
/// [`EvalError::Subprocess`] when the process cannot be spawned, cannot be
/// waited on, or overshoots the deadline.
pub fn run_once(bin: &Path, db: &Path, question: &Question) -> Result<SubprocessSample, EvalError> {
    run_once_deadline(bin, db, question, CALL_DEADLINE)
}

/// [`run_once`] with an injectable deadline; the tests shrink it so the
/// kill path runs in milliseconds instead of the full 30 seconds.
fn run_once_deadline(
    bin: &Path,
    db: &Path,
    question: &Question,
    deadline: Duration,
) -> Result<SubprocessSample, EvalError> {
    let start = Instant::now();
    let mut child = Command::new(bin)
        .arg("--db")
        .arg(db)
        .arg("--json")
        .arg("search")
        .arg(&question.question)
        .arg("--library")
        .arg(&question.library)
        .arg("--version")
        .arg(&question.version)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| EvalError::Subprocess(format!("cannot spawn {}: {e}", bin.display())))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
                return Ok(SubprocessSample {
                    wall_ms,
                    exit_code: status.code().unwrap_or(-1),
                });
            }
            Ok(None) if start.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(EvalError::Subprocess(format!(
                    "devdocs search exceeded the {}s deadline; the process was killed",
                    deadline.as_secs()
                )));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(source) => {
                return Err(EvalError::Subprocess(format!(
                    "cannot wait for {}: {source}",
                    bin.display()
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;
    use std::time::Duration;

    use super::{Question, locate_devdocs, run_once_deadline};
    use crate::suite::{Grader, TaskType};

    /// A one-question stand-in for the suite loader.
    fn question() -> Question {
        Question {
            id: "q-001".to_owned(),
            library: "test-lib".to_owned(),
            version: "1.0.0".to_owned(),
            task_type: TaskType::ExactApi,
            question: "middleware".to_owned(),
            expect_refs: Some(vec!["docs://test-lib/v1.0.0/proxy".to_owned()]),
            expect_symbols: None,
            grader: Grader::RefMatch,
            notes: "n".to_owned(),
        }
    }

    /// A stand-in binary: a script whose behavior the test controls.
    #[cfg(unix)]
    fn stub(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(name);
        std::fs::write(&bin, body).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn an_explicit_binary_is_used_as_given() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let bin = stub(dir.path(), "devdocs", "#!/bin/sh\nexit 0\n");
        #[cfg(not(unix))]
        let bin = {
            let bin = dir.path().join("devdocs.exe");
            std::fs::write(&bin, b"stub").unwrap();
            bin
        };
        assert_eq!(locate_devdocs(Some(&bin)).unwrap(), bin);

        let missing = dir.path().join("absent");
        let err = locate_devdocs(Some(&missing)).unwrap_err();
        assert!(err.to_string().contains("is not a file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_env_override_resolves_a_real_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devdocs", "#!/bin/sh\nexit 0\n");

        assert_eq!(super::env_binary(Some(bin.to_str().unwrap())).unwrap(), bin);
        // An empty or nonexistent override resolves to nothing.
        assert_eq!(super::env_binary(Some("")), None);
        assert_eq!(
            super::env_binary(Some(dir.path().join("absent").to_str().unwrap())),
            None
        );
        assert_eq!(super::env_binary(None), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_target_dir_resolver_checks_the_override_then_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let exe = format!("devdocs{}", std::env::consts::EXE_SUFFIX);

        // The explicit target dir wins when it holds a build, debug first.
        let override_dir = tempfile::tempdir().unwrap();
        let debug_dir = override_dir.path().join("debug");
        std::fs::create_dir_all(&debug_dir).unwrap();
        let debug = stub(&debug_dir, &exe, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            super::from_target_dir_in(
                Some(override_dir.path().to_str().unwrap()),
                dir.path(),
                &exe
            )
            .unwrap(),
            debug
        );

        // A release-only override is found on the second profile.
        let release_dir_dir = tempfile::tempdir().unwrap();
        let release_dir = release_dir_dir.path().join("release");
        std::fs::create_dir_all(&release_dir).unwrap();
        let release = stub(&release_dir, &exe, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            super::from_target_dir_in(
                Some(release_dir_dir.path().to_str().unwrap()),
                dir.path(),
                &exe
            )
            .unwrap(),
            release
        );

        // Without a build in the override it falls back to the walk-up
        // from the start directory, which finds the workspace build.
        let workspace = tempfile::tempdir().unwrap();
        let workspace_debug = workspace.path().join("target").join("debug");
        std::fs::create_dir_all(&workspace_debug).unwrap();
        let found = stub(&workspace_debug, &exe, "#!/bin/sh\nexit 0\n");
        let nested = workspace.path().join("crates").join("eval");
        assert_eq!(
            super::from_target_dir_in(None, &nested, &exe).unwrap(),
            found
        );

        // Without a build anywhere on the walk-up, nothing resolves.
        let bare = tempfile::tempdir().unwrap();
        let empty_override = tempfile::tempdir().unwrap();
        assert_eq!(
            super::from_target_dir_in(
                Some(empty_override.path().to_str().unwrap()),
                bare.path(),
                &exe
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_path_resolver_finds_a_binary_on_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let exe = format!("devdocs{}", std::env::consts::EXE_SUFFIX);
        let bin = stub(dir.path(), &exe, "#!/bin/sh\nexit 0\n");

        assert_eq!(
            super::path_binary(std::ffi::OsStr::new(dir.path().to_str().unwrap()), &exe).unwrap(),
            bin
        );
        // A PATH entry without the binary resolves to nothing.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            super::path_binary(std::ffi::OsStr::new(empty.path().to_str().unwrap()), &exe),
            None
        );
    }

    #[test]
    fn the_missing_binary_error_names_the_workarounds() {
        let message = super::missing_binary_error().to_string();
        assert!(
            message.contains("cannot find the devdocs binary"),
            "{message}"
        );
        assert!(message.contains("--devdocs-bin"), "{message}");
        assert!(message.contains("DEVDOCS_EVAL_DEVDOCS_BIN"), "{message}");
    }

    #[test]
    fn a_spawn_failure_is_a_subprocess_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("devdocs");
        std::fs::write(&bin, "not executable").unwrap();
        let err = run_once_deadline(
            &bin,
            &dir.path().join("fixture.db"),
            &question(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot spawn"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_stalled_call_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "devdocs", "#!/bin/sh\nexec sleep 30\n");
        let db = dir.path().join("fixture.db");
        let err =
            run_once_deadline(&bin, &db, &question(), Duration::from_millis(100)).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("deadline"), "{message}");
        assert!(message.contains("killed"), "{message}");
    }
}
