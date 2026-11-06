//! The git command builder and runner: every invocation is one argv
//! vector against the system binary with a hardened child env
//! (docs/implementation/09 §Git subprocess safety).

use std::io;
use std::path::Path;
use std::process::Command;

use crate::error::IngestError;

/// The `GIT_*` environment variables every invocation strips, so an
/// ambient value cannot redirect the child to another repository or
/// object store, inject config, or hijack prompting and SSH
/// (docs/implementation/09 §Git subprocess safety). Removing
/// `GIT_CONFIG_COUNT` disables the whole ambient
/// `GIT_CONFIG_KEY_i`/`GIT_CONFIG_VALUE_i` config family.
const STRIPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_COUNT",
    "GIT_ASKPASS",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_STDERR",
];

/// Per-command git config, applied to every invocation: checkout hooks
/// and LFS filters cannot run, and the local `file://` transport stays
/// enabled against a host config that disables it.
const BASE_CONFIG: &[&str] = &[
    "core.hooksPath=/dev/null",
    "filter.lfs.process=",
    "filter.lfs.smudge=",
    "filter.lfs.clean=",
    "protocol.file.allow=user",
];

/// Builds the command one `git` invocation runs: `git`, the per-command
/// `-c` config (base plus extras), then `args`, from `cwd`. The child env
/// is hardened here: the prompt and system config pins, then the removal
/// of every [`STRIPPED_ENV`] variable so an ambient value cannot reach
/// the git child (docs/implementation/09 §Git subprocess safety).
/// Extracted so tests assert on the exact command `run_git` executes,
/// not on a seam beside it.
fn build_git_command(cwd: &Path, args: &[&str], extra_config: &[&str]) -> Command {
    let mut command = Command::new("git");
    for config in BASE_CONFIG
        .iter()
        .copied()
        .chain(extra_config.iter().copied())
    {
        command.arg("-c").arg(config);
    }
    command.args(args).current_dir(cwd);
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    for key in STRIPPED_ENV {
        command.env_remove(key);
    }
    command
}

/// Runs one `git` invocation from `cwd` and returns trimmed stdout.
///
/// The command is an argv vector against the system binary — no shell, no
/// interpolated command line — built by [`build_git_command`]
/// (docs/implementation/09 §Git subprocess safety).
pub(crate) fn run_git(
    cwd: &Path,
    args: &[&str],
    extra_config: &[&str],
) -> Result<String, IngestError> {
    let output = build_git_command(cwd, args, extra_config)
        .output()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => IngestError::GitBinaryMissing,
            _ => IngestError::GitSpawn {
                message: error.to_string(),
            },
        })?;
    if !output.status.success() {
        return Err(IngestError::GitCommand {
            command: format!("git {}", args.join(" ")),
            stderr: stderr_tail(&output.stderr),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The bounded stderr tail for an error: the last 512 characters, cut on
/// a char boundary. No full dump.
fn stderr_tail(stderr: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(stderr);
    let text = lossy.trim();
    let mut start = text.len().saturating_sub(512);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use super::stderr_tail;

    /// A stderr tail cut mid-character advances to the next char boundary:
    /// a three-byte payload whose 512-byte back-cut lands inside a
    /// character yields a whole-character tail, never a panic.
    #[test]
    fn the_stderr_tail_cuts_on_a_char_boundary() {
        // 1800 bytes; the cut lands at byte 1288, the second byte of
        // character 429 — the loop advances to the boundary at 1291.
        let long = "€".repeat(600);
        let tail = stderr_tail(long.as_bytes());
        assert!(std::str::from_utf8(tail.as_bytes()).is_ok());
        assert!(tail.chars().all(|c| c == '€'));
        assert_eq!(tail.chars().count(), 170);

        // Below the cap the whole text comes back.
        assert_eq!(stderr_tail(b"short".as_slice()), "short");
    }

    /// The builder `run_git` executes hardens the child env: every
    /// ambient `GIT_*` control and location variable is removed, the
    /// prompt and system config pins are set — what an operator shell may
    /// carry into the process never reaches the git child
    /// (docs/implementation/09 §Git subprocess safety).
    #[test]
    fn the_git_builder_strips_the_ambient_git_env_from_the_child() {
        let command = super::build_git_command(Path::new("."), &["status"], &[]);

        let envs: std::collections::HashMap<_, _> = command.get_envs().collect();
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_ASKPASS",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_STDERR",
        ] {
            assert_eq!(
                envs.get(std::ffi::OsStr::new(key)),
                Some(&None),
                "{key} must be removed from the child env"
            );
        }
        for key in ["GIT_TERMINAL_PROMPT", "GIT_CONFIG_NOSYSTEM"] {
            assert!(
                envs.get(std::ffi::OsStr::new(key))
                    .is_some_and(Option::is_some),
                "{key} must be pinned on the child env"
            );
        }
    }
}
