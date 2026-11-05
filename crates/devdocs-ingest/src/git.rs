//! The git source adapter: pinned clones, a docs walk, and temp-dir
//! cleanup (docs/implementation/07 §Git adapter).
//!
//! Every `git` invocation is an argv vector against the system binary —
//! no shell, no interpolated command line — with the subprocess-safety
//! hardening of docs/implementation/09 §Git subprocess safety: the
//! ambient `GIT_*` location and control variables stripped, prompting and
//! the system config disabled, and hooks and LFS filters disabled per
//! command. The
//! clone lands in a mode-0700 directory under `<data_root>/tmp` that a
//! `Drop` guard removes, and a SIGINT/SIGTERM handler (installed once per
//! process, idempotent) removes the registered path before exiting 130.
//!
//! The adapter is synchronous; the ingest pipeline wraps it in
//! `spawn_blocking` (docs/implementation/07 §Git adapter).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once};

use crate::chrome::PageDocument;
use crate::error::IngestError;
use crate::pipeline::TOTAL_INGEST_CAP;

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

/// The process-global one-slot registry the SIGINT/SIGTERM handler reads.
/// A second live clone replaces the registration; each guard still
/// removes its own path on drop. The single slot assumes one git ingest
/// per process (true today); it must become a set when concurrent git
/// runs land.
static REGISTERED_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Guards the one-time `ctrlc` handler installation.
static SIGNAL_HANDLER: Once = Once::new();

/// The exit code the signal handler exits with (128 + SIGINT).
const SIGNAL_EXIT_CODE: i32 = 130;

/// Installs the SIGINT/SIGTERM cleanup handler, once per process. The
/// handler removes the registered clone path, then exits 130. Later calls
/// are no-ops; a failed installation only logs — the `Drop` guard still
/// covers the normal path.
pub fn install_signal_cleanup() {
    SIGNAL_HANDLER.call_once(|| {
        let result = ctrlc::set_handler(|| {
            let registered = REGISTERED_PATH.lock().ok().and_then(|mut slot| slot.take());
            if let Some(path) = registered {
                let _ = std::fs::remove_dir_all(path);
            }
            std::process::exit(SIGNAL_EXIT_CODE);
        });
        if let Err(error) = result {
            tracing::warn!("could not install the git temp-dir signal handler: {error}");
        }
    });
}

