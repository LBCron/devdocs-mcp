//! Pin planning and the pinned fetch: a shallow tag clone, or a commit
//! fetch with a full-clone fallback, then the docs walk
//! (docs/implementation/07 §Git adapter).

use std::path::Path;

use crate::chrome::PageDocument;
use crate::error::IngestError;

use super::command::run_git;
use super::temp::TempClone;
use super::walk::walk_docs;

/// What kind of pin a plan resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    /// The pin names a tag; the clone checks the tag out shallowly.
    Tag,
    /// The pin is a 40-hex commit SHA; the clone fetches the SHA and
    /// detaches onto it.
    Commit,
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
        let fallback_used = match self.kind {
            PinKind::Tag => {
                clone_tag(tmp, url, &self.pin, &dir.to_string_lossy(), extra_config)?;
                false
            }
            PinKind::Commit => clone_commit(dir, tmp, url, &self.pin, extra_config)?,
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

/// Clones the planned tag shallowly: one branch, no blobs, no checkout of
/// any other ref.
fn clone_tag(
    tmp: &Path,
    url: &str,
    pin: &str,
    dir_arg: &str,
    extra_config: &[&str],
) -> Result<(), IngestError> {
    run_git(
        tmp,
        &[
            "clone",
            "--depth",
            "1",
            "--branch",
            pin,
            "--single-branch",
            "--filter=blob:none",
            url,
            dir_arg,
        ],
        extra_config,
    )?;
    Ok(())
}

/// Clones a commit pin and returns whether the full-clone fallback ran:
/// a shallow clone first, then the SHA fetch; when the server refuses the
/// SHA fetch, the clone dir is cleared and re-cloned in full
/// (docs/implementation/07 §Git adapter).
fn clone_commit(
    dir: &Path,
    tmp: &Path,
    url: &str,
    pin: &str,
    extra_config: &[&str],
) -> Result<bool, IngestError> {
    let dir_arg = dir.to_string_lossy().into_owned();
    run_git(
        tmp,
        &["clone", "--depth", "1", "--filter=blob:none", url, &dir_arg],
        extra_config,
    )?;
    let direct = run_git(dir, &["fetch", "--depth", "1", "origin", pin], extra_config);
    let Err(direct_error) = direct else {
        return Ok(false);
    };
    tracing::warn!(
        "the SHA fetch of {} failed; falling back to a full clone: {direct_error}",
        pin
    );
    // The server rejected the SHA fetch (no allowReachableSHA1InWant):
    // fall back to a full clone and report it
    // (docs/implementation/07 §Git adapter).
    std::fs::remove_dir_all(dir).map_err(|e| IngestError::GitTempDir {
        message: format!("cannot clear {}: {e}", dir.display()),
    })?;
    create_private_dir(dir)?;
    run_git(tmp, &["clone", url, &dir_arg], extra_config)?;
    Ok(true)
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
pub(crate) fn is_commit_sha(value: &str) -> bool {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use tempfile::tempdir;

    use super::super::test_util::{TEMP_CLONE_ORDER, default_extensions, fixture_git};
    use super::{GitPlan, PinKind, is_commit_sha, plan};
    use crate::error::IngestError;

    /// A tag clone checks out the `v1.0.0` content and records the tag's
    /// commit SHA (docs/implementation/07 §Git adapter).
    #[test]
    fn tag_clone_checks_out_the_tag_content() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let repo_dir = tempdir().unwrap();
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
        let mirror_dir = tempdir().unwrap();
        let mirror =
            devdocs_testkit::git_repo::bare_mirror(&repo.work_dir, mirror_dir.path()).unwrap();

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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
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

    /// The 40-hex rule itself: exactly 40 ascii hex characters, nothing
    /// shorter or non-hex.
    #[test]
    fn is_commit_sha_follows_the_40_hex_rule() {
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha(&"a".repeat(39)));
        assert!(!is_commit_sha(&"a".repeat(41)));
        assert!(!is_commit_sha(&"g".repeat(40)));
    }

    /// The plan carries the pinned URL verbatim; the dir accessor points
    /// into the plan's private clone directory.
    #[test]
    fn the_plan_exposes_its_url_pin_and_dir() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let git_plan: GitPlan = plan("file:///nowhere", "v1.0.0", data.path()).unwrap();
        assert_eq!(git_plan.url(), "file:///nowhere");
        assert_eq!(git_plan.pin(), "v1.0.0");
        assert!(git_plan.dir().starts_with(data.path().join("tmp")));
    }
}
