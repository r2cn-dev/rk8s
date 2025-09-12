use crate::overlayfs::AtomicU64;
use crate::overlayfs::HandleData;
use crate::overlayfs::RealHandle;
use futures::stream::Iter;
use rfuse3::raw::prelude::*;
use rfuse3::*;
use std::ffi::OsStr;
use std::io::Error;
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::vec::IntoIter;

use super::Inode;
use super::OverlayFs;
use super::utils;
impl Filesystem for OverlayFs {
    /// initialize filesystem. Called before any other filesystem method.
    async fn init(&self, _req: Request) -> Result<ReplyInit> {
        if self.config.do_import {
            self.import().await?;
        }
        if !self.config.do_import || self.config.writeback {
            self.writeback.store(true, Ordering::Relaxed);
        }
        if !self.config.do_import || self.config.no_open {
            self.no_open.store(true, Ordering::Relaxed);
        }
        if !self.config.do_import || self.config.no_opendir {
            self.no_opendir.store(true, Ordering::Relaxed);
        }
        if !self.config.do_import || self.config.killpriv_v2 {
            self.killpriv_v2.store(true, Ordering::Relaxed);
        }
        if self.config.perfile_dax {
            self.perfile_dax.store(true, Ordering::Relaxed);
        }

        Ok(ReplyInit {
            max_write: NonZeroU32::new(128 * 1024).unwrap(),
        })
    }

    /// clean up filesystem. Called on filesystem exit which is fuseblk, in normal fuse filesystem,
    /// kernel may call forget for root. There is some discuss for this
    /// <https://github.com/bazil/fuse/issues/82#issuecomment-88126886>,
    /// <https://sourceforge.net/p/fuse/mailman/message/31995737/>
    async fn destroy(&self, _req: Request) {}

    /// look up a directory entry by name and get its attributes.
    async fn lookup(&self, req: Request, parent: Inode, name: &OsStr) -> Result<ReplyEntry> {
        let tmp = name.to_string_lossy().to_string();
        let result = self.do_lookup(req, parent, tmp.as_str()).await;
        match result {
            Ok(e) => Ok(e),
            Err(err) => Err(err.into()),
        }
    }

    /// forget an inode. The nlookup parameter indicates the number of lookups previously
    /// performed on this inode. If the filesystem implements inode lifetimes, it is recommended
    /// that inodes acquire a single reference on each lookup, and lose nlookup references on each
    /// forget. The filesystem may ignore forget calls, if the inodes don't need to have a limited
    /// lifetime. On unmount it is not guaranteed, that all referenced inodes will receive a forget
    /// message. When filesystem is normal(not fuseblk) and unmounting, kernel may send forget
    /// request for root and this library will stop session after call forget. There is some
    /// discussion for this <https://github.com/bazil/fuse/issues/82#issuecomment-88126886>,
    /// <https://sourceforge.net/p/fuse/mailman/message/31995737/>
    async fn forget(&self, _req: Request, inode: Inode, nlookup: u64) {
        self.forget_one(inode, nlookup).await;
    }

