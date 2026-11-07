//! The private clone directory: a mode-0700 `TempClone` with a `Drop`
//! guard, the signal-cleanup handler, and the stale-clone sweep
//! (docs/implementation/09 §Local storage controls).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use crate::error::IngestError;
use crate::pipeline::TOTAL_INGEST_CAP;

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
            .ok()
            .and_then(|meta| meta.modified().ok())
            .is_some_and(|mtime| mtime.elapsed().is_ok_and(|age| age >= TOTAL_INGEST_CAP));
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
    pub(crate) fn create(data_root: &Path) -> Result<Self, IngestError> {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use tempfile::tempdir;

    use super::super::test_util::{TEMP_CLONE_ORDER, fixture_git};
    use super::{REGISTERED_PATH, TempClone, install_signal_cleanup};
    #[cfg(unix)]
    use super::{create_private_dir, verify_private_dir};

    /// Ages a path's mtime past the sweep bound. Windows cannot open a
    /// directory with a plain `File::open`; `FILE_FLAG_BACKUP_SEMANTICS`
    /// grants a directory handle there.
    fn age_path(path: &Path, old: std::time::SystemTime) {
        #[cfg(unix)]
        {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .custom_flags(0x0200_0000)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }

    /// A path that already exists as a file refuses the private-dir
    /// create: the error names the path (`GitTempDir` mapping).
    #[cfg(unix)]
    #[test]
    fn create_private_dir_refuses_an_occupied_path() {
        let data = tempdir().unwrap();
        let path = data.path().join("occupied");
        std::fs::write(&path, "x").unwrap();

        let error = create_private_dir(&path).unwrap_err();
        assert!(
            matches!(error, crate::error::IngestError::GitTempDir { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("occupied"));
    }

    /// A missing path refuses the private-dir verification: the stat
    /// failure maps to `GitTempDir` naming the path.
    #[cfg(unix)]
    #[test]
    fn verify_private_dir_refuses_a_missing_path() {
        let data = tempdir().unwrap();
        let path = data.path().join("absent");

        let error = verify_private_dir(&path).unwrap_err();
        assert!(
            matches!(error, crate::error::IngestError::GitTempDir { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("absent"));
    }

    /// Dropping the plan removes the clone directory, the `tmp` parent
    /// keeps its mode 0700, and the clone directory itself was created
    /// 0700 (docs/implementation/09 §Local storage controls).
    #[cfg(unix)]
    #[test]
    fn the_temp_dir_is_removed_on_drop_and_created_private() {
        use std::os::unix::fs::PermissionsExt;

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();

        let guard = TempClone::create(data.path()).unwrap();
        let clone_dir = guard.path().to_path_buf();
        let tmp_dir = data.path().join("tmp");
        assert!(clone_dir.exists());
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&tmp_dir), 0o700, "tmp parent");
        assert_eq!(mode(&clone_dir), 0o700, "clone dir");

        drop(guard);
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
        let guard = TempClone::create(data.path()).unwrap();
        assert_eq!(
            REGISTERED_PATH.lock().unwrap().as_deref(),
            Some(guard.path())
        );
        let path = guard.path().to_path_buf();
        drop(guard);
        assert_eq!(*REGISTERED_PATH.lock().unwrap(), None);
        assert!(!path.exists());
    }

    /// A clone dir planted under the tmp root with an mtime older than
    /// the ingest total cap is swept at the next `TempClone::create`; a
    /// young one survives, as does the freshly created dir — the age
    /// bound keeps the sweep from racing a concurrent process's live
    /// clone (docs/implementation/09 §Local storage controls).
    #[test]
    fn a_stale_clone_dir_is_swept_at_the_next_create() {
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
        age_path(&stale, old);

        // A young clone dir stays: it could belong to another process's
        // live clone (the registry is per-process).
        let fresh = tmp.join("devdocs-git-fresh");
        std::fs::create_dir_all(&fresh).unwrap();

        let guard = TempClone::create(data.path()).unwrap();
        assert!(!stale.exists(), "the stale clone dir must be swept");
        assert!(fresh.exists(), "the young clone dir must survive");
        assert!(guard.path().exists(), "the new clone dir must survive");
    }

    /// A non-directory `devdocs-git-*` entry (a stray file) is also a
    /// sweep candidate: the sweep removes it by file, not only by dir.
    #[test]
    fn the_sweep_removes_a_stale_stray_file() {
        use std::time::{Duration, SystemTime};

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let tmp = data.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let stray = tmp.join("devdocs-git-loose");
        std::fs::write(&stray, "not a dir").unwrap();
        let old = SystemTime::now() - crate::pipeline::TOTAL_INGEST_CAP - Duration::from_secs(60);
        age_path(&stray, old);

        let guard = TempClone::create(data.path()).unwrap();
        assert!(!stray.exists(), "the stale stray file must be swept");
        assert!(guard.path().exists());
    }

    /// A fixture clone keeps working while a live clone holds the
    /// registry slot: the sweep skips the registered path even when it is
    /// old enough to be a candidate (docs/implementation/09 §Local
    /// storage controls).
    #[test]
    fn the_sweep_skips_the_registered_clone() {
        use std::time::{Duration, SystemTime};

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let guard = TempClone::create(data.path()).unwrap();
        let registered = guard.path().to_path_buf();
        // Age the live clone past the cap.
        let old = SystemTime::now() - crate::pipeline::TOTAL_INGEST_CAP - Duration::from_secs(60);
        age_path(&registered, old);

        // A second create in the same process sweeps, but must skip the
        // registered path.
        let _second = TempClone::create(data.path()).unwrap();
        assert!(registered.exists(), "the registered clone must survive");
    }

    /// A tmp root left at 0755 fails the private-dir contract: the create
    /// refuses with `GitTempDir` naming the wrong mode
    /// (docs/implementation/09 §Local storage controls). POSIX modes only:
    /// Windows has no owner-rwx restriction to violate.
    #[cfg(unix)]
    #[test]
    fn an_unprivate_tmp_root_refuses_the_create() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let tmp = data.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let error = TempClone::create(data.path()).unwrap_err();
        let message = match &error {
            crate::error::IngestError::GitTempDir { message } => message.clone(),
            other => panic!("expected GitTempDir, got {other:?}"),
        };
        assert!(message.contains("755"), "message: {message}");
    }

    /// A guard whose clone dir cannot be removed logs the failure and
    /// clears the registry slot anyway (the `Drop` warn path). POSIX only:
    /// a write-revoked directory stays removable on Windows.
    #[cfg(unix)]
    #[test]
    fn an_unremovable_clone_dir_warns_on_drop() {
        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let guard = TempClone::create(data.path()).unwrap();
        let path = guard.path().to_path_buf();
        // Content inside: removal must unlink it, which the revoked write
        // bit below forbids.
        std::fs::write(path.join("leftover"), "x").unwrap();

        // Make removal impossible: no write permission on the clone dir.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        drop(guard);
        assert!(path.exists(), "the unremovable clone dir must survive");

        // Restore so the parent tempdir can clean up.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::remove_dir_all(&path).unwrap();
        assert_eq!(*REGISTERED_PATH.lock().unwrap(), None);
    }

    /// The sweep ignores a tmp root it cannot read, and a non-clone entry
    /// in the root is skipped untouched.
    #[test]
    fn the_sweep_skips_unreadable_roots_and_foreign_entries() {
        // A nonexistent root: the early return keeps the create alive.
        super::sweep_stale_clones(Path::new("/devdocs-nonexistent-root"));

        let _order = TEMP_CLONE_ORDER.lock().unwrap();
        let data = tempdir().unwrap();
        let tmp = data.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // A foreign entry (not a `devdocs-git-*` name) never gets touched.
        let foreign = tmp.join("other-run");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("data"), "x").unwrap();
        let guard = TempClone::create(data.path()).unwrap();
        assert!(foreign.exists(), "the foreign entry must survive");
        assert!(guard.path().exists());
    }

    /// The `fixture_git` helper runs real git and trims stdout (used by
    /// the clone tests; asserted here once).
    #[test]
    fn fixture_git_returns_trimmed_stdout() {
        let dir = tempdir().unwrap();
        let version = fixture_git(dir.path(), &["--version"]);
        assert!(version.starts_with("git version"), "version: {version}");
    }
}
