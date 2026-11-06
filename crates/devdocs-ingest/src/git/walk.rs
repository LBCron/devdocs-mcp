//! The docs walk over the clone directory: the `ignore` walker with the
//! extension allowlist applied after gitignore
//! (docs/implementation/07 §Git adapter).

use std::path::Path;

use crate::chrome::PageDocument;
use crate::error::IngestError;

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
pub(crate) fn walk_docs(
    dir: &Path,
    extensions: &[String],
) -> Result<(Vec<PageDocument>, usize), IngestError> {
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

    use tempfile::tempdir;

    use super::super::test_util::default_extensions;
    use super::walk_docs;

    /// The walk honors an untracked `.gitignore` in the fixture
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn the_walk_honors_gitignore() {
        let repo_dir = tempdir().unwrap();
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join(".gitignore"), "secret.md\n").unwrap();
        std::fs::write(repo.work_dir.join("secret.md"), "# secret\n").unwrap();

        let (docs, skipped) = walk_docs(&repo.work_dir, &default_extensions()).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join("notes.txt"), "notes\n").unwrap();
        std::fs::write(repo.work_dir.join("spec.custom"), "custom\n").unwrap();

        // Default allowlist: markdown and txt in, the custom file out.
        let (docs, _) = walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
        assert!(paths.contains(&"notes.txt"), "paths: {paths:?}");
        assert!(!paths.contains(&"spec.custom"), "paths: {paths:?}");

        // A narrowed (config-shaped) allowlist: only the named extensions.
        let (docs, _) = walk_docs(&repo.work_dir, &["txt".to_owned()]).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["notes.txt"]);

        // An extended allowlist: the custom extension joins the set.
        let (docs, _) = walk_docs(&repo.work_dir, &["md".to_owned(), "custom".to_owned()]).unwrap();
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
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
        std::fs::write(repo.work_dir.join("bin.md"), [0xff, 0xfe, 0x00]).unwrap();

        let (docs, skipped) = walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        assert_eq!(skipped, 1);
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(!paths.contains(&"bin.md"), "paths: {paths:?}");
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
    }

    /// A subdirectory the walk cannot read surfaces as `GitWalk` — the
    /// walker error is the adapter's walk failure, not a silent skip
    /// (docs/implementation/07 §Git adapter).
    #[test]
    fn an_unreadable_subdir_fails_the_walk() {
        let repo_dir = tempdir().unwrap();
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();
        let locked = repo.work_dir.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let error = walk_docs(&repo.work_dir, &default_extensions()).unwrap_err();
        assert!(
            matches!(error, crate::error::IngestError::GitWalk { .. }),
            "{error:?}"
        );

        // Restore so the tempdir cleanup can run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// The walk collects the fixture docs under their clone-relative
    /// paths; the path accessor carries forward slashes on every host.
    #[test]
    fn the_walk_reports_clone_relative_paths() {
        let repo_dir = tempdir().unwrap();
        let repo = devdocs_testkit::git_repo::docs_repo(repo_dir.path()).unwrap();

        let (docs, skipped) = walk_docs(&repo.work_dir, &default_extensions()).unwrap();
        assert_eq!(skipped, 0);
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");
        assert!(paths.contains(&"docs/guide.md"), "paths: {paths:?}");
    }
}