/// Removes every `devdocs-git-*` entry under `tmp` whose mtime is older
/// than the ingest total cap, except the currently registered path (one
/// slot; the clone of the slot is taken first). The registry is
/// per-process, so the age bound is what keeps the sweep safe across
/// concurrent `add` runs sharing a data root: a live clone is never older
/// than the cap (the cap bounds the whole fetch), while a dir leaked by a
/// crashed or cap-tripped process is swept by a later run. The sweep is
/// best effort — every error is ignored, and entries whose mtime cannot
/// be read are skipped — and covers clone dirs left behind when the
/// total-cap timeout drops the `spawn_blocking` future and the process
/// exits before the guard's drop runs (docs/implementation/09 §Local
/// storage controls). It runs at the next `TempClone::create`, i.e. the
/// next `add` run of this short-lived CLI process.
fn sweep_stale_clones(tmp: &Path) {
    let registered = REGISTERED_PATH.lock().ok().and_then(|slot| slot.clone());
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_clone_entry = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("devdocs-git-"));
        if !is_clone_entry || Some(path.as_path()) == registered.as_deref() {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .is_some_and(|age| age >= TOTAL_INGEST_CAP);
        if !stale {
            continue;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// What kind of pin a plan resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    /// The pin names a tag; the clone checks the tag out shallowly.
    Tag,
    /// The pin is a 40-hex commit SHA; the clone fetches the SHA and
    /// detaches onto it.
    Commit,
}

/// A private, random-named clone directory under `<data_root>/tmp` that
/// removes itself on drop. The path is registered in the process-global
/// registry so the SIGINT/SIGTERM handler can remove it too
/// (docs/implementation/09 §Local storage controls).
#[derive(Debug)]
pub struct TempClone {
    path: PathBuf,
}

impl TempClone {
    /// Creates `<data_root>/tmp` with mode 0700, then a random-named
    /// subdirectory inside it, and verifies both modes on Unix. A
    /// best-effort sweep first removes stale clone dirs left by an
    /// aborted run.
    fn create(data_root: &Path) -> Result<Self, IngestError> {
        let tmp = data_root.join("tmp");
        create_private_dir(&tmp)?;
        sweep_stale_clones(&tmp);
        let handle = tempfile::Builder::new()
            .prefix("devdocs-git-")
            .tempdir_in(&tmp)
            .map_err(|e| IngestError::GitTempDir {
                message: format!(
                    "cannot create a clone directory under {}: {e}",
                    tmp.display()
                ),
            })?;
        // `keep` hands the removal duty to this guard.
        let path = handle.keep();
        // tempfile creates its directory 0755; the contract here is 0700.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
                |e| IngestError::GitTempDir {
                    message: format!("cannot restrict {}: {e}", path.display()),
                },
            )?;
        }
        verify_private_dir(&tmp)?;
        verify_private_dir(&path)?;
        if let Ok(mut slot) = REGISTERED_PATH.lock() {
            *slot = Some(path.clone());
        }
        install_signal_cleanup();
        Ok(Self { path })
    }

    /// The clone directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempClone {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                "cannot remove the git clone dir {}: {error}",
                self.path.display()
            );
        }
        if let Ok(mut slot) = REGISTERED_PATH.lock()
            && slot.as_deref() == Some(self.path.as_path())
        {
            *slot = None;
        }
    }
}

/// One planned git source: the pinned URL (the single source of truth for
/// the run), the resolved pin, and the private clone directory. Dropping
/// the plan removes the clone directory.
#[derive(Debug)]
pub struct GitPlan {
    url: String,
    pin: String,
    kind: PinKind,
    dir: TempClone,
}

impl GitPlan {
    /// The pinned URL the fetch must echo back.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The resolved pin: a tag name as given, or a commit SHA normalized
    /// to lowercase.
    #[must_use]
    pub fn pin(&self) -> &str {
        &self.pin
    }

    /// Whether the pin names a tag or a commit.
    #[must_use]
    pub fn kind(&self) -> PinKind {
        self.kind
    }

    /// The private clone directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Clones into the plan's directory and walks the docs it finds.
    ///
    /// The fetch refuses a `url` that differs from the planned URL: the
    /// URL cannot change between plan and fetch
    /// (docs/implementation/09 §Git subprocess safety).
    ///
    /// # Errors
    /// [`IngestError::GitUrlMismatch`] when the URL changed, and the
    /// git/adapter errors otherwise. The clone directory is removed on
    /// every error return through the plan's guard.
    pub fn fetch(&self, url: &str, extensions: &[String]) -> Result<GitFetch, IngestError> {
        self.fetch_with(url, extensions, &[])
    }

