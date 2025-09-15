//! Mount helpers for starting/stopping FUSE
//!
//! Notes:
//! - Only supported on Unix-like systems. On Linux we support unprivileged mount via fusermount3.
//! - These helpers are thin wrappers over rfuse3 raw Session APIs.

use std::path::Path;

use rfuse3::MountOptions;

use crate::chuck::store::BlockStore;
use crate::meta::MetaStore;
use crate::vfs::fs::VFS;

/// Build default mount options for SlayerFS.
#[allow(dead_code)]
fn default_mount_options() -> MountOptions {
    let mut mo = MountOptions::default();
    mo.fs_name("slayerfs");
    // Keep defaults conservative: no allow_other, require empty mountpoint.
    mo
}

/// Mount a VFS instance to the given empty directory using unprivileged mode when available.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub async fn mount_vfs_unprivileged<S, M>(
    fs: VFS<S, M>,
    mount_point: impl AsRef<Path>,
) -> std::io::Result<rfuse3::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaStore + Send + Sync + 'static,
{
    let opts = default_mount_options();
    let session = rfuse3::raw::Session::new(opts);
    // Prefer unprivileged mount on Linux (requires fusermount3 in PATH)
    session.mount_with_unprivileged(fs, mount_point).await
}

/// Fallback stub for non-Linux targets.
#[cfg(not(target_os = "linux"))]
pub async fn mount_vfs_unprivileged<S, M>(
    _fs: VFS<S, M>,
    _mount_point: impl AsRef<Path>,
) -> std::io::Result<rfuse3::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaStore + Send + Sync + 'static,
{
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FUSE mount is only supported on Linux in this build",
    ))
}
