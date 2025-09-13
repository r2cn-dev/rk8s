use rfuse3::raw::reply::{FileAttr, ReplyXAttr};
use rfuse3::{
    Inode, Result,
    raw::{Filesystem, Request, reply::ReplyEntry},
};
use std::ffi::OsStr;
use std::io::Error;

use crate::passthrough::PassthroughFs;
pub const OPAQUE_XATTR_LEN: u32 = 16;
pub const OPAQUE_XATTR: &str = "user.fuseoverlayfs.opaque";
pub const UNPRIVILEGED_OPAQUE_XATTR: &str = "user.overlay.opaque";
pub const PRIVILEGED_OPAQUE_XATTR: &str = "trusted.overlay.opaque";

/// A filesystem must implement Layer trait, or it cannot be used as an OverlayFS layer.
pub trait Layer: Filesystem {
    /// Return the root inode number
    fn root_inode(&self) -> Inode;
    /// Create whiteout file with name <name>.
    ///
    /// If this call is successful then the lookup count of the `Inode` associated with the returned
    /// `Entry` must be increased by 1.
    async fn create_whiteout(
        &self,
        ctx: Request,
        parent: Inode,
        name: &OsStr,
    ) -> Result<ReplyEntry> {
        // Use temp value to avoid moved 'parent'.
        let ino: u64 = parent;
        match self.lookup(ctx, ino, name).await {
            Ok(v) => {
                // Find whiteout char dev.
                if is_whiteout(&v.attr) {
                    return Ok(v);
                }
                // Non-negative entry with inode larger than 0 indicates file exists.
                if v.attr.ino != 0 {
                    // Decrease the refcount.
                    self.forget(ctx, v.attr.ino, 1).await;
                    // File exists with same name, create whiteout file is not allowed.
                    return Err(Error::from_raw_os_error(libc::EEXIST).into());
                }
            }
            Err(e) => {
                let e: std::io::Error = e.into();
                match e.raw_os_error() {
                    Some(raw_error) => {
                        // We expect ENOENT error.
                        if raw_error != libc::ENOENT {
                            return Err(e.into());
                        }
                    }
                    None => return Err(e.into()),
                }
            }
        }

        // Try to create whiteout char device with 0/0 device number.
        let dev = libc::makedev(0, 0);
        let mode = libc::S_IFCHR | 0o777;
        self.mknod(ctx, ino, name, mode, dev as u32).await
    }

    /// Delete whiteout file with name <name>.
    async fn delete_whiteout(&self, ctx: Request, parent: Inode, name: &OsStr) -> Result<()> {
        // Use temp value to avoid moved 'parent'.
        let ino: u64 = parent;
        match self.lookup(ctx, ino, name).await {
            Ok(v) => {
                if v.attr.ino != 0 {
                    // Decrease the refcount since we make a lookup call.
                    self.forget(ctx, v.attr.ino, 1).await;
                }

                // Find whiteout so we can safely delete it.
                if is_whiteout(&v.attr) {
                    return self.unlink(ctx, ino, name).await;
                }
                //  Non-negative entry with inode larger than 0 indicates file exists.
                if v.attr.ino != 0 {
                    // File exists but not whiteout file.
                    return Err(Error::from_raw_os_error(libc::EINVAL).into());
                }
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Check if the Inode is a whiteout file
    async fn is_whiteout(&self, ctx: Request, inode: Inode) -> Result<bool> {
        let rep = self.getattr(ctx, inode, None, 0).await?;

        // Check attributes of the inode to see if it's a whiteout char device.
        Ok(is_whiteout(&rep.attr))
    }

    /// Set the directory to opaque.
    async fn set_opaque(&self, ctx: Request, inode: Inode) -> Result<()> {
        // Use temp value to avoid moved 'parent'.
        let ino: u64 = inode;

        // Get attributes and check if it's directory.
        let rep = self.getattr(ctx, ino, None, 0).await?;
        if !is_dir(&rep.attr) {
            // Only directory can be set to opaque.
            return Err(Error::from_raw_os_error(libc::ENOTDIR).into());
        }
        // A directory is made opaque by setting the xattr "trusted.overlay.opaque" to "y".
        // See ref: https://docs.kernel.org/filesystems/overlayfs.html#whiteouts-and-opaque-directories
        self.setxattr(ctx, ino, OsStr::new(OPAQUE_XATTR), b"y", 0, 0)
            .await
    }

    /// Check if the directory is opaque.
    async fn is_opaque(&self, ctx: Request, inode: Inode) -> Result<bool> {
        // Use temp value to avoid moved 'parent'.
        let ino: u64 = inode;

        // Get attributes of the directory.
        let attr: rfuse3::raw::prelude::ReplyAttr = self.getattr(ctx, ino, None, 0).await?;
        if !is_dir(&attr.attr) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR).into());
        }

        // Return Result<is_opaque>.
        let check_attr = |inode: Inode, attr_name: &'static str, attr_size: u32| async move {
            let cname = OsStr::new(attr_name);
            match self.getxattr(ctx, inode, cname, attr_size).await {
                Ok(v) => {
                    // xattr name exists and we get value.
                    if let ReplyXAttr::Data(bufs) = v
                        && bufs.len() == 1
                        && bufs[0].eq_ignore_ascii_case(&b'y')
                    {
                        return Ok(true);
                    }
                    // No value found, go on to next check.
                    Ok(false)
                }
                Err(e) => {
                    let ioerror: std::io::Error = e.into();
                    if let Some(raw_error) = ioerror.raw_os_error()
                        && raw_error == libc::ENODATA
                    {
                        return Ok(false);
                    }

                    Err(e)
                }
            }
        };