    /// [`GitPlan::fetch`] with extra per-command `-c key=value` config.
    /// The seam exists for tests that must pin a git protocol version
    /// (for example `protocol.version=0` to exercise the commit-pin
    /// fallback); production callers go through `fetch`.
    pub(crate) fn fetch_with(
        &self,
        url: &str,
        extensions: &[String],
        extra_config: &[&str],
    ) -> Result<GitFetch, IngestError> {
        if url != self.url {
            return Err(IngestError::GitUrlMismatch {
                planned: self.url.clone(),
                actual: url.to_owned(),
            });
        }
        let dir = self.dir.path();
        // Clones run from the parent (the tmp dir), because `Command`
        // fails to spawn with a `current_dir` that does not exist yet.
        let tmp = dir.parent().unwrap_or(dir);
        let dir_arg = dir.to_string_lossy().into_owned();

        let fallback_used = match self.kind {
            PinKind::Tag => {
                run_git(
                    tmp,
                    &[
                        "clone",
                        "--depth",
                        "1",
                        "--branch",
                        &self.pin,
                        "--single-branch",
                        "--filter=blob:none",
                        url,
                        &dir_arg,
                    ],
                    extra_config,
                )?;
                false
            }
            PinKind::Commit => {
                run_git(
                    tmp,
                    &["clone", "--depth", "1", "--filter=blob:none", url, &dir_arg],
                    extra_config,
                )?;
                let direct = run_git(
                    dir,
                    &["fetch", "--depth", "1", "origin", &self.pin],
                    extra_config,
                );
                if let Err(direct_error) = direct {
                    // The direct error is the likely root cause when the
                    // fallback also fails, so it surfaces here, not only
                    // in the discarded result.
                    tracing::warn!(
                        "the SHA fetch of {} failed; falling back to a full clone: {direct_error}",
                        self.pin
                    );
                    // The server rejected the SHA fetch (no
                    // allowReachableSHA1InWant): fall back to a full clone
                    // and report it (docs/implementation/07 §Git adapter).
                    std::fs::remove_dir_all(dir).map_err(|e| IngestError::GitTempDir {
                        message: format!("cannot clear {}: {e}", dir.display()),
                    })?;
                    create_private_dir(dir)?;
                    run_git(tmp, &["clone", url, &dir_arg], extra_config)?;
                    true
                } else {
                    false
                }
            }
        };

        if self.kind == PinKind::Commit {
            run_git(dir, &["checkout", "--detach", &self.pin], extra_config)?;
        }
        let commit_sha = run_git(dir, &["rev-parse", "HEAD"], extra_config)?;
        if !is_commit_sha(&commit_sha) {
            return Err(IngestError::GitCommitShaInvalid(commit_sha));
        }
        let (docs, skipped_non_utf8) = walk_docs(dir, extensions)?;
        Ok(GitFetch {
            commit_sha,
            fallback_used,
            docs,
            skipped_non_utf8,
        })
    }
}

/// What one git fetch produced.
#[derive(Debug, Clone)]
pub struct GitFetch {
    /// The commit the clone checked out (`git rev-parse HEAD`).
    pub commit_sha: String,
    /// Whether the commit pin needed the full-clone fallback.
    pub fallback_used: bool,
    /// The docs the walk found, relative to the clone root.
    pub docs: Vec<PageDocument>,
    /// Files the walk skipped because they are not valid UTF-8.
    pub skipped_non_utf8: usize,
}

/// Plans one git source: validates the pin, classifies it
/// (40-hex commit vs. tag), and creates the private clone directory under
/// `<data_root>/tmp`.
///
/// A pin starting with `-` is a usage error — it would read as a git
/// option on the argv vector (docs/implementation/09 §Git subprocess
/// safety).
///
/// # Errors
/// [`IngestError::GitInvalidPin`] for an option-shaped pin,
/// [`IngestError::GitTempDir`] when the private directory cannot be
/// created or verified.
pub fn plan(url: &str, pin: &str, data_root: &Path) -> Result<GitPlan, IngestError> {
    if pin.starts_with('-') {
        return Err(IngestError::GitInvalidPin {
            pin: pin.to_owned(),
        });
    }
    let kind = if is_commit_sha(pin) {
        PinKind::Commit
    } else {
        PinKind::Tag
    };
    let pin = if kind == PinKind::Commit {
        pin.to_ascii_lowercase()
    } else {
        pin.to_owned()
    };
    let dir = TempClone::create(data_root)?;
    Ok(GitPlan {
        url: url.to_owned(),
        pin,
        kind,
        dir,
    })
}

/// Whether `value` is exactly 40 hex characters.
fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Creates `path` (and ancestors) with mode 0700 on Unix; a plain
/// recursive create elsewhere.
#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<(), IngestError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| IngestError::GitTempDir {
            message: format!("cannot create {}: {e}", path.display()),
        })
}