    /// get file attributes. If `fh` is None, means `fh` is not set.
    async fn getattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        flags: u32,
    ) -> Result<ReplyAttr> {
        if !self.no_open.load(Ordering::Relaxed)
            && let Some(h) = fh
        {
            let handles = self.handles.lock().await;
            if let Some(hd) = handles.get(&h)
                && let Some(ref rh) = hd.real_handle
            {
                let mut rep: ReplyAttr = rh
                    .layer
                    .getattr(req, rh.inode, Some(rh.handle.load(Ordering::Relaxed)), 0)
                    .await?;
                rep.attr.ino = inode;
                return Ok(rep);
            }
        }

        let node: Arc<super::OverlayInode> = self.lookup_node(req, inode, "").await?;
        let (layer, _, lower_inode) = node.first_layer_inode().await;
        let mut re = layer.getattr(req, lower_inode, None, flags).await?;
        re.attr.ino = inode;
        Ok(re)
    }

    /// set file attributes. If `fh` is None, means `fh` is not set.
    async fn setattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        // Check if upper layer exists.
        self.upper_layer
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::from_raw_os_error(libc::EROFS))?;

        // deal with handle first
        if !self.no_open.load(Ordering::Relaxed)
            && let Some(h) = fh
        {
            let handles = self.handles.lock().await;
            if let Some(hd) = handles.get(&h)
                && let Some(ref rhd) = hd.real_handle
            {
                // handle opened in upper layer
                if rhd.in_upper_layer {
                    let mut rep = rhd
                        .layer
                        .setattr(
                            req,
                            rhd.inode,
                            Some(rhd.handle.load(Ordering::Relaxed)),
                            set_attr,
                        )
                        .await?;
                    rep.attr.ino = inode;
                    return Ok(rep);
                }
            }
        }

        let mut node = self.lookup_node(req, inode, "").await?;

        if !node.in_upper_layer().await {
            node = self.copy_node_up(req, node.clone()).await?
        }

        let (layer, _, real_inode) = node.first_layer_inode().await;
        // layer.setattr(req, real_inode, None, set_attr).await
        let mut rep = layer.setattr(req, real_inode, None, set_attr).await?;
        rep.attr.ino = inode;
        Ok(rep)
    }

    /// read symbolic link.
    async fn readlink(&self, req: Request, inode: Inode) -> Result<ReplyData> {
        trace!("READLINK: inode: {inode}\n");

        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let (layer, _, inode) = node.first_layer_inode().await;
        layer.readlink(req, inode).await
    }

    /// create a symbolic link.
    async fn symlink(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        // soft link
        let sname = name.to_string_lossy().into_owned().to_owned();
        let slinkname = link.to_string_lossy().into_owned().to_owned();

        let pnode = self.lookup_node(req, parent, "").await?;
        self.do_symlink(req, slinkname.as_str(), &pnode, sname.as_str())
            .await?;

        self.do_lookup(req, parent, sname.as_str())
            .await
            .map_err(|e| e.into())
    }

    /// create file node. Create a regular file, character device, block device, fifo or socket
    /// node. When creating file, most cases user only need to implement
    /// [`create`][Filesystem::create].
    async fn mknod(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        rdev: u32,
    ) -> Result<ReplyEntry> {
        let sname = name.to_string_lossy().to_string();

        // Check if parent exists.
        let pnode = self.lookup_node(req, parent, "").await?;
        if pnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        self.do_mknod(req, &pnode, sname.as_str(), mode, rdev, 0)
            .await?;
        self.do_lookup(req, parent, sname.as_str())
            .await
            .map_err(|e| e.into())
    }

    /// create a directory.
    async fn mkdir(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<ReplyEntry> {
        let sname = name.to_string_lossy().to_string();

        // no entry or whiteout
        let pnode = self.lookup_node(req, parent, "").await?;
        if pnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        self.do_mkdir(req, pnode, sname.as_str(), mode, umask)
            .await?;
        self.do_lookup(req, parent, sname.as_str())
            .await
            .map_err(|e| e.into())
    }

    /// remove a file.
    async fn unlink(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        self.do_rm(req, parent, name, false)
            .await
            .map_err(|e| e.into())
    }

    /// remove a directory.
    async fn rmdir(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        self.do_rm(req, parent, name, true)
            .await
            .map_err(|e| e.into())
    }

    /// rename a file or directory.
    async fn rename(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        self.do_rename(req, parent, name, new_parent, new_name)
            .await
            .map_err(|e| e.into())
    }

    /// create a hard link.
    async fn link(
        &self,
        req: Request,
        inode: Inode,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let node = self.lookup_node(req, inode, "").await?;
        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let newpnode = self.lookup_node(req, new_parent, "").await?;
        if newpnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }
        let new_name = new_name.to_str().unwrap();
        // trace!(
        //     "LINK: inode: {}, new_parent: {}, trying to do_link: src_inode: {}, newpnode: {}",
        //     inode, new_parent, node.inode, newpnode.inode
        // );
        self.do_link(req, &node, &newpnode, new_name).await?;
        // trace!("LINK: done, looking up new entry");
        self.do_lookup(req, new_parent, new_name)
            .await
            .map_err(|e| e.into())
    }

    /// open a file. Open flags (with the exception of `O_CREAT`, `O_EXCL` and `O_NOCTTY`) are
    /// available in flags. Filesystem may store an arbitrary file handle (pointer, index, etc) in
    /// fh, and use this in other all other file operations (read, write, flush, release, fsync).
    /// Filesystem may also implement stateless file I/O and not store anything in fh. There are
    /// also some flags (`direct_io`, `keep_cache`) which the filesystem may set, to change the way
    /// the file is opened. A filesystem need not implement this method if it
    /// sets [`MountOptions::no_open_support`][crate::MountOptions::no_open_support] and if the
    /// kernel supports `FUSE_NO_OPEN_SUPPORT`.
    ///
    /// # Notes:
    ///
    /// See `fuse_file_info` structure in
    /// [fuse_common.h](https://libfuse.github.io/doxygen/include_2fuse__common_8h_source.html) for
    /// more details.
    async fn open(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        if self.no_open.load(Ordering::Relaxed) {
            info!("fuse: open is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOSYS).into());
        }

        let readonly: bool = flags
            & (libc::O_APPEND | libc::O_CREAT | libc::O_TRUNC | libc::O_RDWR | libc::O_WRONLY)
                as u32
            == 0;
        // toggle flags
        let mut flags: i32 = flags as i32;

        flags |= libc::O_NOFOLLOW;

        if self.config.writeback {
            if flags & libc::O_ACCMODE == libc::O_WRONLY {
                flags &= !libc::O_ACCMODE;
                flags |= libc::O_RDWR;
            }

            if flags & libc::O_APPEND != 0 {
                flags &= !libc::O_APPEND;
            }
        }
        // lookup node
        let node = self.lookup_node(req, inode, "").await?;

        // whiteout node
        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        if !readonly {
            // copy up to upper layer
            self.copy_node_up(req, node.clone()).await?;
        }

        // assign a handle in overlayfs and open it
        let (_l, h) = node.open(req, flags as u32, 0).await?;

        let hd = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let (layer, in_upper_layer, inode) = node.first_layer_inode().await;
        let handle_data = HandleData {
            node: node.clone(),
            real_handle: Some(RealHandle {
                layer,
                in_upper_layer,
                inode,
                handle: AtomicU64::new(h.fh),
            }),
        };

        self.handles.lock().await.insert(hd, Arc::new(handle_data));

        trace!("OPEN: returning handle: {hd}");

        Ok(ReplyOpen {
            fh: hd,
            flags: flags as u32,
        })
    }

    /// read data. Read should send exactly the number of bytes requested except on EOF or error,
    /// otherwise the rest of the data will be substituted with zeroes. An exception to this is
    /// when the file has been opened in `direct_io` mode, in which case the return value of the
    /// read system call will reflect the return value of this operation. `fh` will contain the
    /// value set by the open method, or will be undefined if the open method didn't set any value.
    async fn read(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        let data = self.get_data(req, Some(fh), inode, 0).await?;

        match data.real_handle {
            None => Err(Error::from_raw_os_error(libc::ENOENT).into()),
            Some(ref hd) => {
                hd.layer
                    .read(
                        req,
                        hd.inode,
                        hd.handle.load(Ordering::Relaxed),
                        offset,
                        size,
                    )
                    .await
            }
        }
    }

    /// write data. Write should return exactly the number of bytes requested except on error. An
    /// exception to this is when the file has been opened in `direct_io` mode, in which case the
    /// return value of the write system call will reflect the return value of this operation. `fh`
    /// will contain the value set by the open method, or will be undefined if the open method
    /// didn't set any value. When `write_flags` contains
    /// [`FUSE_WRITE_CACHE`](crate::raw::flags::FUSE_WRITE_CACHE), means the write operation is a
    /// delay write.
    #[allow(clippy::too_many_arguments)]
    async fn write(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        data: &[u8],
        write_flags: u32,
        flags: u32,
    ) -> Result<ReplyWrite> {
        let handle_data: Arc<HandleData> = self.get_data(req, Some(fh), inode, flags).await?;

        match handle_data.real_handle {
            None => Err(Error::from_raw_os_error(libc::ENOENT).into()),
            Some(ref hd) => {
                hd.layer
                    .write(
                        req,
                        hd.inode,
                        hd.handle.load(Ordering::Relaxed),
                        offset,
                        data,
                        write_flags,
                        flags,
                    )
                    .await
            }
        }
    }

    /// get filesystem statistics.
    async fn statfs(&self, req: Request, inode: Inode) -> Result<ReplyStatFs> {
        self.do_statvfs(req, inode).await.map_err(|e| e.into())
    }

    /// release an open file. Release is called when there are no more references to an open file:
    /// all file descriptors are closed and all memory mappings are unmapped. For every open call
    /// there will be exactly one release call. The filesystem may reply with an error, but error
    /// values are not returned to `close()` or `munmap()` which triggered the release. `fh` will
    /// contain the value set by the open method, or will be undefined if the open method didn't
    /// set any value. `flags` will contain the same flags as for open. `flush` means flush the
    /// data or not when closing file.
    async fn release(
        &self,
        req: Request,
        _inode: Inode,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
    ) -> Result<()> {
        if self.no_open.load(Ordering::Relaxed) {
            info!("fuse: release is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOSYS).into());
        }

        if let Some(hd) = self.handles.lock().await.get(&fh) {
            let rh = if let Some(ref h) = hd.real_handle {
                h
            } else {
                return Err(
                    Error::other(format!("no real handle found for file handle {fh}")).into(),
                );
            };
            let real_handle = rh.handle.load(Ordering::Relaxed);
            let real_inode = rh.inode;
            rh.layer
                .release(req, real_inode, real_handle, flags, lock_owner, flush)
                .await?;
        }

        self.handles.lock().await.remove(&fh);

        Ok(())
    }

    /// synchronize file contents. If the `datasync` is true, then only the user data should be
    /// flushed, not the metadata.
    async fn fsync(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        self.do_fsync(req, inode, datasync, fh, false)
            .await
            .map_err(|e| e.into())
    }

    /// set an extended attribute.
    async fn setxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        value: &[u8],
        flags: u32,
        position: u32,
    ) -> Result<()> {
        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        if !node.in_upper_layer().await {
            // Copy node up.
            self.copy_node_up(req, node.clone()).await?;
        }

        let (layer, _, real_inode) = node.first_layer_inode().await;

        layer
            .setxattr(req, real_inode, name, value, flags, position)
            .await
    }

    /// Get an extended attribute. If `size` is too small, return `Err<ERANGE>`.
    /// Otherwise, use [`ReplyXAttr::Data`] to send the attribute data, or
    /// return an error.
    async fn getxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let (layer, real_inode) = self.find_real_inode(inode).await?;

        layer.getxattr(req, real_inode, name, size).await
    }

    /// List extended attribute names.
    ///
    /// If `size` is too small, return `Err<ERANGE>`.  Otherwise, use
    /// [`ReplyXAttr::Data`] to send the attribute list, or return an error.
    async fn listxattr(&self, req: Request, inode: Inode, size: u32) -> Result<ReplyXAttr> {
        let node = self.lookup_node(req, inode, "").await?;
        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }
        let (layer, real_inode) = self.find_real_inode(inode).await?;
        layer.listxattr(req, real_inode, size).await
    }

    /// remove an extended attribute.
    async fn removexattr(&self, req: Request, inode: Inode, name: &OsStr) -> Result<()> {
        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        if !node.in_upper_layer().await {
            // copy node into upper layer
            self.copy_node_up(req, node.clone()).await?;
        }

        let (layer, _, ino) = node.first_layer_inode().await;
        layer.removexattr(req, ino, name).await

        // TODO: recreate the node since removexattr may remove the opaque xattr.
    }

    /// flush method. This is called on each `close()` of the opened file. Since file descriptors
    /// can be duplicated (`dup`, `dup2`, `fork`), for one open call there may be many flush calls.
    /// Filesystems shouldn't assume that flush will always be called after some writes, or that if
    /// will be called at all. `fh` will contain the value set by the open method, or will be
    /// undefined if the open method didn't set any value.
    ///
    /// # Notes:
    ///
    /// the name of the method is misleading, since (unlike fsync) the filesystem is not forced to
    /// flush pending writes. One reason to flush data, is if the filesystem wants to return write
    /// errors. If the filesystem supports file locking operations ([`setlk`][Filesystem::setlk],
    /// [`getlk`][Filesystem::getlk]) it should remove all locks belonging to `lock_owner`.
    async fn flush(&self, req: Request, inode: Inode, fh: u64, lock_owner: u64) -> Result<()> {
        if self.no_open.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOSYS).into());
        }

        let node = self.lookup_node(req, inode, "").await;
        match node {
            Ok(n) => {
                if n.whiteout.load(Ordering::Relaxed) {
                    return Err(Error::from_raw_os_error(libc::ENOENT).into());
                }
            }
            Err(e) => {
                if e.raw_os_error() == Some(libc::ENOENT) {
                    trace!("flush: inode {inode} is stale");
                } else {
                    return Err(e.into());
                }
            }
        }

        let (layer, real_inode, real_handle) = self.find_real_info_from_handle(fh).await?;

        // FIXME: need to test if inode matches corresponding handle?
        if inode
            != self
                .handles
                .lock()
                .await
                .get(&fh)
                .map(|h| h.node.inode)
                .unwrap_or(0)
        {
            return Err(Error::other("inode does not match handle").into());
        }

        trace!("flushing, real_inode: {real_inode}, real_handle: {real_handle}");
        layer.flush(req, real_inode, real_handle, lock_owner).await
    }

    /// open a directory. Filesystem may store an arbitrary file handle (pointer, index, etc) in
    /// `fh`, and use this in other all other directory stream operations
    /// ([`readdir`][Filesystem::readdir], [`releasedir`][Filesystem::releasedir],
    /// [`fsyncdir`][Filesystem::fsyncdir]). Filesystem may also implement stateless directory
    /// I/O and not store anything in `fh`.  A file system need not implement this method if it
    /// sets [`MountOptions::no_open_dir_support`][crate::MountOptions::no_open_dir_support] and
    /// if the kernel supports `FUSE_NO_OPENDIR_SUPPORT`.
    async fn opendir(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        if self.no_opendir.load(Ordering::Relaxed) {
            info!("fuse: opendir is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOSYS).into());
        }

        // lookup node
        let node = self.lookup_node(req, inode, ".").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let st = node.stat64(req).await?;
        if !utils::is_dir(&st.attr.kind) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR).into());
        }

        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        // Get the layer information and open directory in the underlying layer
        let (layer, in_upper_layer, real_inode) = node.first_layer_inode().await;
        let reply = layer.opendir(req, real_inode, flags).await?;

        self.handles.lock().await.insert(
            handle,
            Arc::new(HandleData {
                node: Arc::clone(&node),
                real_handle: Some(RealHandle {
                    layer,
                    in_upper_layer,
                    inode: real_inode,
                    handle: AtomicU64::new(reply.fh),
                }),
            }),
        );

        Ok(ReplyOpen { fh: handle, flags })
    }

    /// dir entry stream given by [`readdir`][Filesystem::readdir].
    type DirEntryStream<'a>
        = Iter<IntoIter<Result<DirectoryEntry>>>
    where
        Self: 'a;
    /// dir entry stream given by [`readdir`][Filesystem::readdir].
    type DirEntryPlusStream<'a>
        = Iter<IntoIter<Result<DirectoryEntryPlus>>>
    where
        Self: 'a;

    /// read directory. `offset` is used to track the offset of the directory entries. `fh` will
    /// contain the value set by the [`opendir`][Filesystem::opendir] method, or will be
    /// undefined if the [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn readdir(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: i64,
    ) -> Result<ReplyDirectory<Self::DirEntryStream<'_>>> {
        if self.config.no_readdir {
            info!("fuse: readdir is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOTDIR).into());
        }
        let entries = self
            .do_readdir(req, parent, fh, offset.try_into().unwrap())
            .await?;
        Ok(ReplyDirectory { entries })
    }

    /// read directory entries, but with their attribute, like [`readdir`][Filesystem::readdir]
    /// + [`lookup`][Filesystem::lookup] at the same time.
    async fn readdirplus(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: u64,
        _lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        if self.config.no_readdir {
            info!("fuse: readdir is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOTDIR).into());
        }
        trace!("readdirplus: parent: {parent}, fh: {fh}, offset: {offset}");
        let entries = self.do_readdirplus(req, parent, fh, offset).await?;
        match self.handles.lock().await.get(&fh) {
            Some(h) => {
                trace!(
                    "after readdirplus: found handle, seeing real_handle: {}",
                    h.real_handle.is_some()
                );
            }
            None => trace!("after readdirplus: no handle found: {fh}"),
        }
        Ok(ReplyDirectoryPlus { entries })
    }
    /// release an open directory. For every [`opendir`][Filesystem::opendir] call there will
    /// be exactly one `releasedir` call. `fh` will contain the value set by the
    /// [`opendir`][Filesystem::opendir] method, or will be undefined if the
    /// [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn releasedir(&self, req: Request, _inode: Inode, fh: u64, flags: u32) -> Result<()> {
        if self.no_opendir.load(Ordering::Relaxed) {
            info!("fuse: releasedir is not supported.");
            return Err(Error::from_raw_os_error(libc::ENOSYS).into());
        }

        if let Some(hd) = self.handles.lock().await.get(&fh) {
            let rh = if let Some(ref h) = hd.real_handle {
                h
            } else {
                return Err(
                    Error::other(format!("no real handle found for file handle {fh}")).into(),
                );
            };
            let real_handle = rh.handle.load(Ordering::Relaxed);
            let real_inode = rh.inode;
            rh.layer
                .releasedir(req, real_inode, real_handle, flags)
                .await?;
        }

        self.handles.lock().await.remove(&fh);
        Ok(())
    }

    /// synchronize directory contents. If the `datasync` is true, then only the directory contents
    /// should be flushed, not the metadata. `fh` will contain the value set by the
    /// [`opendir`][Filesystem::opendir] method, or will be undefined if the
    /// [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn fsyncdir(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        self.do_fsync(req, inode, datasync, fh, true)
            .await
            .map_err(|e| e.into())
    }
    /// check file access permissions. This will be called for the `access()` system call. If the
    /// `default_permissions` mount option is given, this method is not be called. This method is
    /// not called under Linux kernel versions 2.4.x.
    async fn access(&self, req: Request, inode: Inode, mask: u32) -> Result<()> {
        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let (layer, real_inode) = self.find_real_inode(inode).await?;
        layer.access(req, real_inode, mask).await
    }

    /// create and open a file. If the file does not exist, first create it with the specified
    /// mode, and then open it. Open flags (with the exception of `O_NOCTTY`) are available in
    /// flags. Filesystem may store an arbitrary file handle (pointer, index, etc) in `fh`, and use
    /// this in other all other file operations ([`read`][Filesystem::read],
    /// [`write`][Filesystem::write], [`flush`][Filesystem::flush],
    /// [`release`][Filesystem::release], [`fsync`][Filesystem::fsync]). There are also some flags
    /// (`direct_io`, `keep_cache`) which the filesystem may set, to change the way the file is
    /// opened. If this method is not implemented or under Linux kernel versions earlier than
    /// 2.6.15, the [`mknod`][Filesystem::mknod] and [`open`][Filesystem::open] methods will be
    /// called instead.
    ///
    /// # Notes:
    ///
    /// See `fuse_file_info` structure in
    /// [fuse_common.h](https://libfuse.github.io/doxygen/include_2fuse__common_8h_source.html) for
    /// more details.
    async fn create(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        flags: u32,
    ) -> Result<ReplyCreated> {
        // Parent doesn't exist.
        let pnode = self.lookup_node(req, parent, "").await?;
        if pnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let mut flags: i32 = flags as i32;
        flags |= libc::O_NOFOLLOW;
        flags &= !libc::O_DIRECT;
        if self.config.writeback {
            if flags & libc::O_ACCMODE == libc::O_WRONLY {
                flags &= !libc::O_ACCMODE;
                flags |= libc::O_RDWR;
            }

            if flags & libc::O_APPEND != 0 {
                flags &= !libc::O_APPEND;
            }
        }

        let final_handle = self
            .do_create(req, &pnode, name, mode, flags.try_into().unwrap())
            .await?;
        let entry = self.do_lookup(req, parent, name.to_str().unwrap()).await?;
        let fh = final_handle
            .ok_or_else(|| std::io::Error::new(ErrorKind::NotFound, "Handle not found"))?;
        Ok(ReplyCreated {
            ttl: entry.ttl,
            attr: entry.attr,
            generation: entry.generation,
            fh,
            flags: flags.try_into().unwrap(),
        })
    }

    /// forget more than one inode. This is a batch version [`forget`][Filesystem::forget]
    async fn batch_forget(&self, _req: Request, inodes: &[(Inode, u64)]) {
        for inode in inodes {
            self.forget_one(inode.0, inode.1).await;
        }
    }

    /// allocate space for an open file. This function ensures that required space is allocated for
    /// specified file.
    ///
    /// # Notes:
    ///
    /// more information about `fallocate`, please see **`man 2 fallocate`**
    async fn fallocate(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        length: u64,
        mode: u32,
    ) -> Result<()> {
        // Use O_RDONLY flags which indicates no copy up.
        let data = self
            .get_data(req, Some(fh), inode, libc::O_RDONLY as u32)
            .await?;

        match data.real_handle {
            None => Err(Error::from_raw_os_error(libc::ENOENT).into()),
            Some(ref rhd) => {
                if !rhd.in_upper_layer {
                    // TODO: in lower layer, error out or just success?
                    return Err(Error::from_raw_os_error(libc::EROFS).into());
                }
                rhd.layer
                    .fallocate(
                        req,
                        rhd.inode,
                        rhd.handle.load(Ordering::Relaxed),
                        offset,
                        length,
                        mode,
                    )
                    .await
            }
        }
    }

    /// find next data or hole after the specified offset.
    async fn lseek(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        whence: u32,
    ) -> Result<ReplyLSeek> {
        // can this be on dir? FIXME: assume file for now
        // we need special process if it can be called on dir
        let node = self.lookup_node(req, inode, "").await?;

        if node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT).into());
        }

        let st = node.stat64(req).await?;
        if utils::is_dir(&st.attr.kind) {
            error!("lseek on directory");
            return Err(Error::from_raw_os_error(libc::EINVAL).into());
        }

        let (layer, real_inode, real_handle) = self.find_real_info_from_handle(fh).await?;
        layer
            .lseek(req, real_inode, real_handle, offset, whence)
            .await
    }

    async fn interrupt(&self, _req: Request, _unique: u64) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, sync::Arc};

    use rfuse3::{MountOptions, raw::Session};
    use tokio::signal;

    use crate::{
        overlayfs::{OverlayFs, config::Config},
        passthrough::{new_passthroughfs_layer, newlogfs::LoggingFileSystem},
    };

    #[tokio::test]
    #[ignore]
    async fn test_a_ovlfs() {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Trace)
            .init();
        // Set up test environment
        let mountpoint = "/home/luxian/megatest/true_temp".to_string();
        let lowerdir = vec!["/home/luxian/github/buck2-rust-third-party".to_string()];
        let upperdir = "/home/luxian/upper".to_string();

        // Create lower layers
        let mut lower_layers = Vec::new();
        for lower in &lowerdir {
            let layer = new_passthroughfs_layer(lower).await.unwrap();
            lower_layers.push(Arc::new(layer));
        }
        // Create upper layer
        let upper_layer = Arc::new(new_passthroughfs_layer(&upperdir).await.unwrap());
        // Create overlayfs
        let config = Config {
            mountpoint: mountpoint.clone(),
            do_import: true,
            ..Default::default()
        };

        let overlayfs = OverlayFs::new(Some(upper_layer), lower_layers, config, 1).unwrap();

        let logfs = LoggingFileSystem::new(overlayfs);

        let mount_path: OsString = OsString::from(mountpoint);

        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let not_unprivileged = false;

        let mut mount_options = MountOptions::default();
        // .allow_other(true)
        mount_options.force_readdir_plus(true).uid(uid).gid(gid);

        let mut mount_handle: rfuse3::raw::MountHandle = if !not_unprivileged {
            Session::new(mount_options)
                .mount_with_unprivileged(logfs, mount_path)
                .await
                .unwrap()
        } else {
            Session::new(mount_options)
                .mount(logfs, mount_path)
                .await
                .unwrap()
        };

        let handle = &mut mount_handle;

        tokio::select! {
            res = handle => res.unwrap(),
            _ = signal::ctrl_c() => {
                mount_handle.unmount().await.unwrap()
            }
        }
    }
}
