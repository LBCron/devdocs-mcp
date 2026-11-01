//! `Store::open` behavior: path checks and file creation mode.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_store::{Store, StoreOptions};
use tempfile::tempdir;

/// A database file this call creates lands with mode 0600.
#[cfg(unix)]
#[test]
fn created_database_file_gets_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("docs.db");
    let _store = Store::open(&path).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "created database file must be 0600");
}

/// A symlink to a file the current user owns opens fine: the ownership check
/// only refuses foreign territory.
#[cfg(unix)]
#[test]
fn symlink_to_an_owned_file_opens() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("real.db");
    let _real = Store::open(&target).unwrap();
    let link = dir.path().join("link.db");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let store = Store::open(&link).unwrap();
    assert!(store.migrate().is_ok());
}

/// A dangling symlink fails with a clear filesystem error instead of
/// creating a database at the target.
#[cfg(unix)]
#[test]
fn dangling_symlink_is_refused() {
    let dir = tempdir().unwrap();
    let link = dir.path().join("dangling.db");
    std::os::unix::fs::symlink(dir.path().join("nowhere.db"), &link).unwrap();

    let error = Store::open(&link).unwrap_err();
    assert!(
        matches!(error, devdocs_store::StoreError::Io { .. }),
        "expected StoreError::Io, got {error:?}"
    );
}

/// The network-filesystem refusal carries an explicit option, and opening
/// with it on a local directory still works.
#[test]
fn allow_network_fs_option_opens_local_paths() {
    let dir = tempdir().unwrap();
    let options = StoreOptions {
        allow_network_fs: true,
    };
    let store = Store::open_with(&dir.path().join("docs.db"), &options).unwrap();
    assert!(store.migrate().is_ok());
}

/// Concurrent first opens of the same fresh path all succeed: the loser of
/// the `create_new` race falls through to a normal open instead of failing.
///
/// Unix-only: the pre-creation the race runs through is a no-op elsewhere,
/// and the Windows VFS can refuse a concurrent journal-mode switch without
/// retrying the busy handler.
#[cfg(unix)]
#[test]
fn concurrent_first_opens_all_succeed() {
    use std::sync::Arc;

    let dir = tempdir().unwrap();
    let path = Arc::new(dir.path().join("docs.db"));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                Store::open(&path)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        })
        .collect();
    for handle in handles {
        if let Err(error) = handle.join().unwrap() {
            panic!("every concurrent open must succeed, got: {error}");
        }
    }
}

/// Opening into a missing directory fails with the path attached.
#[test]
fn missing_directory_fails_with_path_context() {
    let dir = tempdir().unwrap();
    let error = Store::open(&dir.path().join("missing").join("docs.db")).unwrap_err();
    match error {
        devdocs_store::StoreError::Io { path, .. } => {
            assert!(path.ends_with("docs.db"));
        }
        other => panic!("expected StoreError::Io, got {other:?}"),
    }
}
