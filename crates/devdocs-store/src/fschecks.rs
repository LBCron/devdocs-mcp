//! Filesystem checks run at `Store::open`.
//!
//! Contract: docs/implementation/09 §Local storage controls.

use std::fs;
use std::path::Path;

use crate::error::StoreError;

/// Whether `file_uid` names another user's territory.
#[cfg(unix)]
fn is_foreign_owner(file_uid: u32) -> bool {
    file_uid != nix::unistd::geteuid().as_raw()
}

/// Refuses `path` when it is a symlink that resolves to a file the current
/// user does not own.
///
/// A symlink into another user's territory stays an error even with
/// `StoreOptions::allow_network_fs`, because that option only covers the
/// network-filesystem refusal (docs/implementation/09).
#[cfg(unix)]
pub(crate) fn check_symlink_owner(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;

    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        // A fresh database has no file yet, so no symlink either.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if !meta.is_symlink() {
        return Ok(());
    }
    let resolved = path.canonicalize().map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    let meta = fs::metadata(&resolved).map_err(|source| StoreError::Io {
        path: resolved.clone(),
        source,
    })?;
    if is_foreign_owner(meta.uid()) {
        return Err(StoreError::ForeignSymlink {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// Non-Unix platforms carry no POSIX symlink-ownership semantics to check.
#[cfg(not(unix))]
pub(crate) fn check_symlink_owner(path: &Path) -> Result<(), StoreError> {
    let _ = path;
    Ok(())
}

/// Refuses `path` when the filesystem holding it is a known network mount.
#[cfg(target_os = "macos")]
pub(crate) fn check_network_mount(path: &Path) -> Result<(), StoreError> {
    let mount = statfs_of(path)?;
    // MNT_LOCAL marks filesystems stored locally; without it the mount is
    // network-backed (automounts, SMB, NFS, ...).
    if !mount.flags().contains(nix::mount::MntFlags::MNT_LOCAL) {
        return Err(StoreError::NetworkFsPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// Refuses `path` when the filesystem holding it is a known network mount.
#[cfg(target_os = "linux")]
pub(crate) fn check_network_mount(path: &Path) -> Result<(), StoreError> {
    use nix::sys::statfs::FsType;
    use nix::sys::statfs::{AFS_SUPER_MAGIC, CODA_SUPER_MAGIC, FUSE_SUPER_MAGIC, NFS_SUPER_MAGIC};
    use nix::sys::statfs::{NCP_SUPER_MAGIC, OCFS2_SUPER_MAGIC, SMB_SUPER_MAGIC};

    // Magic numbers from linux/magic.h that nix does not ship.
    const SMB2_SUPER_MAGIC: FsType = FsType(0xFE53_4D42);
    const V9FS_SUPER_MAGIC: FsType = FsType(0x0102_1997);
    const NETWORK_MAGICS: [FsType; 8] = [
        NFS_SUPER_MAGIC,
        SMB_SUPER_MAGIC,
        SMB2_SUPER_MAGIC,
        AFS_SUPER_MAGIC,
        NCP_SUPER_MAGIC,
        CODA_SUPER_MAGIC,
        V9FS_SUPER_MAGIC,
        OCFS2_SUPER_MAGIC,
    ];

    let mount = statfs_of(path)?;
    let fs_type = mount.filesystem_type();
    if NETWORK_MAGICS.contains(&fs_type) {
        return Err(StoreError::NetworkFsPath {
            path: path.to_owned(),
        });
    }
    // Every FUSE mount reports FUSE_SUPER_MAGIC, local ones included
    // (ntfs-3g over FUSE), so the refusal needs the subtype from
    // /proc/self/mountinfo: sshfs and friends hide behind the shared magic.
    if fs_type == FUSE_SUPER_MAGIC
        && let Some(subtype) = covering_mount_fstype(&existing_ancestor(path)?)
            .as_deref()
            .and_then(|fstype| fstype.strip_prefix("fuse."))
        && NETWORK_FUSE_SUBTYPES.contains(&subtype)
    {
        return Err(StoreError::NetworkFsPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// FUSE subtypes that serve a store from network or object storage. A plain
/// `fuse` fstype with no subtype, and local FUSE filesystems, stay allowed.
#[cfg(target_os = "linux")]
const NETWORK_FUSE_SUBTYPES: &[&str] = &[
    "sshfs",
    "s3fs",
    "rclone",
    "gcsfuse",
    "goofys",
    "juicefs",
    "blobfuse2",
];

/// The fstype of the mount covering `target`, read from
/// `/proc/self/mountinfo`: the line with the longest mount point that is a
/// path-prefix of `target` wins, so a nested mount shadows its parent.
#[cfg(target_os = "linux")]
fn covering_mount_fstype(target: &Path) -> Option<String> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    covering_mount_fstype_from_lines(&mountinfo, target)
}

/// Pure core of [`covering_mount_fstype`]: finds the covering mount in the
/// given mountinfo lines.
///
/// Mountinfo fields escape space, tab, newline, and backslash as `\040`,
/// `\011`, `\012`, and `\134`.
#[cfg(target_os = "linux")]
fn covering_mount_fstype_from_lines(mountinfo: &str, target: &Path) -> Option<String> {
    let mut best: Option<String> = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(separator) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        let Some(mountpoint) = fields.get(4) else {
            continue;
        };
        let mountpoint = decode_mountinfo_escapes(mountpoint);
        if !target.strip_prefix(&mountpoint).is_ok() {
            continue;
        }
        // Equal lengths mean an overmount: the later line shadows the
        // earlier one covering the same mount point.
        if best.is_none() || mountpoint.len() >= best.as_ref().map_or(0, String::len) {
            best = Some(fields[separator + 1].to_owned());
        }
    }
    best
}

/// Decodes the four mountinfo escape sequences.
#[cfg(target_os = "linux")]
fn decode_mountinfo_escapes(field: &str) -> String {
    // `\134` (a literal backslash) last, so an escaped backslash followed by
    // an escape like `\134040` survives as `\040` instead of becoming a
    // second decode.
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// No portable network-mount signal exists on other platforms; the check
/// degrades to "assume local" (docs/implementation/09 scopes the control to
/// known network mounts).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn check_network_mount(path: &Path) -> Result<(), StoreError> {
    let _ = path;
    Ok(())
}

/// The deepest existing ancestor of `path` — the file itself when it exists,
/// its parent directory otherwise (a fresh database has no file yet).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn existing_ancestor(path: &Path) -> Result<std::path::PathBuf, StoreError> {
    if path.exists() {
        return Ok(path.to_owned());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.exists() {
        return Err(StoreError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "database directory does not exist",
            ),
        });
    }
    Ok(parent.to_owned())
}

/// `statfs` on the deepest existing ancestor of `path` — the DB file when it
/// exists, its parent directory when it does not (a fresh database has no
/// file yet).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn statfs_of(path: &Path) -> Result<nix::sys::statfs::Statfs, StoreError> {
    let target = existing_ancestor(path)?;
    nix::sys::statfs::statfs(&target).map_err(|source| StoreError::Io {
        path: target,
        source: std::io::Error::from(source),
    })
}

/// Fails with the path attached when the database directory does not exist.
///
/// Platform-neutral: without it, non-Unix open paths surface the missing
/// directory as a SQLite `CannotOpen` error instead of a filesystem error.
pub(crate) fn check_parent_exists(path: &Path) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.exists() {
        return Err(StoreError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "database directory does not exist",
            ),
        });
    }
    Ok(())
}

/// Creates the database file with mode 0600 when this call creates it.
///
/// rusqlite would create it with `0666 & !umask`; the security contract
/// wants 0600 (docs/implementation/09 §Local storage controls). An existing
/// file keeps its mode, and a concurrent first opener that won the
/// `create_new` race is treated as success — it created the file 0600 too.
#[cfg(unix)]
pub(crate) fn ensure_private_file(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::OpenOptionsExt;

    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(StoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Non-Unix platforms have no file mode to enforce at creation.
#[cfg(not(unix))]
pub(crate) fn ensure_private_file(path: &Path) -> Result<(), StoreError> {
    let _ = path;
    Ok(())
}

/// Warns when the file or its directory drifted from the 0600/0700 contract.
///
/// Verification warns rather than refuses (docs/implementation/09: "The
/// store verifies modes at open and warns on drift").
#[cfg(unix)]
pub(crate) fn warn_on_mode_drift(path: &Path) {
    use std::os::unix::fs::MetadataExt;
    use tracing::warn;

    if let Ok(meta) = fs::metadata(path) {
        let mode = meta.mode() & 0o777;
        if mode != 0o600 {
            warn!(
                file = %path.display(),
                mode = format_args!("{mode:04o}"),
                "database file mode drifted from 0600"
            );
        }
    }
    if let Some(parent) = path.parent()
        && let Ok(meta) = fs::metadata(parent)
    {
        let mode = meta.mode() & 0o777;
        if mode != 0o700 {
            warn!(
                directory = %parent.display(),
                mode = format_args!("{mode:04o}"),
                "database directory mode drifted from 0700"
            );
        }
    }
}

/// Non-Unix platforms have no POSIX file mode to compare against.
#[cfg(not(unix))]
pub(crate) fn warn_on_mode_drift(path: &Path) {
    let _ = path;
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{covering_mount_fstype_from_lines, decode_mountinfo_escapes};
    use std::path::Path;

    /// A representative `/proc/self/mountinfo` body: a root mount, an sshfs
    /// mount under `/mnt`, and a nested FUSE mount with an escaped space.
    const MOUNTINFO: &str = "\
21 1 0:20 / / rw - ext4 /dev/sda1 rw
35 21 0:34 / /mnt rw - fuse.sshfs user@host:/srv/docs
38 35 0:40 / /mnt/ssh/my\\040docs rw - fuse.sshfs sshfs#user@host:/srv rw
41 21 0:44 / /mnt/inner rw - fuse.ext4 /dev/loop0 rw";

    /// sshfs hides behind FUSE_SUPER_MAGIC in statfs; mountinfo carries the
    /// subtype, and the deepest covering mount wins.
    #[test]
    fn the_covering_mount_fstype_comes_from_the_deepest_prefix() {
        let fstype =
            covering_mount_fstype_from_lines(MOUNTINFO, Path::new("/mnt/ssh/docs.db")).unwrap();
        assert_eq!(fstype, "fuse.sshfs");
        let nested =
            covering_mount_fstype_from_lines(MOUNTINFO, Path::new("/mnt/ssh/my docs/x")).unwrap();
        assert_eq!(nested, "fuse.sshfs", "escaped space in the mount point");
    }

    /// A local FUSE filesystem under a network one is reported by its own
    /// covering mount, and prefixes stop at component boundaries.
    #[test]
    fn prefix_matching_stops_at_component_boundaries() {
        let fstype =
            covering_mount_fstype_from_lines(MOUNTINFO, Path::new("/mnt/inner/db")).unwrap();
        assert_eq!(fstype, "fuse.ext4", "a deeper mount shadows its parent");
        let outside =
            covering_mount_fstype_from_lines(MOUNTINFO, Path::new("/mntx/docs.db")).unwrap();
        assert_eq!(
            outside, "ext4",
            "/mnt must not cover /mntx; the root mount answers"
        );
    }

    /// The four escape sequences decode; an escaped backslash before an
    /// escape stays a literal.
    #[test]
    fn mountinfo_escapes_decode() {
        assert_eq!(decode_mountinfo_escapes("my\\040docs"), "my docs");
        assert_eq!(
            decode_mountinfo_escapes("a\\011b\\012c\\134040d"),
            "a\tb\nc\\040d"
        );
    }
}