/// Non-Unix variant: a plain recursive create, no POSIX mode.
#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<(), IngestError> {
    std::fs::create_dir_all(path).map_err(|e| IngestError::GitTempDir {
        message: format!("cannot create {}: {e}", path.display()),
    })
}

/// Verifies `path` carries mode 0700
/// (docs/implementation/09 §Local storage controls). Unix only: other
/// platforms have no POSIX mode.
#[cfg(unix)]
fn verify_private_dir(path: &Path) -> Result<(), IngestError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| IngestError::GitTempDir {
            message: format!("cannot stat {}: {e}", path.display()),
        })?
        .permissions()
        .mode();
    if mode & 0o777 != 0o700 {
        return Err(IngestError::GitTempDir {
            message: format!(
                "{} has mode {:o}, expected 700",
                path.display(),
                mode & 0o777
            ),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_private_dir(_path: &Path) -> Result<(), IngestError> {
    Ok(())
}

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
fn run_git(cwd: &Path, args: &[&str], extra_config: &[&str]) -> Result<String, IngestError> {
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

/// Walks the clone directory with the `ignore` walker and collects the
/// docs matching the extension allowlist. The walk respects `.gitignore`
/// (docs/implementation/07 §Git adapter): `require_git(true)` keeps the
/// ignore rules scoped to repositories, `follow_links(false)` never
/// follows a symlink out of the clone.
///
/// The extension allowlist filters in the loop, not through an override
/// whitelist: the `ignore` crate gives overrides the highest precedence —
/// an override match short-circuits before the ignore rules are consulted
/// (ignore 0.4.34 `dir.rs`, `matched_ignore`) — so a `*.md` override
/// would re-admit `.gitignore`d markdown. Filtering after the walk keeps
/// gitignore authoritative over the allowlist. Files that fail to read as
/// UTF-8 are skipped and counted.
fn walk_docs(dir: &Path, extensions: &[String]) -> Result<(Vec<PageDocument>, usize), IngestError> {
    let allowlist: Vec<String> = extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
    // The walker keeps the `ignore` default of skipping hidden entries:
    // dot files and dot directories (`.github/`, `.config/mkdocs.yml`)
    // never reach the allowlist (docs/implementation/07 §Git adapter).
    let walker = ignore::WalkBuilder::new(dir)
        .require_git(true)
        .follow_links(false)
        .build();

    let mut docs = Vec::new();
    let mut skipped_non_utf8 = 0;
    for entry in walker {
        let entry = entry.map_err(|e| IngestError::GitWalk {
            message: format!("walk failed: {e}"),
        })?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let allowed = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                let ext = ext.to_ascii_lowercase();
                allowlist.contains(&ext)
            });
        if !allowed {
            continue;
        }
        let relative = path.strip_prefix(dir).unwrap_or(path);
        let Ok(body) = std::fs::read_to_string(path) else {
            skipped_non_utf8 += 1;
            continue;
        };
        docs.push(PageDocument {
            path: relative.to_string_lossy().into_owned(),
            body,
        });
    }
    Ok((docs, skipped_non_utf8))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;
    use std::process::Command as StdCommand;
    use std::sync::Mutex;

    use devdocs_model::config::DEFAULT_GIT_EXTENSIONS;
    use devdocs_testkit::git_repo::{bare_mirror, docs_repo};
    use tempfile::tempdir;

    use super::{PinKind, install_signal_cleanup, plan};
    use crate::error::IngestError;

    /// The default extension allowlist as `Vec<String>`, the shape the
    /// config's `ingest.git_extensions` carries.
    fn default_extensions() -> Vec<String> {
        DEFAULT_GIT_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_owned())
            .collect()
    }

    /// Serializes the tests that create a `TempClone`: the clone-path
    /// registry is one process-global slot, so parallel `cargo test`
    /// threads would overwrite each other's registration. nextest runs
    /// each test in its own process and never contends.
    static TEMP_CLONE_ORDER: Mutex<()> = Mutex::new(());

    /// Runs `git` against a fixture with the same argv-only, pinned-env
    /// discipline the testkit uses, returning trimmed stdout.
    fn fixture_git(dir: &Path, args: &[&str]) -> String {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A tag clone checks out the `v1.0.0` content and records the tag's
    /// commit SHA (docs/implementation/07 §Git adapter).
    #[test]
    fn tag_clone_checks_out_the_tag_content() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        let git_plan = plan(&url, "v1.0.0", data.path()).unwrap();
        assert_eq!(git_plan.kind(), PinKind::Tag);
        assert_eq!(git_plan.pin(), "v1.0.0");

        let fetched = git_plan.fetch(&url, &default_extensions()).unwrap();
        assert_eq!(fetched.commit_sha, repo.tag_sha);
        assert!(!fetched.fallback_used);
        let bodies: Vec<&str> = fetched.docs.iter().map(|d| d.body.as_str()).collect();
        assert!(bodies.iter().any(|b| b.contains("The index page.")));
        assert!(bodies.iter().any(|b| b.contains("The guide body.")));
        // The tag checkout is shallow: the docs-v2 branch file never shows.
        assert!(!bodies.iter().any(|b| b.contains("Only on docs-v2.")));
    }

    /// A commit pin resolves directly on a local repository under the
    /// default protocol v2: no fallback
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn commit_pin_resolves_directly() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        let git_plan = plan(&url, &repo.tag_sha, data.path()).unwrap();
        assert_eq!(git_plan.kind(), PinKind::Commit);
        // A 40-hex pin normalizes to lowercase.
        assert_eq!(git_plan.pin(), repo.tag_sha.to_ascii_lowercase());

        let fetched = git_plan.fetch(&url, &default_extensions()).unwrap();
        assert_eq!(fetched.commit_sha, repo.tag_sha);
        assert!(!fetched.fallback_used);
    }

    /// A commit pin the server refuses to serve by SHA (the bare mirror
    /// under protocol v0, no `allowReachableSHA1InWant`) takes the
    /// full-clone fallback, reports it, and still checks the commit out
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn commit_pin_takes_the_full_clone_fallback_when_the_sha_fetch_fails() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let mirror_dir = tempdir().unwrap();
        let mirror = bare_mirror(&repo.work_dir, mirror_dir.path()).unwrap();

        // Mid-history commit: reachable from `main`, named by no ref.
        let mid_history = fixture_git(&repo.work_dir, &["rev-parse", "HEAD~1"]);
        let url = format!("file://{}", mirror.display());
        let data = tempdir().unwrap();

        let git_plan = plan(&url, &mid_history, data.path()).unwrap();
        // Force protocol v0 for this test only: protocol v2 — the git
        // default — allows reachable-SHA wants unconditionally, and no
        // server-side config disables that (testkit §bare_mirror).
        let fetched = git_plan
            .fetch_with(&url, &default_extensions(), &["protocol.version=0"])
            .unwrap();
        assert!(fetched.fallback_used);
        assert_eq!(fetched.commit_sha, mid_history);
        // The mid-history commit carries the README only.
        let bodies: Vec<&str> = fetched.docs.iter().map(|d| d.body.as_str()).collect();
        assert!(bodies.iter().any(|b| b.contains("The index page.")));
        assert!(!bodies.iter().any(|b| b.contains("The guide body.")));
    }

    /// A URL that changes between plan and fetch aborts the run
    /// (docs/implementation/09 §Git subprocess safety).
    #[test]
    fn a_changed_url_aborts_the_fetch() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        let git_plan = plan(&url, "v1.0.0", data.path()).unwrap();
        let error = git_plan
            .fetch("file:///tmp/somewhere-else", &default_extensions())
            .unwrap_err();
        match error {
            IngestError::GitUrlMismatch { planned, actual } => {
                assert_eq!(planned, url);
                assert_eq!(actual, "file:///tmp/somewhere-else");
            }
            other => panic!("expected GitUrlMismatch, got {other:?}"),
        }
    }

    /// A pin starting with `-` is a usage error: it cannot read as a git
    /// option on the argv vector (docs/implementation/09 §Git subprocess
    /// safety).
    #[test]
    fn an_option_shaped_pin_is_rejected() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        for pin in ["--upload-pack=evil", "-oProxyCommand=touch/nothing", "-"] {
            let error = plan(&url, pin, data.path()).unwrap_err();
            match error {
                IngestError::GitInvalidPin { pin: bad } => assert_eq!(bad, pin),
                other => panic!("expected GitInvalidPin for {pin:?}, got {other:?}"),
            }
        }
    }

    /// A tag carrying shell metacharacters is passed as one argv element:
    /// the clone simply fails to resolve it, and nothing executes
    /// (docs/implementation/09 §Git subprocess safety).
    #[test]
    fn shell_metacharacters_in_a_tag_stay_one_argv_element() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        for pin in [
            format!("x; touch {}", data.path().join("semicolon").display()),
            format!("x`touch {}`", data.path().join("backtick").display()),
        ] {
            let git_plan = plan(&url, &pin, data.path()).unwrap();
            assert_eq!(git_plan.kind(), PinKind::Tag, "pin: {pin}");
            let error = git_plan.fetch(&url, &default_extensions()).unwrap_err();
            assert!(
                matches!(error, IngestError::GitCommand { .. }),
                "pin: {pin}"
            );
        }
        // Neither metacharacter payload executed.
        assert!(!data.path().join("semicolon").exists());
        assert!(!data.path().join("backtick").exists());
    }

    /// The walk honors an untracked `.gitignore` in the fixture
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn the_walk_honors_gitignore() {
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join(".gitignore"), "secret.md\n").unwrap();
        std::fs::write(repo.work_dir.join("secret.md"), "# secret\n").unwrap();

        let (docs, skipped) = super::walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        assert_eq!(skipped, 0);
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
        assert!(paths.contains(&"docs/guide.md"), "paths: {paths:?}");
        assert!(!paths.contains(&"secret.md"), "paths: {paths:?}");
    }

    /// The default allowlist picks up the markdown and txt files of the
    /// fixture; a config-extended allowlist narrows the walk to the
    /// extensions it names (docs/implementation/07 §Git adapter).
    #[test]
    fn the_walk_honors_the_allowlists() {
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join("notes.txt"), "notes\n").unwrap();
        std::fs::write(repo.work_dir.join("spec.custom"), "custom\n").unwrap();

        // Default allowlist: markdown and txt in, the custom file out.
        let (docs, _) = super::walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
        assert!(paths.contains(&"notes.txt"), "paths: {paths:?}");
        assert!(!paths.contains(&"spec.custom"), "paths: {paths:?}");

        // A narrowed (config-shaped) allowlist: only the named extensions.
        let (docs, _) = super::walk_docs(&repo.work_dir, &["txt".to_owned()]).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["notes.txt"]);

        // An extended allowlist: the custom extension joins the set.
        let (docs, _) =
            super::walk_docs(&repo.work_dir, &["md".to_owned(), "custom".to_owned()]).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
        assert!(paths.contains(&"spec.custom"), "paths: {paths:?}");
        assert!(!paths.contains(&"notes.txt"), "paths: {paths:?}");
    }

    /// Files that fail to read as UTF-8 are skipped and counted
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn the_walk_skips_non_utf8_files() {
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join("bin.md"), [0xff, 0xfe, 0x00]).unwrap();

        let (docs, skipped) = super::walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        assert_eq!(skipped, 1);
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(!paths.contains(&"bin.md"), "paths: {paths:?}");
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
    }

    /// Dropping the plan removes the clone directory, the `tmp` parent
    /// keeps its mode 0700, and the clone directory itself was created
    /// 0700 (docs/implementation/09 §Local storage controls).
    #[cfg(unix)]
    #[test]
    fn the_temp_dir_is_removed_on_drop_and_created_private() {
        use std::os::unix::fs::PermissionsExt;

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let data = tempdir().unwrap();
        let url = format!("file://{}", repo.work_dir.display());

        let git_plan = plan(&url, "v1.0.0", data.path()).unwrap();
        let clone_dir = git_plan.dir().to_path_buf();
        let tmp_dir = data.path().join("tmp");
        assert!(clone_dir.exists());
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&tmp_dir), 0o700, "tmp parent");
        assert_eq!(mode(&clone_dir), 0o700, "clone dir");

        drop(git_plan);
        assert!(!clone_dir.exists());
        assert!(tmp_dir.exists());
    }

    /// The signal cleanup handler installs idempotently; the registry
    /// tracks the live clone path and the guard's drop clears both the
    /// registry slot and the directory.
    #[test]
    fn signal_registry_tracks_the_live_clone() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        install_signal_cleanup();
        install_signal_cleanup();
        let data = tempdir().unwrap();
        let guard = super::TempClone::create(data.path()).unwrap();
        assert_eq!(
            super::REGISTERED_PATH.lock().unwrap().as_deref(),
            Some(guard.path())
        );
        let path = guard.path().to_path_buf();
        drop(guard);
        assert_eq!(*super::REGISTERED_PATH.lock().unwrap(), None);
        assert!(!path.exists());
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

    /// A clone dir planted under the tmp root with an mtime older than
    /// the ingest total cap is swept at the next `TempClone::create`; a
    /// young one survives, as does the freshly created dir — the age
    /// bound keeps the sweep from racing a concurrent process's live
    /// clone (docs/implementation/09 §Local storage controls).
    #[test]
    fn a_stale_clone_dir_is_swept_at_the_next_create() {
        use std::fs::File;
        use std::time::{Duration, SystemTime};

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        // The tmp root exists at mode 0700 in production; the sweep must
        // not depend on `create` bootstrapping it.
        let tmp = data.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let stale = tmp.join("devdocs-git-stale");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("leftover"), "x").unwrap();
        // Age the planted dir past the cap: the sweep only removes
        // entries older than the total ingest cap.
        let old = SystemTime::now() - crate::pipeline::TOTAL_INGEST_CAP - Duration::from_secs(60);
        File::open(&stale).unwrap().set_modified(old).unwrap();

        // A young clone dir stays: it could belong to another process's
        // live clone (the registry is per-process).
        let fresh = tmp.join("devdocs-git-fresh");
        std::fs::create_dir_all(&fresh).unwrap();

        let guard = super::TempClone::create(data.path()).unwrap();
        assert!(!stale.exists(), "the stale clone dir must be swept");
        assert!(fresh.exists(), "the young clone dir must survive");
        assert!(guard.path().exists(), "the new clone dir must survive");
    }

    /// A 40-hex uppercase pin normalizes to lowercase; a 40-char
    /// non-hex value stays a tag pin (pin rule: exactly 40 hex
    /// characters).
    #[test]
    fn pin_classification_follows_the_40_hex_rule() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let git_plan = plan(
            "file:///nowhere",
            "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
            data.path(),
        )
        .unwrap();
        assert_eq!(git_plan.kind(), PinKind::Commit);
        assert_eq!(git_plan.pin(), "abcdef0123456789abcdef0123456789abcdef01");

        let not_hex = plan(
            "file:///nowhere",
            "g1234567890123456789012345678901234567",
            data.path(),
        )
        .unwrap();
        assert_eq!(not_hex.kind(), PinKind::Tag);
        assert_eq!(not_hex.pin(), "g1234567890123456789012345678901234567");

        let short = plan("file:///nowhere", "abc123", data.path()).unwrap();
        assert_eq!(short.kind(), PinKind::Tag);
    }
}
