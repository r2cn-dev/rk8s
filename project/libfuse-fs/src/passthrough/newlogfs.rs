use std::any::type_name_of_val;
use std::ffi::OsStr;

use super::Inode;
use bytes::Bytes;
use fuse3::notify::Notify;
use fuse3::raw::reply::*;
use fuse3::raw::{Filesystem, Request, reply::ReplyInit};
use fuse3::{Result, SetAttr};
use uuid::Uuid;
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
impl<FS: Filesystem> LoggingFileSystem<FS> {
    fn log_start(&self, req: &Request,id: &Uuid, method: &str, args: &[(&str, String)]) {
        let args_str = args
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(", ");
        println!("ID:{}|[{}] REQ{:?}-  - Call: {}",id, method, req, args_str);
    }

    fn log_result(&self, id: &Uuid, method: &str, result: &Result<impl std::fmt::Debug>) {
        match result {
            Ok(res) => println!("ID:{} [{}] - Success: {:?}", id, method, res),
            Err(e) => println!("ID:{} [{}] - Error: {:?}", id, method, e),
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

    async fn init(&self, req: Request) -> Result<ReplyInit> {
        let uuid = Uuid::new_v4();
        let method = "init";
        self.log_start(&req, &uuid, method, &[]);
        let result = self.inner.init(req).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn destroy(&self, req: Request) {
        let uuid = Uuid::new_v4();
        let method = "destroy";
        self.log_start(&req,&uuid, method, &[]);
        self.inner.destroy(req).await;
        println!("ID:{} [{}] {} - Completed", uuid, self.fsname, method);
    }

    async fn lookup(&self, req: Request, parent: Inode, name: &OsStr) -> Result<ReplyEntry> {
        let uuid = Uuid::new_v4();
        let method = "lookup";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.lookup(req, parent, name).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn forget(&self, req: Request, inode: Inode, nlookup: u64) {
        let uuid = Uuid::new_v4();
        let method = "forget";
        let args = vec![
            ("inode", inode.to_string()),
            ("nlookup", nlookup.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        self.inner.forget(req, inode, nlookup).await;
        println!("ID:{} [{}] {} - Completed", uuid, self.fsname, method);
    }

    async fn getattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        flags: u32,
    ) -> Result<ReplyAttr> {
        let uuid = Uuid::new_v4();
        let method = "getattr";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.map(|v| v.to_string()).unwrap_or_default()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.getattr(req, inode, fh, flags).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn setattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        let uuid = Uuid::new_v4();
        let method = "setattr";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.map(|v| v.to_string()).unwrap_or_default()),
            ("set_attr", format!("{:?}", set_attr)),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.setattr(req, inode, fh, set_attr).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn readdirplus(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: u64,
        lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        let uuid = Uuid::new_v4();
        let method = "readdirplus";
        let args = vec![
            ("parent", parent.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
            ("lock_owner", lock_owner.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .readdirplus(req, parent, fh, offset, lock_owner)
            .await;
        self.log_result(&uuid, method, &Ok("".to_string()));
        result
    }

    async fn opendir(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        let uuid = Uuid::new_v4();
        let method = "opendir";
        let args = vec![("inode", inode.to_string()), ("flags", flags.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.opendir(req, inode, flags).await;
        if let Ok(ref reply) = result {
            println!(
                "ID:{} [{}] {} - Obtained fh: {}",
                uuid, self.fsname, method, reply.fh
            );
        }
        self.log_result(&uuid, method, &result);
        result
    }

    async fn readdir(
        &self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: i64,
    ) -> Result<ReplyDirectory<Self::DirEntryStream<'_>>> {
        let uuid = Uuid::new_v4();
        let method = "readdir";
        let args = vec![
            ("parent", parent.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.readdir(req, parent, fh, offset).await;
        self.log_result(&uuid, method, &Ok("".to_string()));
        result
    }

    async fn read(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        let uuid = Uuid::new_v4();
        let method = "read";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
            ("size", size.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.read(req, inode, fh, offset, size).await;
        if let Ok(ref data) = result {
            println!(
                "ID:{} [{}] {} - Read {} bytes",
                uuid,
                self.fsname,
                method,
                data.data.len()
            );
        }
        
        //self.log_result(&uuid, method, &result);
        result
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
        let uuid = Uuid::new_v4();
        let method = "write";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
            ("data_len", data.len().to_string()),
            ("write_flags", write_flags.to_string()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .write(req, inode, fh, offset, data, write_flags, flags)
            .await;
        if let Ok(ref reply) = result {
            println!(
                "ID:{} [{}] {} - Wrote {} bytes",
                uuid, self.fsname, method, reply.written
            );
        }
        self.log_result(&uuid, method, &result);
        result
    }

    async fn fsync(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "fsync";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("datasync", datasync.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.fsync(req, inode, fh, datasync).await;
        self.log_result(&uuid, method, &result);
        result
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
        let uuid = Uuid::new_v4();
        let method = "setxattr";
        let args = vec![
            ("inode", inode.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("value_len", value.len().to_string()),
            ("flags", flags.to_string()),
            ("position", position.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .setxattr(req, inode, name, value, flags, position)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn rename2(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
        flags: u32,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "rename2";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("new_parent", new_parent.to_string()),
            ("new_name", new_name.to_string_lossy().into_owned()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .rename2(req, parent, name, new_parent, new_name, flags)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn unlink(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "unlink";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let re = self.inner.unlink(req, parent, name).await;
        self.log_result(&uuid, method, &re);
        re
    }
    async fn mkdir(
        &self,
        req: Request,
        parent: fuse3::Inode,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<ReplyEntry> {
        let uuid = Uuid::new_v4();
        let method = "mkdir";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("mode", mode.to_string()),
            ("umask", umask.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let re = self.inner.mkdir(req, parent, name, mode, umask).await;
        self.log_result(&uuid, method, &re);
        re
    }
    async fn access(&self, req: Request, inode: fuse3::Inode, mask: u32) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "access";
        let args = vec![("inode", inode.to_string()), ("mask", mask.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.access(req, inode, mask).await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn getxattr(
        &self,
        req: Request,
        inode: fuse3::Inode,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        let uuid = Uuid::new_v4();
        let method = "getxattr";
        let args = vec![
            ("inode", inode.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("size", size.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.getxattr(req, inode, name, size).await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn create(
        &self,
        req: Request,
        parent: fuse3::Inode,
        name: &OsStr,
        mode: u32,
        flags: u32,
    ) -> Result<ReplyCreated> {
        let uuid = Uuid::new_v4();
        let method = "create";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("mode", mode.to_string()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.create(req, parent, name, mode, flags).await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn lseek(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        offset: u64,
        whence: u32,
    ) -> Result<ReplyLSeek> {
        let uuid = Uuid::new_v4();
        let method = "lseek";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
            ("whence", whence.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.lseek(req, inode, fh, offset, whence).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn mknod(
        &self,
        req: Request,
        parent: fuse3::Inode,
        name: &OsStr,
        mode: u32,
        rdev: u32,
    ) -> Result<ReplyEntry> {
        let uuid = Uuid::new_v4();
        let method = "mknod";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("mode", mode.to_string()),
            ("rdev", rdev.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.mknod(req, parent, name, mode, rdev).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn rename(
        &self,
        req: Request,
        parent: fuse3::Inode,
        name: &OsStr,
        new_parent: fuse3::Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "rename";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("new_parent", new_parent.to_string()),
            ("new_name", new_name.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .rename(req, parent, name, new_parent, new_name)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn listxattr(&self, req: Request, inode: fuse3::Inode, size: u32) -> Result<ReplyXAttr> {
        let uuid = Uuid::new_v4();
        let method = "listxattr";
        let args = vec![("inode", inode.to_string()), ("size", size.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.listxattr(req, inode, size).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn open(&self, req: Request, inode: fuse3::Inode, flags: u32) -> Result<ReplyOpen> {
        let uuid = Uuid::new_v4();
        let method = "open";
        let args = vec![("inode", inode.to_string()), ("flags", flags.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.open(req, inode, flags).await;
        if let Ok(ref reply) = result {
            println!(
                "ID:{} [{}] {} - Obtained fh: {}",
                uuid, self.fsname, method, reply.fh
            );
        }
        self.log_result(&uuid, method, &result);
        result
    }

    async fn rmdir(&self, req: Request, parent: fuse3::Inode, name: &OsStr) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "rmdir";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.rmdir(req, parent, name).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn statfs(&self, req: Request, inode: fuse3::Inode) -> Result<ReplyStatFs> {
        let uuid = Uuid::new_v4();
        let method = "statfs";
        let args = vec![("inode", inode.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.statfs(req, inode).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn link(
        &self,
        req: Request,
        inode: fuse3::Inode,
        new_parent: fuse3::Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let uuid = Uuid::new_v4();
        let method = "link";
        let args = vec![
            ("inode", inode.to_string()),
            ("new_parent", new_parent.to_string()),
            ("new_name", new_name.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.link(req, inode, new_parent, new_name).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn symlink(
        &self,
        req: Request,
        parent: fuse3::Inode,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        let uuid = Uuid::new_v4();
        let method = "symlink";
        let args = vec![
            ("parent", parent.to_string()),
            ("name", name.to_string_lossy().into_owned()),
            ("link", link.to_string_lossy().into_owned()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.symlink(req, parent, name, link).await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn batch_forget(&self, req: Request, inodes: &[(Inode,u64)]) {
        let uuid = Uuid::new_v4();
        let method = "batch_forget";
        let args = vec![(
            "inodes",
            inodes
                .iter()
                .map(|inode| inode.0.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )];
        self.log_start(&req,&uuid, method, &args);
        self.inner.batch_forget(req, inodes).await;
        self.log_result(&uuid, method, &Ok("".to_string()));
    }
    async fn bmap(
        &self,
        req: Request,
        inode: fuse3::Inode,
        blocksize: u32,
        idx: u64,
    ) -> Result<ReplyBmap> {
        let uuid = Uuid::new_v4();
        let method = "bmap";
        let args = vec![
            ("inode", inode.to_string()),
            ("blocksize", blocksize.to_string()),
            ("idx", idx.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.bmap(req, inode, blocksize, idx).await;
        self.log_result(&uuid, method, &result);
        result
    }
    async fn copy_file_range(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh_in: u64,
        off_in: u64,
        inode_out: fuse3::Inode,
        fh_out: u64,
        off_out: u64,
        length: u64,
        flags: u64,
    ) -> Result<ReplyCopyFileRange> {
        let uuid = Uuid::new_v4();
        let method = "copy_file_range";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh_in", fh_in.to_string()),
            ("off_in", off_in.to_string()),
            ("inode_out", inode_out.to_string()),
            ("fh_out", fh_out.to_string()),
            ("off_out", off_out.to_string()),
            ("length", length.to_string()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .copy_file_range(
                req, inode, fh_in, off_in, inode_out, fh_out, off_out, length, flags,
            )
            .await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn fallocate(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        offset: u64,
        length: u64,
        mode: u32,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "fallocate";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("offset", offset.to_string()),
            ("length", length.to_string()),
            ("mode", mode.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .fallocate(req, inode, fh, offset, length, mode)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn flush(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        lock_owner: u64,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "flush";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("lock_owner", lock_owner.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.flush(req, inode, fh, lock_owner).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn fsyncdir(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        datasync: bool,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "fsyncdir";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("datasync", datasync.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.fsyncdir(req, inode, fh, datasync).await;
        self.log_result(&uuid, method, &result);
        result
    }

    // async  fn interrupt(&self, req: Request, unique: u64) -> Result<()> {
    //     let uuid = Uuid::new_v4();
    //     let method = "interrupt";
    //     let args = vec![
    //         ("unique", unique.to_string())
    //     ];
    //     self.log_start(&req,&uuid, method, &args);
    //     let result = self.inner.interrupt(req, unique).await;
    //     self.log_result(&uuid, method, &result);
    //     result
    // }

    async fn notify_reply(
        &self,
        req: Request,
        inode: fuse3::Inode,
        offset: u64,
        data: Bytes,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "notify_reply";
        let args = vec![("inode", inode.to_string()), ("offset", offset.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.notify_reply(req, inode, offset, data).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn poll(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        kh: Option<u64>,
        flags: u32,
        events: u32,
        notify: &Notify,
    ) -> Result<ReplyPoll> {
        let uuid = Uuid::new_v4();
        let method = "poll";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("flags", flags.to_string()),
            ("events", events.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .poll(req, inode, fh, kh, flags, events, notify)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn readlink(&self, req: Request, inode: fuse3::Inode) -> Result<ReplyData> {
        let uuid = Uuid::new_v4();
        let method = "readlink";
        let args = vec![("inode", inode.to_string())];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.readlink(req, inode).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn release(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "release";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("flags", flags.to_string()),
            ("lock_owner", lock_owner.to_string()),
            ("flush", flush.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self
            .inner
            .release(req, inode, fh, flags, lock_owner, flush)
            .await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn releasedir(
        &self,
        req: Request,
        inode: fuse3::Inode,
        fh: u64,
        flags: u32,
    ) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "releasedir";
        let args = vec![
            ("inode", inode.to_string()),
            ("fh", fh.to_string()),
            ("flags", flags.to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.releasedir(req, inode, fh, flags).await;
        self.log_result(&uuid, method, &result);
        result
    }

    async fn removexattr(&self, req: Request, inode: fuse3::Inode, name: &OsStr) -> Result<()> {
        let uuid = Uuid::new_v4();
        let method = "removexattr";
        let args = vec![
            ("inode", inode.to_string()),
            ("name", name.to_string_lossy().to_string()),
        ];
        self.log_start(&req,&uuid, method, &args);
        let result = self.inner.removexattr(req, inode, name).await;
        self.log_result(&uuid, method, &result);
        result
    }
}
