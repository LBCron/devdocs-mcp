//! Local git fixture repositories (docs/implementation/04 §Fixtures).
//!
//! The helpers shell out to the `git` binary with argv vectors only: no
//! shell, no interpolated command line. The child env pins
//! `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`, removes every
//! inherited `GIT_*` variable, and each repo
//! sets `user.name` and `user.email` locally, so a fixture run cannot
//! read the host identity or ever prompt.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A docs repository built at test time, with the paths and resolved
/// SHAs its callers would otherwise re-derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRepo {
    /// The working tree of the fixture repository.
    pub work_dir: PathBuf,
    /// The commit `v1.0.0` tags.
    pub tag_sha: String,
    /// The tip of the second branch `docs-v2`.
    pub branch_sha: String,
    /// The tip of the default branch.
    pub default_sha: String,
}

/// Run `git` with `args` in `dir` and return trimmed stdout. The
/// isolation env and argv-only invocation keep the child off the host's
/// interactive and system configuration. The child runs with a clean
/// git environment: all inherited `GIT_*` variables are removed.
fn git<I, S>(dir: &Path, args: I) -> io::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let command: Vec<std::ffi::OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();
    let mut child = Command::new("git");
    // A hook process inherits GIT_DIR, GIT_INDEX_FILE and friends; drop
    // them so the child only sees the fixture repository.
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            child.env_remove(&key);
        }
    }
    let output = child
        .args(&command)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {command:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Set the identity locally so commits never read the host config.
fn configure_local_identity(dir: &Path) -> io::Result<()> {
    git(dir, ["config", "--local", "user.name", "devdocs-test"])?;
    git(
        dir,
        [
            "config",
            "--local",
            "user.email",
            "devdocs-test@example.test",
        ],
    )?;
    // A host that signs commits by default would break the fixture.
    git(dir, ["config", "--local", "commit.gpgsign", "false"])?;
    Ok(())
}

/// Write `relative` under `dir` with `content`, creating parent dirs.
fn write_file(dir: &Path, relative: &str, content: &str) -> io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Build a tiny docs repository in `dir`: two files, two commits on the
/// default branch, tag `v1.0.0` on the second commit, and a second
/// branch `docs-v2` with one extra commit so pin tests can tell the
/// branches apart.
pub fn docs_repo(dir: &Path) -> io::Result<DocsRepo> {
    git(dir, ["init", "-b", "main"])?;
    configure_local_identity(dir)?;

    write_file(dir, "README.md", "# docs\n\nThe index page.\n")?;
    git(dir, ["add", "."])?;
    git(dir, ["commit", "-m", "first commit"])?;

    write_file(dir, "docs/guide.md", "# Guide\n\nThe guide body.\n")?;
    git(dir, ["add", "."])?;
    git(dir, ["commit", "-m", "second commit"])?;

    let default_sha = git(dir, ["rev-parse", "HEAD"])?;
    git(dir, ["tag", "v1.0.0"])?;
    let tag_sha = git(dir, ["rev-parse", "v1.0.0"])?;

    git(dir, ["checkout", "-b", "docs-v2"])?;
    write_file(dir, "docs/v2.md", "# v2\n\nOnly on docs-v2.\n")?;
    git(dir, ["add", "."])?;
    git(dir, ["commit", "-m", "docs-v2 commit"])?;
    let branch_sha = git(dir, ["rev-parse", "docs-v2"])?;
    // Leave HEAD on the default branch so a later bare clone mirrors it.
    git(dir, ["checkout", "main"])?;

    Ok(DocsRepo {
        work_dir: dir.to_path_buf(),
        tag_sha,
        branch_sha,
        default_sha,
    })
}

/// Clone `source` into a bare repository at `dir/mirror.git` and return
/// the mirror path. The `file://` origin forces every later fetch through
/// upload-pack, and the mirror pins `uploadpack.allowReachableSHA1InWant`
/// to `false`, so a reachable but unadvertised full SHA is rejected under
/// protocol v0. Protocol v2 (git's default) allows reachable-SHA wants
/// and no server-side config disables that, so callers that need the
/// rejection must fetch with `-c protocol.version=0`.
pub fn bare_mirror(source: &Path, dir: &Path) -> io::Result<PathBuf> {
    let mirror = dir.join("mirror.git");
    let source_url = format!("file://{}", source.display());
    git(
        dir,
        vec![
            "clone".to_owned(),
            "--bare".to_owned(),
            source_url,
            mirror.to_string_lossy().into_owned(),
        ],
    )?;
    git(
        &mirror,
        [
            "config",
            "--local",
            "uploadpack.allowReachableSHA1InWant",
            "false",
        ],
    )?;
    Ok(mirror)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use tempfile::tempdir;

    use super::{bare_mirror, docs_repo, git};

    /// `docs_repo` builds, `v1.0.0` resolves on the default tip, and the
    /// second branch exists with a differing commit.
    #[test]
    fn docs_repo_builds_and_resolves() {
        let dir = tempdir().unwrap();
        let repo = docs_repo(dir.path()).unwrap();

        assert_eq!(repo.work_dir, dir.path());
        // Two commits, then the tag: v1.0.0 sits on the default tip.
        assert_eq!(repo.tag_sha, repo.default_sha);
        assert_eq!(repo.tag_sha.len(), 40);

        // The second branch exists and carries a differing commit.
        let branches = git(dir.path(), ["branch", "--list", "docs-v2"]).unwrap();
        assert!(branches.contains("docs-v2"), "branches: {branches}");
        assert_ne!(repo.branch_sha, repo.default_sha);

        // The two committed files are in the working tree.
        assert!(dir.path().join("README.md").is_file());
        assert!(dir.path().join("docs/guide.md").is_file());
    }

    /// `bare_mirror` clones the fixture, and the local file transport
    /// rejects a fetch of a reachable but unadvertised full SHA — the
    /// exact precondition the fallback-path test relies on, asserted
    /// here so it can never silently degrade into the direct path.
    ///
    /// The fetch runs from a probe repo that lacks the object (a fetch
    /// inside the mirror short-circuits on objects it already has) and
    /// pins protocol v0, because protocol v2 — git's default — allows
    /// reachable-SHA wants unconditionally, with no server-side config
    /// to disable that. The pinned SHA is a mid-history commit: an
    /// advertised ref tip is always fetchable, even under v0.
    #[test]
    fn bare_mirror_rejects_reachable_sha_fetch() {
        let repo_dir = tempdir().unwrap();
        let repo = docs_repo(repo_dir.path()).unwrap();
        let mirror_dir = tempdir().unwrap();
        let mirror = bare_mirror(&repo.work_dir, mirror_dir.path()).unwrap();

        // A bare mirror has no work tree; HEAD resolves to the cloned
        // default tip.
        let head = git(&mirror, ["rev-parse", "HEAD"]).unwrap();
        assert_eq!(head, repo.default_sha);

        // The first commit: reachable from `main`, but no ref names it.
        let mid_history = git(&repo.work_dir, ["rev-parse", "HEAD~1"]).unwrap();

        let probe = tempdir().unwrap();
        git(probe.path(), ["init", "-q"]).unwrap();
        let mirror_url = format!("file://{}", mirror.display());
        let fetch = git(
            probe.path(),
            [
                "-c",
                "protocol.version=0",
                "fetch",
                mirror_url.as_str(),
                &mid_history,
            ],
        );
        assert!(fetch.is_err(), "reachable-SHA fetch unexpectedly succeeded");
    }
}