        // A directory is made opaque by setting some specific xattr to "y".
        // See ref: https://docs.kernel.org/filesystems/overlayfs.html#whiteouts-and-opaque-directories

        // Check our customized version of the xattr "user.fuseoverlayfs.opaque".
        let is_opaque = check_attr(ino, OPAQUE_XATTR, OPAQUE_XATTR_LEN).await?;
        if is_opaque {
            return Ok(true);
        }

        // Also check for the unprivileged version of the xattr "trusted.overlay.opaque".
        let is_opaque = check_attr(ino, PRIVILEGED_OPAQUE_XATTR, OPAQUE_XATTR_LEN).await?;
        if is_opaque {
            return Ok(true);
        }

        // Also check for the unprivileged version of the xattr "user.overlay.opaque".
        let is_opaque = check_attr(ino, UNPRIVILEGED_OPAQUE_XATTR, OPAQUE_XATTR_LEN).await?;
        if is_opaque {
            return Ok(true);
        }

        Ok(false)
    }
}
impl Layer for PassthroughFs {
    fn root_inode(&self) -> Inode {
        1
    }
}
pub(crate) fn is_dir(st: &FileAttr) -> bool {
    st.kind.const_into_mode_t() & libc::S_IFMT == libc::S_IFDIR
}

pub(crate) fn is_chardev(st: &FileAttr) -> bool {
    st.kind.const_into_mode_t() & libc::S_IFMT == libc::S_IFCHR
}

pub(crate) fn is_whiteout(st: &FileAttr) -> bool {
    // A whiteout is created as a character device with 0/0 device number.
    // See ref: https://docs.kernel.org/filesystems/overlayfs.html#whiteouts-and-opaque-directories
    let major = libc::major(st.rdev.into());
    let minor = libc::minor(st.rdev.into());
    is_chardev(st) && major == 0 && minor == 0
}

#[cfg(test)]
mod test {
    use std::{ffi::OsStr, path::PathBuf};

    use rfuse3::raw::{Filesystem as _, Request};

    use crate::{
        overlayfs::layer::Layer, passthrough::new_passthroughfs_layer, unwrap_or_skip_eperm,
    };

