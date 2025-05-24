use std::ffi::OsStr;

use bytes::Bytes;
use fuse3::notify::Notify;
use fuse3::raw::reply::*;
use fuse3::raw::{Filesystem, Request, reply::ReplyInit};
use fuse3::{Result, SetAttr};

use super::Inode;
use std::any::type_name_of_val;
// LoggingFileSystem . provide log info for a filesystem trait.
pub struct LoggingFileSystem<FS: Filesystem> {
    inner: FS,
    fsname: String,
}

impl<FS: Filesystem> LoggingFileSystem<FS> {
    pub fn new(fs: FS) -> Self {
        let fsname = type_name_of_val(&fs);
        Self {
            inner: fs,
            fsname: String::from(fsname),
        }
    }
}

impl<FS: fuse3::raw::Filesystem + std::marker::Sync> Filesystem for LoggingFileSystem<FS> {
    type DirEntryStream<'a>
        = FS::DirEntryStream<'a>
    where
        Self: 'a;

    type DirEntryPlusStream<'a>
        = FS::DirEntryPlusStream<'a>
    where
        Self: 'a;

    /// read directory entries, but with their attribute, like [`readdir`][Filesystem::readdir]
    /// + [`lookup`][Filesystem::lookup] at the same time.
    async fn readdirplus(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: u64,
        lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        println!(
            "fs:{}, [readdirplus]: parent: {:?}, fh: {}, offset: {}",
            self.fsname, parent, fh, offset
        );
        match self
            .inner
            .readdirplus(req, parent, fh, offset, lock_owner)
            .await
        {
            Ok(reply) => {
                // println!("readdirplus result:{:?}",reply.entries.);
                Ok(reply)
            }
            Err(e) => {
                println!("fs:{}, readdirplus error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// initialize filesystem. Called before any other filesystem method.
    async fn init(&self, req: Request) -> Result<ReplyInit> {
        println!("fs:{}, init ", self.fsname);
        match self.inner.init(req).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, init error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn destroy(&self, req: Request) {
        println!("fs:{}, destroy ", self.fsname);
        self.inner.destroy(req).await
    }

    async fn lookup(&self, req: Request, parent: Inode, name: &OsStr) -> Result<ReplyEntry> {
        println!(
            "fs:{}, lookup: parent: {:?}, name: {:?}",
            self.fsname, parent, name
        );
        match self.inner.lookup(req, parent, name).await {
            Ok(reply) => {
                println!("look up result :{:?}", reply);
                Ok(reply)
            }
            Err(e) => {
                println!("fs:{}, lookup error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn forget(&self, req: Request, inode: Inode, nlookup: u64) {
        println!(
            "fs:{}, forget: inode: {:?}, nlookup: {}",
            self.fsname, inode, nlookup
        );
        self.inner.forget(req, inode, nlookup).await
    }

    async fn getattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        flags: u32,
    ) -> Result<ReplyAttr> {
        println!(
            "fs:{}, getattr: inode: {:?}, fh: {:?}, flags: {}",
            self.fsname, inode, fh, flags
        );
        match self.inner.getattr(req, inode, fh, flags).await {
            Ok(reply) => {
                println!("getattr result :{:?}", reply);
                Ok(reply)
            }
            Err(e) => {
                println!("fs:{}, getattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn setattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        println!(
            "fs:{}, setattr: inode: {:?}, fh: {:?}, set_attr: {:?}",
            self.fsname, inode, fh, set_attr
        );
        match self.inner.setattr(req, inode, fh, set_attr).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, setattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn readlink(&self, req: Request, inode: Inode) -> Result<ReplyData> {
        println!("fs:{}, readlink: inode: {:?}", self.fsname, inode);
        match self.inner.readlink(req, inode).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, readlink error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn symlink(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        println!(
            "fs:{}, symlink: parent: {:?}, name: {:?}, link: {:?}",
            self.fsname, parent, name, link
        );
        match self.inner.symlink(req, parent, name, link).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, symlink error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn mknod(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        rdev: u32,
    ) -> Result<ReplyEntry> {
        println!(
            "fs:{}, mknod: parent: {:?}, name: {:?}, mode: {}, rdev: {}",
            self.fsname, parent, name, mode, rdev
        );
        match self.inner.mknod(req, parent, name, mode, rdev).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, mknod error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn mkdir(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<ReplyEntry> {
        println!(
            "fs:{}, mkdir: parent: {:?}, name: {:?}, mode: {}, umask: {}",
            self.fsname, parent, name, mode, umask
        );
        match self.inner.mkdir(req, parent, name, mode, umask).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, mkdir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn unlink(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        println!(
            "fs:{}, unlink: parent: {:?}, name: {:?}",
            self.fsname, parent, name
        );
        match self.inner.unlink(req, parent, name).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, unlink error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn rmdir(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        println!(
            "fs:{}, rmdir: parent: {:?}, name: {:?}",
            self.fsname, parent, name
        );
        match self.inner.rmdir(req, parent, name).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, rmdir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn rename(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        println!(
            "fs:{}, rename: parent: {:?}, name: {:?}, new_parent: {:?}, new_name: {:?}",
            self.fsname, parent, name, new_parent, new_name
        );
        match self
            .inner
            .rename(req, parent, name, new_parent, new_name)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, rename error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn link(
        &self,
        req: Request,
        inode: Inode,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        println!(
            "fs:{}, link: inode: {:?}, new_parent: {:?}, new_name: {:?}",
            self.fsname, inode, new_parent, new_name
        );
        match self.inner.link(req, inode, new_parent, new_name).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, link error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn open(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        println!(
            "fs:{}, open: inode: {:?}, flags: {}",
            self.fsname, inode, flags
        );
        match self.inner.open(req, inode, flags).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, open error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn read(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        println!(
            "fs:{}, read: inode: {:?}, fh: {}, offset: {}, size: {}",
            self.fsname, inode, fh, offset, size
        );
        match self.inner.read(req, inode, fh, offset, size).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, read error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

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
        println!(
            "fs:{}, write: inode: {:?}, fh: {}, offset: {}, size: {}",
            self.fsname,
            inode,
            fh,
            offset,
            data.len()
        );
        match self
            .inner
            .write(req, inode, fh, offset, data, write_flags, flags)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, write error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn statfs(&self, req: Request, inode: Inode) -> Result<ReplyStatFs> {
        println!("fs:{}, statfs: inode: {:?}", self.fsname, inode);
        match self.inner.statfs(req, inode).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, statfs error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn release(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
    ) -> Result<()> {
        println!(
            "fs:{}, release: inode: {:?}, fh: {}, flags: {}, lock_owner: {}, flush: {}",
            self.fsname, inode, fh, flags, lock_owner, flush
        );
        match self
            .inner
            .release(req, inode, fh, flags, lock_owner, flush)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, release error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn fsync(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        println!(
            "fs:{}, fsync: inode: {:?}, fh: {}, datasync: {}",
            self.fsname, inode, fh, datasync
        );
        match self.inner.fsync(req, inode, fh, datasync).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, fsync error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn setxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        value: &[u8],
        flags: u32,
        position: u32,
    ) -> Result<()> {
        println!(
            "fs:{}, setxattr: inode: {:?}, name: {:?}, value_size: {}, flags: {}",
            self.fsname,
            inode,
            name,
            value.len(),
            flags
        );
        match self
            .inner
            .setxattr(req, inode, name, value, flags, position)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, setxattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    async fn getxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        println!(
            "fs:{}, getxattr: inode: {:?}, name: {:?}, size: {}",
            self.fsname, inode, name, size
        );
        match self.inner.getxattr(req, inode, name, size).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, getxattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// List extended attribute names.
    async fn listxattr(&self, req: Request, inode: Inode, size: u32) -> Result<ReplyXAttr> {
        println!(
            "fs:{}, listxattr: inode: {:?}, size: {}",
            self.fsname, inode, size
        );
        match self.inner.listxattr(req, inode, size).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, listxattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// remove an extended attribute.
    async fn removexattr(&self, req: Request, inode: Inode, name: &OsStr) -> Result<()> {
        println!(
            "fs:{}, removexattr: inode: {:?}, name: {:?}",
            self.fsname, inode, name
        );
        match self.inner.removexattr(req, inode, name).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, removexattr error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// flush method. This is called on each `close()` of the opened file.
    async fn flush(&self, req: Request, inode: Inode, fh: u64, lock_owner: u64) -> Result<()> {
        println!(
            "fs:{}, flush: inode: {:?}, fh: {}, lock_owner: {}",
            self.fsname, inode, fh, lock_owner
        );
        match self.inner.flush(req, inode, fh, lock_owner).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, flush error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// open a directory.
    async fn opendir(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        match self.inner.opendir(req, inode, flags).await {
            Ok(reply) => {
                println!(
                    "fs:{}, opendir: inode: {:?}, flags: {} --- return fh:{}",
                    self.fsname, inode, flags, reply.fh
                );
                Ok(reply)
            }
            Err(e) => {
                println!("fs:{}, opendir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// read directory.
    async fn readdir(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: i64,
    ) -> Result<ReplyDirectory<Self::DirEntryStream<'_>>> {
        println!(
            "fs:{}, readdir: parent: {:?}, fh: {}, offset: {}",
            self.fsname, parent, fh, offset
        );
        match self.inner.readdir(req, parent, fh, offset).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, readdir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// release an open directory.
    async fn releasedir(&self, req: Request, inode: Inode, fh: u64, flags: u32) -> Result<()> {
        println!(
            "fs:{}, releasedir: inode: {:?}, fh: {}, flags: {}",
            self.fsname, inode, fh, flags
        );
        match self.inner.releasedir(req, inode, fh, flags).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, releasedir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// synchronize directory contents.
    async fn fsyncdir(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        println!(
            "fs:{}, fsyncdir: inode: {:?}, fh: {}, datasync: {}",
            self.fsname, inode, fh, datasync
        );
        match self.inner.fsyncdir(req, inode, fh, datasync).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, fsyncdir error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// check file access permissions. This will be called for the `access()` system call. If the
    /// `default_permissions` mount option is given, this method is not be called. This method is
    /// not called under Linux kernel versions 2.4.x.
    async fn access(&self, req: Request, inode: Inode, mask: u32) -> Result<()> {
        println!(
            "fs:{}, access: inode: {:?}, mask: {}",
            self.fsname, inode, mask
        );
        match self.inner.access(req, inode, mask).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, access error: {:?}", self.fsname, e);
                Err(e)
            }
        }
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
        println!(
            "fs:{}, create: parnet: {}; name :{},mode:{},flags:{}",
            self.fsname,
            parent,
            name.to_str().unwrap_or_default(),
            mode,
            flags
        );
        let reply = self.inner.create(req, parent, name, mode, flags).await?;
        Ok(reply)
    }

    /// handle interrupt. When a operation is interrupted, an interrupt request will send to fuse
    /// server with the unique id of the operation.
    async fn interrupt(&self, req: Request, unique: u64) -> Result<()> {
        println!("fs:{}, interrupt: unique: {}", self.fsname, unique);
        match self.inner.interrupt(req, unique).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, interrupt error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// map block index within file to block index within device.
    ///
    /// # Notes:
    ///
    /// This may not works because currently this crate doesn't support fuseblk mode yet.
    async fn bmap(
        &self,
        req: Request,
        inode: Inode,
        blocksize: u32,
        idx: u64,
    ) -> Result<ReplyBmap> {
        match self.inner.bmap(req, inode, blocksize, idx).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, bmap error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /*async fn ioctl(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        flags: u32,
        cmd: u32,
        arg: u64,
        in_size: u32,
        out_size: u32,
    ) -> Result<ReplyIoctl> {
        Err(libc::ENOSYS.into())
    }*/

    /// poll for IO readiness events.
    #[allow(clippy::too_many_arguments)]
    async fn poll(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        kh: Option<u64>,
        flags: u32,
        events: u32,
        notify: &Notify,
    ) -> Result<ReplyPoll> {
        match self
            .inner
            .poll(req, inode, fh, kh, flags, events, notify)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, poll error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// receive notify reply from kernel.
    async fn notify_reply(
        &self,
        req: Request,
        inode: Inode,
        offset: u64,
        data: Bytes,
    ) -> Result<()> {
        println!(
            "fs:{}, notify_reply: inode: {:?}, offset: {}, data: {:?}",
            self.fsname, inode, offset, data
        );
        match self.inner.notify_reply(req, inode, offset, data).await {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, notify_reply error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// forget more than one inode. This is a batch version [`forget`][Filesystem::forget]
    async fn batch_forget(&self, req: Request, inodes: &[(Inode, u64)]) {
        let _ = self.inner.batch_forget(req, inodes).await;
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
        println!(
            "fs:{}, fallocate: inode: {:?}, fh: {}, offset: {}, length: {}, mode: {}",
            self.fsname, inode, fh, offset, length, mode
        );
        match self
            .inner
            .fallocate(req, inode, fh, offset, length, mode)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, fallocate error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// rename a file or directory with flags.
    async fn rename2(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
        flags: u32,
    ) -> Result<()> {
        println!(
            "fs:{}, rename2: parent: {:?}, name: {:?}, new_parent: {:?}, new_name: {:?}, flags: {}",
            self.fsname, parent, name, new_parent, new_name, flags
        );

        match self
            .inner
            .rename2(req, parent, name, new_parent, new_name, flags)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                println!("fs:{}, rename2 error: {:?}", self.fsname, e);
                Err(e)
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
        println!(
            "fs:{}, lseek: inode: {:?}, fh: {}, offset: {}, whence: {}",
            self.fsname, inode, fh, offset, whence
        );
        match self.inner.lseek(req, inode, fh, offset, whence).await {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, lseek error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }

    /// copy a range of data from one file to another. This can improve performance because it
    /// reduce data copy: in normal, data will copy from FUSE server to kernel, then to user-space,
    /// then to kernel, finally send back to FUSE server. By implement this method, data will only
    /// copy in FUSE server internal.
    #[allow(clippy::too_many_arguments)]
    async fn copy_file_range(
        &self,
        req: Request,
        inode: Inode,
        fh_in: u64,
        off_in: u64,
        inode_out: Inode,
        fh_out: u64,
        off_out: u64,
        length: u64,
        flags: u64,
    ) -> Result<ReplyCopyFileRange> {
        println!(
            "fs:{}, copy_file_range: inode: {:?}, fh_in: {}, off_in: {}, inode_out: {:?}, fh_out: {}, off_out: {}, length: {}, flags: {}",
            self.fsname, inode, fh_in, off_in, inode_out, fh_out, off_out, length, flags
        );

        match self
            .inner
            .copy_file_range(
                req, inode, fh_in, off_in, inode_out, fh_out, off_out, length, flags,
            )
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => {
                println!("fs:{}, copy_file_range error: {:?}", self.fsname, e);
                Err(e)
            }
        }
    }
}