    // Mark as ignored by default; run with: RUN_PRIVILEGED_TESTS=1 cargo test -- --ignored
    #[ignore]
    #[tokio::test]
    async fn test_whiteout_create_delete() {
        let temp_dir = "/tmp/test_whiteout/t2";
        let rootdir = PathBuf::from(temp_dir);
        std::fs::create_dir_all(&rootdir).unwrap();
        if std::env::var("RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skip test_whiteout_create_delete: RUN_PRIVILEGED_TESTS!=1");
            return;
        }
        let fs = unwrap_or_skip_eperm!(
            new_passthroughfs_layer(rootdir.to_str().unwrap()).await,
            "init passthrough layer"
        );
        let _ = unwrap_or_skip_eperm!(fs.init(Request::default()).await, "fs init");
        let white_name = OsStr::new(&"test");
        let res = unwrap_or_skip_eperm!(
            fs.create_whiteout(Request::default(), 1, white_name).await,
            "create whiteout"
        );

        print!("{res:?}");
        let res = fs.delete_whiteout(Request::default(), 1, white_name).await;
        if res.is_err() {
            panic!("{res:?}");
        }
        let _ = fs.destroy(Request::default()).await;
    }

    #[tokio::test]
    async fn test_is_opaque_on_non_directory() {
        let temp_dir = "/tmp/test_opaque_non_dir/t2";
        let rootdir = PathBuf::from(temp_dir);
        std::fs::create_dir_all(&rootdir).unwrap();
        if std::env::var("RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skip test_is_opaque_on_non_directory: RUN_PRIVILEGED_TESTS!=1");
            return;
        }
        let fs = unwrap_or_skip_eperm!(
            new_passthroughfs_layer(rootdir.to_str().unwrap()).await,
            "init passthrough layer"
        );
        let _ = unwrap_or_skip_eperm!(fs.init(Request::default()).await, "fs init");

        // Create a file
        let file_name = OsStr::new("not_a_dir");
        let _ = unwrap_or_skip_eperm!(
            fs.create(Request::default(), 1, file_name, 0o644, 0).await,
            "create file"
        );

        // Lookup to get the inode of the file
        let entry = unwrap_or_skip_eperm!(
            fs.lookup(Request::default(), 1, file_name).await,
            "lookup file"
        );
        let file_inode = entry.attr.ino;

        // is_opaque should return ENOTDIR error
        let res = fs.is_opaque(Request::default(), file_inode).await;
        assert!(res.is_err());
        let err = res.err().unwrap();
        let ioerr: std::io::Error = err.into();
        assert_eq!(ioerr.raw_os_error(), Some(libc::ENOTDIR));

        // Clean up
        let _ = fs.unlink(Request::default(), 1, file_name).await;
        let _ = fs.destroy(Request::default()).await;
    }

    #[tokio::test]
    async fn test_set_opaque_on_non_directory() {
        let temp_dir = "/tmp/test_set_opaque_non_dir/t2";
        let rootdir = PathBuf::from(temp_dir);
        std::fs::create_dir_all(&rootdir).unwrap();
        if std::env::var("RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skip test_set_opaque_on_non_directory: RUN_PRIVILEGED_TESTS!=1");
            return;
        }
        let fs = unwrap_or_skip_eperm!(
            new_passthroughfs_layer(rootdir.to_str().unwrap()).await,
            "init passthrough layer"
        );
        let _ = unwrap_or_skip_eperm!(fs.init(Request::default()).await, "fs init");

        // Create a file
        let file_name = OsStr::new("not_a_dir2");
        let _ = unwrap_or_skip_eperm!(
            fs.create(Request::default(), 1, file_name, 0o644, 0).await,
            "create file"
        );

        // Lookup to get the inode of the file
        let entry = unwrap_or_skip_eperm!(
            fs.lookup(Request::default(), 1, file_name).await,
            "lookup file"
        );
        let file_inode = entry.attr.ino;

        // set_opaque should return ENOTDIR error
        let res = fs.set_opaque(Request::default(), file_inode).await;
        assert!(res.is_err());
        let err = res.err().unwrap();
        let ioerr: std::io::Error = err.into();
        assert_eq!(ioerr.raw_os_error(), Some(libc::ENOTDIR));

        // Clean up
        let _ = fs.unlink(Request::default(), 1, file_name).await;
        let _ = fs.destroy(Request::default()).await;
    }
}
