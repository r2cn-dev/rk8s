use bitflags::bitflags;
use bytes::Bytes;
use fuse3::{Errno, Inode, Result, raw::prelude::*};
use futures::stream;
use futures_util::stream::Iter;
use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{self, ManuallyDrop, MaybeUninit},
    num::NonZeroU32,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::ffi::OsStringExt,
    },
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use vm_memory::{ByteValued, bitmap::BitmapSlice};

use crate::{
    passthrough::{CURRENT_DIR_CSTR, EMPTY_CSTR, PARENT_DIR_CSTR},
    util::{convert_stat64_to_file_attr, filetype_from_mode},
};

use super::{
    Handle, HandleData, PassthroughFs, config::CachePolicy, os_compat::LinuxDirent64, util::*,
};
use std::vec::IntoIter;
// Flags use by the OPEN request/reply.
/// Bypass page cache for this open file.
const FOPEN_DIRECT_IO: u32 = 1;

/// Don't invalidate the data cache on open.
const FOPEN_KEEP_CACHE: u32 = 2;

/// The file is not seekable.
const FOPEN_NONSEEKABLE: u32 = 4;

/// allow caching this directory
const FOPEN_CACHE_DIR: u32 = 8;

/// the file is stream-like (no file position at all)
const FOPEN_STREAM: u32 = 16;

bitflags! {
    /// Options controlling the behavior of files opened by the server in response
    /// to an open or create request.
    pub struct OpenOptions: u32 {
        /// Bypass page cache for this open file.
        const DIRECT_IO = FOPEN_DIRECT_IO;
        /// Don't invalidate the data cache on open.
        const KEEP_CACHE = FOPEN_KEEP_CACHE;
        /// The file is not seekable.
        const NONSEEKABLE = FOPEN_NONSEEKABLE;
        /// allow caching this directory
        const CACHE_DIR = FOPEN_CACHE_DIR;
        /// the file is stream-like (no file position at all)
        const STREAM = FOPEN_STREAM;
    }
}

impl<S: BitmapSlice + Send + Sync> PassthroughFs<S> {
    async fn open_inode(&self, inode: Inode, flags: i32) -> io::Result<File> {
        let data = self.inode_map.get(inode).await?;
        if !is_safe_inode(data.mode) {
            Err(ebadf())
        } else {
            let mut new_flags = self.get_writeback_open_flags(flags).await;
            if !self.cfg.allow_direct_io && flags & libc::O_DIRECT != 0 {
                new_flags &= !libc::O_DIRECT;
            }
            data.open_file(new_flags | libc::O_CLOEXEC, &self.proc_self_fd)
        }
    }

    /// Check the HandleData flags against the flags from the current request
    /// if these do not match update the file descriptor flags and store the new
    /// result in the HandleData entry
    async fn check_fd_flags(&self, data: Arc<HandleData>, fd: RawFd, flags: u32) -> io::Result<()> {
        let open_flags = data.get_flags().await;
        if open_flags != flags {
            let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
            data.set_flags(flags).await;
        }
        Ok(())
    }
    async fn do_readdir(
        &self,
        inode: Inode,
        handle: Handle,
        offset: u64,
        entry_list: &mut Vec<std::result::Result<DirectoryEntry, Errno>>,
    ) -> io::Result<()> {
        const BUFFER_SIZE: usize = 8192;

        let data = self.get_dirdata(handle, inode, libc::O_RDONLY).await?;

        // Since we are going to work with the kernel offset, we have to acquire the file lock
        // for both the `lseek64` and `getdents64` syscalls to ensure that no other thread
        // changes the kernel offset while we are using it.
        let (guard, dir) = data.get_file_mut().await;

        // alloc buff ,pay attention to alian.
        let mut buffer = vec![0u8; BUFFER_SIZE];

        // Safe because this doesn't modify any memory and we check the return value.
        let res =
            unsafe { libc::lseek64(dir.as_raw_fd(), offset as libc::off64_t, libc::SEEK_SET) };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        loop {
            // call getdents64 system call
            let result = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    dir.as_raw_fd(),
                    buffer.as_mut_ptr() as *mut LinuxDirent64,
                    BUFFER_SIZE,
                )
            };

            if result == -1 {
                return Err(std::io::Error::last_os_error());
            }

            let bytes_read = result as usize;
            if bytes_read == 0 {
                break; // no more
            }

            // push every entry .
            let mut offset = 0;
            while offset < bytes_read {
                //let (front, back) = buffer.split_at(size_of::<LinuxDirent64>());
                //size_of::<LinuxDirent64>()
                let front = &buffer[offset..offset + size_of::<LinuxDirent64>()];
                let back = &buffer[offset + size_of::<LinuxDirent64>()..];

                let dirent64 = LinuxDirent64::from_slice(front)
                    .expect("fuse: unable to get LinuxDirent64 from slice");

                let namelen = dirent64.d_reclen as usize - size_of::<LinuxDirent64>();
                debug_assert!(
                    namelen <= back.len(),
                    "fuse: back is smaller than `namelen`"
                );

                let name = &back[..namelen];
                if name.eq(CURRENT_DIR_CSTR) || name.eq(PARENT_DIR_CSTR) {
                    offset += dirent64.d_reclen as usize;
                    continue;
                }
                let name = bytes_to_cstr(name)
                    .map_err(|e| {
                        error!("fuse: do_readdir: {:?}", e);
                        einval()
                    })?
                    .to_bytes();

                let mut entry = DirectoryEntry {
                    inode: dirent64.d_ino,
                    kind: filetype_from_mode((dirent64.d_ty as u16 * 0x1000u16).into()),
                    name: OsString::from_vec(name.to_vec()),
                    offset: dirent64.d_off,
                };
                // Safe because do_readdir() has ensured dir_entry.name is a
                // valid [u8] generated by CStr::to_bytes().
                let name = osstr_to_cstr(&entry.name)?;

                let _entry = self.do_lookup(inode, &name).await?;
                let mut inodes = self.inode_map.inodes.write().await;

                self.forget_one(&mut inodes, _entry.attr.ino, 1);
                entry.inode = _entry.attr.ino;
                entry_list.push(Ok(entry));

                // move to next entry
                offset += dirent64.d_reclen as usize;
            }
        }

        // Explicitly drop the lock so that it's not held while we fill in the fuse buffer.
        mem::drop(guard);

        Ok(())
    }

    async fn do_readdirplus(
        &self,
        inode: Inode,
        handle: Handle,
        offset: u64,
        entry_list: &mut Vec<std::result::Result<DirectoryEntryPlus, Errno>>,
    ) -> io::Result<()> {
        const BUFFER_SIZE: usize = 8192;

        let data = self.get_dirdata(handle, inode, libc::O_RDONLY).await?;

        // Since we are going to work with the kernel offset, we have to acquire the file lock
        // for both the `lseek64` and `getdents64` syscalls to ensure that no other thread
        // changes the kernel offset while we are using it.
        let (guard, dir) = data.get_file_mut().await;

        // alloc buff ,pay attention to alian.
        let mut buffer = vec![0u8; BUFFER_SIZE];

        // Safe because this doesn't modify any memory and we check the return value.
        let res =
            unsafe { libc::lseek64(dir.as_raw_fd(), offset as libc::off64_t, libc::SEEK_SET) };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }
        loop {
            // call getdents64 system call
            let result = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    dir.as_raw_fd(),
                    buffer.as_mut_ptr() as *mut LinuxDirent64,
                    BUFFER_SIZE,
                )
            };

            if result == -1 {
                return Err(std::io::Error::last_os_error());
            }

            let bytes_read = result as usize;
            if bytes_read == 0 {
                break;
            }

            let mut offset = 0;
            while offset < bytes_read {
                //size_of::<LinuxDirent64>()
                let front = &buffer[offset..offset + size_of::<LinuxDirent64>()];
                let back = &buffer[offset + size_of::<LinuxDirent64>()..];
                //let (front, back) = buffer.split_at(size_of::<LinuxDirent64>());

                let dirent64 = LinuxDirent64::from_slice(front)
                    .expect("fuse: unable to get LinuxDirent64 from slice");

                let namelen = dirent64.d_reclen as usize - size_of::<LinuxDirent64>();
                debug_assert!(
                    namelen <= back.len(),
                    "fuse: back is smaller than `namelen`"
                );

                let name = &back[..namelen];
                if name.starts_with(CURRENT_DIR_CSTR) || name.starts_with(PARENT_DIR_CSTR) {
                    offset += dirent64.d_reclen as usize;
                    continue;
                }
                let name = bytes_to_cstr(name)
                    .map_err(|e| {
                        error!("fuse: do_readdir: {:?}", e);
                        einval()
                    })?
                    .to_bytes();

                let mut entry = DirectoryEntry {
                    inode: dirent64.d_ino,
                    kind: filetype_from_mode((dirent64.d_ty as u16 * 0x1000u16).into()),
                    name: OsString::from_vec(name.to_vec()),
                    offset: dirent64.d_off,
                };
                // Safe because do_readdir() has ensured dir_entry.name is a
                // valid [u8] generated by CStr::to_bytes().
                let name = osstr_to_cstr(&entry.name)?;
                debug!("readdir:{}", name.to_str().unwrap());
                let _entry = self.do_lookup(inode, &name).await?;
                entry.inode = _entry.attr.ino;

                entry_list.push(Ok(DirectoryEntryPlus {
                    inode: entry.inode,
                    generation: _entry.generation,
                    kind: entry.kind,
                    name: entry.name,
                    offset: entry.offset,
                    attr: _entry.attr,
                    entry_ttl: _entry.ttl,
                    attr_ttl: _entry.ttl,
                }));
                // add the offset.
                offset += dirent64.d_reclen as usize;
            }
        }
        drop(guard);
        Ok(())
    }

    async fn do_open(&self, inode: Inode, flags: u32) -> io::Result<(Option<Handle>, OpenOptions)> {
        let file = self.open_inode(inode, flags as i32).await?;

        let data = HandleData::new(inode, file, flags);
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handle_map.insert(handle, data).await;

        let mut opts = OpenOptions::empty();
        match self.cfg.cache_policy {
            // We only set the direct I/O option on files.
            CachePolicy::Never => opts.set(
                OpenOptions::DIRECT_IO,
                flags & (libc::O_DIRECTORY as u32) == 0,
            ),
            CachePolicy::Metadata => {
                if flags & (libc::O_DIRECTORY as u32) == 0 {
                    opts |= OpenOptions::DIRECT_IO;
                } else {
                    opts |= OpenOptions::CACHE_DIR | OpenOptions::KEEP_CACHE;
                }
            }
            CachePolicy::Always => {
                opts |= OpenOptions::KEEP_CACHE;
                if flags & (libc::O_DIRECTORY as u32) != 0 {
                    opts |= OpenOptions::CACHE_DIR;
                }
            }
            _ => {}
        };

        Ok((Some(handle), opts))
    }

    async fn do_getattr(
        &self,
        inode: Inode,
        handle: Option<Handle>,
    ) -> io::Result<(libc::stat64, Duration)> {
        let data = self.inode_map.get(inode).await.map_err(|e| {
            error!("fuse: do_getattr ino {} Not find err {:?}", inode, e);
            e
        })?;

        // kernel sends 0 as handle in case of no_open, and it depends on fuse server to handle
        // this case correctly.
        let st = if !self.no_open.load(Ordering::Relaxed) && handle.is_some() {
            // Safe as we just checked handle
            let hd = self.handle_map.get(handle.unwrap(), inode).await?;
            stat_fd(hd.get_file(), None)
        } else {
            data.handle.stat()
        };

        let mut st = st.map_err(|e| {
            error!("fuse: do_getattr stat failed ino {} err {:?}", inode, e);
            e
        })?;
        st.st_ino = inode; //FIX BUG!
        Ok((st, self.cfg.attr_timeout))
    }

    async fn do_unlink(&self, parent: Inode, name: &CStr, flags: libc::c_int) -> io::Result<()> {
        println!(
            "do_unlink: parent = {}, name = {}",
            parent,
            name.to_str().unwrap_or("<invalid UTF-8>")
        );
        let data = self.inode_map.get(parent).await?;
        let file = data.get_file()?;
        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe { libc::unlinkat(file.as_raw_fd(), name.as_ptr(), flags) };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    async fn get_dirdata(
        &self,
        handle: Handle,
        inode: Inode,
        flags: libc::c_int,
    ) -> io::Result<Arc<HandleData>> {
        let no_open = self.no_opendir.load(Ordering::Relaxed);
        if !no_open {
            self.handle_map.get(handle, inode).await
        } else {
            let file = self.open_inode(inode, flags | libc::O_DIRECTORY).await?;
            Ok(Arc::new(HandleData::new(inode, file, flags as u32)))
        }
    }

    async fn get_data(
        &self,
        handle: Handle,
        inode: Inode,
        flags: libc::c_int,
    ) -> io::Result<Arc<HandleData>> {
        let no_open = self.no_open.load(Ordering::Relaxed);
        if !no_open {
            self.handle_map.get(handle, inode).await
        } else {
            let file = self.open_inode(inode, flags).await?;
            Ok(Arc::new(HandleData::new(inode, file, flags as u32)))
        }
    }
}
impl Filesystem for PassthroughFs {
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

    /// initialize filesystem. Called before any other filesystem method.
    async fn init(&self, _req: Request) -> Result<ReplyInit> {
        if self.cfg.do_import {
            self.import().await?;
        }

        Ok(ReplyInit {
            max_write: NonZeroU32::new(128 * 1024).unwrap(),
        })
    }

    /// clean up filesystem. Called on filesystem exit which is fuseblk, in normal fuse filesystem,
    /// kernel may call forget for root. There is some discuss for this
    /// <https://github.com/bazil/fuse/issues/82#issuecomment-88126886>,
    /// <https://sourceforge.net/p/fuse/mailman/message/31995737/>
    async fn destroy(&self, _req: Request) {
        self.handle_map.clear().await;
        self.inode_map.clear().await;

        if let Err(e) = self.import().await {
            error!("fuse: failed to destroy instance, {:?}", e);
        };
    }

    /// look up a directory entry by name and get its attributes.
    async fn lookup(&self, _req: Request, parent: Inode, name: &OsStr) -> Result<ReplyEntry> {
        // Don't use is_safe_path_component(), allow "." and ".." for NFS export support
        if name.to_string_lossy().as_bytes().contains(&SLASH_ASCII) {
            return Err(einval().into());
        }
        let name = osstr_to_cstr(name).unwrap();
        self.do_lookup(parent, name.as_ref()).await
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
        let mut inodes = self.inode_map.inodes.write().await;

        self.forget_one(&mut inodes, inode, nlookup)
    }

    /// get file attributes. If `fh` is None, means `fh` is not set.
    async fn getattr(
        &self,
        _req: Request,
        inode: Inode,
        fh: Option<u64>,
        _flags: u32,
    ) -> Result<ReplyAttr> {
        let re = self.do_getattr(inode, fh).await?;
        Ok(ReplyAttr {
            ttl: re.1,
            attr: convert_stat64_to_file_attr(re.0),
        })
    }

    /// set file attributes. If `fh` is None, means `fh` is not set.
    async fn setattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        let inode_data = self.inode_map.get(inode).await?;

        enum Data {
            Handle(Arc<HandleData>),
            ProcPath(CString),
        }

        let file = inode_data.get_file()?;
        let data = if self.no_open.load(Ordering::Relaxed) {
            let pathname = CString::new(format!("{}", file.as_raw_fd()))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Data::ProcPath(pathname)
        } else {
            // If we have a handle then use it otherwise get a new fd from the inode.
            if let Some(handle) = fh {
                let hd = self.handle_map.get(handle, inode).await?;
                Data::Handle(hd)
            } else {
                let pathname = CString::new(format!("{}", file.as_raw_fd()))
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Data::ProcPath(pathname)
            }
        };

        if set_attr.size.is_some() && self.seal_size.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(libc::EPERM).into());
        }

        if set_attr.mode.is_some() {
            // Safe because this doesn't modify any memory and we check the return value.
            let res = unsafe {
                match data {
                    Data::Handle(ref h) => {
                        libc::fchmod(h.borrow_fd().as_raw_fd(), set_attr.mode.unwrap())
                    }
                    Data::ProcPath(ref p) => libc::fchmodat(
                        self.proc_self_fd.as_raw_fd(),
                        p.as_ptr(),
                        set_attr.mode.unwrap(),
                        0,
                    ),
                }
            };
            if res < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }

        if set_attr.uid.is_some() && set_attr.gid.is_some() {
            //valid.intersects(SetattrValid::UID | SetattrValid::GID)
            let uid = set_attr.uid.unwrap();
            let gid = set_attr.gid.unwrap();

            // Safe because this is a constant value and a valid C string.
            let empty = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

            // Safe because this doesn't modify any memory and we check the return value.
            let res = unsafe {
                libc::fchownat(
                    file.as_raw_fd(),
                    empty.as_ptr(),
                    uid,
                    gid,
                    libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if res < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }

        if set_attr.size.is_some() {
            let size = set_attr.size.unwrap();
            // Safe because this doesn't modify any memory and we check the return value.
            let res = match data {
                Data::Handle(ref h) => unsafe {
                    libc::ftruncate(h.borrow_fd().as_raw_fd(), size.try_into().unwrap())
                },
                _ => {
                    // There is no `ftruncateat` so we need to get a new fd and truncate it.
                    let f = self
                        .open_inode(inode, libc::O_NONBLOCK | libc::O_RDWR)
                        .await?;
                    unsafe { libc::ftruncate(f.as_raw_fd(), size.try_into().unwrap()) }
                }
            };
            if res < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }

        if set_attr.atime.is_some() && set_attr.mtime.is_some() {
            let mut tvs: [libc::timespec; 2] = [
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
            ];
            tvs[0].tv_sec = set_attr.atime.unwrap().sec;
            tvs[1].tv_sec = set_attr.mtime.unwrap().sec;

            // Safe because this doesn't modify any memory and we check the return value.
            let res = match data {
                Data::Handle(ref h) => unsafe {
                    libc::futimens(h.borrow_fd().as_raw_fd(), tvs.as_ptr())
                },
                Data::ProcPath(ref p) => unsafe {
                    libc::utimensat(self.proc_self_fd.as_raw_fd(), p.as_ptr(), tvs.as_ptr(), 0)
                },
            };
            if res < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }

        self.getattr(req, inode, fh, 0).await
    }

    /// read symbolic link.
    async fn readlink(&self, _req: Request, inode: Inode) -> Result<ReplyData> {
        // Safe because this is a constant value and a valid C string.
        let empty = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };
        let mut buf = Vec::<u8>::with_capacity(libc::PATH_MAX as usize);
        let data = self.inode_map.get(inode).await?;

        let file = data.get_file()?;

        // Safe because this will only modify the contents of `buf` and we check the return value.
        let res = unsafe {
            libc::readlinkat(
                file.as_raw_fd(),
                empty.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                libc::PATH_MAX as usize,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error().into());
        }

        // Safe because we trust the value returned by kernel.
        unsafe { buf.set_len(res as usize) };

        Ok(ReplyData {
            data: Bytes::from(buf),
        })
    }

    /// create a symbolic link.
    async fn symlink(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        let link = osstr_to_cstr(link).unwrap();
        let link = link.as_ref();
        self.validate_path_component(name)?;

        let data = self.inode_map.get(parent).await?;

        let res = {
            //let (_uid, _gid) = set_creds(req.uid, req.gid)?;

            let file = data.get_file()?;
            // Safe because this doesn't modify any memory and we check the return value.
            unsafe { libc::symlinkat(link.as_ptr(), file.as_raw_fd(), name.as_ptr()) }
        };
        if res == 0 {
            self.do_lookup(parent, name).await
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// create file node. Create a regular file, character device, block device, fifo or socket
    /// node. When creating file, most cases user only need to implement
    /// [`create`][Filesystem::create].
    async fn mknod(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        rdev: u32,
    ) -> Result<ReplyEntry> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        self.validate_path_component(name)?;

        let data = self.inode_map.get(parent).await?;
        let file = data.get_file()?;

        let res = {
            //let (_uid, _gid) = set_creds(req.uid, req.gid)?;

            // Safe because this doesn't modify any memory and we check the return value.
            unsafe {
                libc::mknodat(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    (mode) as libc::mode_t,
                    u64::from(rdev),
                )
            }
        };
        if res < 0 {
            Err(io::Error::last_os_error().into())
        } else {
            self.do_lookup(parent, name).await
        }
    }

    /// create a directory.
    async fn mkdir(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<ReplyEntry> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        self.validate_path_component(name)?;

        let data = self.inode_map.get(parent).await?;

        let res = {
            //let (_uid, _gid) = set_creds(req.uid, req.gid)?;

            let file = data.get_file()?;
            // Safe because this doesn't modify any memory and we check the return value.
            unsafe { libc::mkdirat(file.as_raw_fd(), name.as_ptr(), mode & !umask) }
        };
        if res < 0 {
            return Err(io::Error::last_os_error().into());
        }

        self.do_lookup(parent, name).await
    }

    /// remove a file.
    async fn unlink(&self, _req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        self.validate_path_component(name)?;
        self.do_unlink(parent, name, 0).await.map_err(|e| e.into())
    }

    /// remove a directory.
    async fn rmdir(&self, _req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        self.validate_path_component(name)?;
        self.do_unlink(parent, name, libc::AT_REMOVEDIR)
            .await
            .map_err(|e| e.into())
    }

    /// create a hard link.
    async fn link(
        &self,
        _req: Request,
        inode: Inode,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let newname = osstr_to_cstr(new_name).unwrap();
        let newname = newname.as_ref();
        self.validate_path_component(newname)?;

        let data = self.inode_map.get(inode).await?;
        let new_inode = self.inode_map.get(new_parent).await?;
        let file = data.get_file()?;
        let new_file = new_inode.get_file()?;

        // Safe because this is a constant value and a valid C string.
        let empty = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::linkat(
                file.as_raw_fd(),
                empty.as_ptr(),
                new_file.as_raw_fd(),
                newname.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        if res == 0 {
            self.do_lookup(new_parent, newname).await
        } else {
            Err(io::Error::last_os_error().into())
        }
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
    async fn open(&self, _req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        if self.no_open.load(Ordering::Relaxed) {
            info!("fuse: open is not supported.");
            Err(enosys().into())
        } else {
            let re = self.do_open(inode, flags).await?;
            println!("open handle:{}", re.0.unwrap());
            Ok(ReplyOpen {
                fh: re.0.unwrap(),
                flags: re.1.bits(),
            })
        }
    }

    /// read data. Read should send exactly the number of bytes requested except on EOF or error,
    /// otherwise the rest of the data will be substituted with zeroes. An exception to this is
    /// when the file has been opened in `direct_io` mode, in which case the return value of the
    /// read system call will reflect the return value of this operation. `fh` will contain the
    /// value set by the open method, or will be undefined if the open method didn't set any value.
    async fn read(
        &self,
        _req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        let data = self.get_data(fh, inode, libc::O_RDONLY).await?;
        let f = unsafe { File::from_raw_fd(data.borrow_fd().as_raw_fd()) };
        let mut f = ManuallyDrop::new(f);
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0; size as usize];
        match f.read(&mut buf) {
            Ok(bytes_read) => {
                if bytes_read < size as usize {
                    buf.truncate(bytes_read); // Adjust the buffer size
                }
            }
            Err(err) => {
                error!("read error: {}", err);
                return Err(err.into());
            }
        };

        Ok(ReplyData {
            data: Bytes::from(buf),
        })
        //w.wr(&mut *f, size as usize, offset)
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
        _req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        data: &[u8],
        _write_flags: u32,
        flags: u32,
    ) -> Result<ReplyWrite> {
        let handle_data = self.get_data(fh, inode, libc::O_RDWR).await?;

        // Manually implement File::try_clone() by borrowing fd of data.file instead of dup().
        // It's safe because the `data` variable's lifetime spans the whole function,
        // so data.file won't be closed.
        let f = unsafe { File::from_raw_fd(handle_data.borrow_fd().as_raw_fd()) };
        let mut f = ManuallyDrop::new(f);
        self.check_fd_flags(handle_data.clone(), f.as_raw_fd(), flags)
            .await?; //TODO: deal with this flags. 

        // if self.seal_size.load().await {
        //     let st = stat_fd(&f, None)?;
        //     self.seal_size_check(Opcode::Write, st.st_size as u64, offset, size as u64, 0)?;
        // }

        // Cap restored when _killpriv is dropped
        // let _killpriv =
        //     if self.killpriv_v2.load(Ordering::Relaxed) && (fuse_flags & WRITE_KILL_PRIV != 0) {
        //         self::drop_cap_fsetid()?
        //     } else {
        //         None
        //     };
        f.seek(SeekFrom::Start(offset))?;
        let res = f.write(data)?;

        Ok(ReplyWrite {
            written: res as u32,
        })
    }

    /// get filesystem statistics.
    async fn statfs(&self, _req: Request, inode: Inode) -> Result<ReplyStatFs> {
        let mut out = MaybeUninit::<libc::statvfs64>::zeroed();
        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;

        // Safe because this will only modify `out` and we check the return value.
        let statfs: libc::statvfs64 =
            match unsafe { libc::fstatvfs64(file.as_raw_fd(), out.as_mut_ptr()) } {
                // Safe because the kernel guarantees that `out` has been initialized.
                0 => unsafe { out.assume_init() },
                _ => return Err(io::Error::last_os_error().into()),
            };

        Ok(
            // Populate the ReplyStatFs structure with the necessary information
            ReplyStatFs {
                blocks: statfs.f_blocks,
                bfree: statfs.f_bfree,
                bavail: statfs.f_bavail,
                files: statfs.f_files,
                ffree: statfs.f_ffree,
                bsize: statfs.f_bsize as u32,
                namelen: statfs.f_namemax as u32,
                frsize: statfs.f_frsize as u32,
            },
        )
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
        _req: Request,
        inode: Inode,
        fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
    ) -> Result<()> {
        if self.no_open.load(Ordering::Relaxed) {
            Err(enosys().into())
        } else {
            self.do_release(inode, fh).await.map_err(|e| e.into())
        }
    }

    /// synchronize file contents. If the `datasync` is true, then only the user data should be
    /// flushed, not the metadata.
    async fn fsync(&self, _req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        let data = self.get_data(fh, inode, libc::O_RDONLY).await?;
        let fd = data.borrow_fd();

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            if datasync {
                libc::fdatasync(fd.as_raw_fd())
            } else {
                libc::fsync(fd.as_raw_fd())
            }
        };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// set an extended attribute.
    async fn setxattr(
        &self,
        _req: Request,
        inode: Inode,
        name: &OsStr,
        value: &[u8],
        flags: u32,
        _position: u32,
    ) -> Result<()> {
        if !self.cfg.xattr {
            return Err(enosys().into());
        }
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;
        let pathname = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to use the {set,get,remove,list}xattr variants.
        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::setxattr(
                pathname.as_ptr(),
                name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                flags as libc::c_int,
            )
        };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// Get an extended attribute. If `size` is too small, return `Err<ERANGE>`.
    /// Otherwise, use [`ReplyXAttr::Data`] to send the attribute data, or
    /// return an error.
    async fn getxattr(
        &self,
        _req: Request,
        inode: Inode,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        if !self.cfg.xattr {
            return Err(enosys().into());
        }
        let name =
            osstr_to_cstr(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let name = name.as_ref();
        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;
        let mut buf = Vec::<u8>::with_capacity(size as usize);
        let pathname = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd(),))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to use the {set,get,remove,list}xattr variants.
        // Safe because this will only modify the contents of `buf`.
        let res = unsafe {
            libc::getxattr(
                pathname.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                size as libc::size_t,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error().into());
        }

        if size == 0 {
            Ok(ReplyXAttr::Size(res as u32))
        } else {
            // Safe because we trust the value returned by kernel.
            unsafe { buf.set_len(res as usize) };
            Ok(ReplyXAttr::Data(Bytes::from(buf)))
        }
    }

    /// List extended attribute names.
    ///
    /// If `size` is too small, return `Err<ERANGE>`.  Otherwise, use
    /// [`ReplyXAttr::Data`] to send the attribute list, or return an error.
    async fn listxattr(&self, _req: Request, inode: Inode, size: u32) -> Result<ReplyXAttr> {
        if !self.cfg.xattr {
            return Err(enosys().into());
        }

        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;
        let mut buf = Vec::<u8>::with_capacity(size as usize);
        let pathname = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to use the {set,get,remove,list}xattr variants.
        // Safe because this will only modify the contents of `buf`.
        let res = unsafe {
            libc::listxattr(
                pathname.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                size as libc::size_t,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error().into());
        }

        if size == 0 {
            Ok(ReplyXAttr::Size(res as u32))
        } else {
            // Safe because we trust the value returned by kernel.
            unsafe { buf.set_len(res as usize) };
            Ok(ReplyXAttr::Data(Bytes::from(buf)))
        }
    }

    /// remove an extended attribute.
    async fn removexattr(&self, _req: Request, inode: Inode, name: &OsStr) -> Result<()> {
        if !self.cfg.xattr {
            return Err(enosys().into());
        }
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;
        let pathname = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to use the {set,get,remove,list}xattr variants.
        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe { libc::removexattr(pathname.as_ptr(), name.as_ptr()) };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
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
    async fn flush(&self, _req: Request, inode: Inode, fh: u64, _lock_owner: u64) -> Result<()> {
        if self.no_open.load(Ordering::Relaxed) {
            return Err(enosys().into());
        }

        let data = self.handle_map.get(fh, inode).await?;

        // Since this method is called whenever an fd is closed in the client, we can emulate that
        // behavior by doing the same thing (dup-ing the fd and then immediately closing it). Safe
        // because this doesn't modify any memory and we check the return values.
        unsafe {
            let newfd = libc::dup(data.borrow_fd().as_raw_fd());
            if newfd < 0 {
                return Err(io::Error::last_os_error().into());
            }

            if libc::close(newfd) < 0 {
                Err(io::Error::last_os_error().into())
            } else {
                Ok(())
            }
        }
    }

    /// open a directory. Filesystem may store an arbitrary file handle (pointer, index, etc) in
    /// `fh`, and use this in other all other directory stream operations
    /// ([`readdir`][Filesystem::readdir], [`releasedir`][Filesystem::releasedir],
    /// [`fsyncdir`][Filesystem::fsyncdir]). Filesystem may also implement stateless directory
    /// I/O and not store anything in `fh`.  A file system need not implement this method if it
    /// sets [`MountOptions::no_open_dir_support`][crate::MountOptions::no_open_dir_support] and
    /// if the kernel supports `FUSE_NO_OPENDIR_SUPPORT`.
    async fn opendir(&self, _req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        if self.no_opendir.load(Ordering::Relaxed) {
            info!("fuse: opendir is not supported.");
            Err(enosys().into())
        } else {
            let t = self
                .do_open(inode, flags | (libc::O_DIRECTORY as u32))
                .await?;
            let fd = t.0.unwrap();
            Ok(ReplyOpen {
                fh: fd,
                flags: t.1.bits(),
            })
        }
    }

    /// read directory. `offset` is used to track the offset of the directory entries. `fh` will
    /// contain the value set by the [`opendir`][Filesystem::opendir] method, or will be
    /// undefined if the [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn readdir(
        &self,
        _req: Request,
        parent: Inode,
        fh: u64,
        offset: i64,
    ) -> Result<ReplyDirectory<Self::DirEntryStream<'_>>> {
        if self.no_readdir.load(Ordering::Relaxed) {
            return Err(enosys().into());
        }
        let mut entry_list = Vec::new();
        self.do_readdir(parent, fh, offset as u64, &mut entry_list)
            .await?;
        Ok(ReplyDirectory {
            entries: stream::iter(entry_list),
        })
    }

    /// read directory entries, but with their attribute, like [`readdir`][Filesystem::readdir]
    /// + [`lookup`][Filesystem::lookup] at the same time.
    async fn readdirplus(
        &self,
        _req: Request,
        parent: Inode,
        fh: u64,
        offset: u64,
        _lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        if self.no_readdir.load(Ordering::Relaxed) {
            return Err(enosys().into());
        }
        let mut entry_list = Vec::new();
        self.do_readdirplus(parent, fh, offset, &mut entry_list)
            .await?;
        Ok(ReplyDirectoryPlus {
            entries: stream::iter(entry_list),
        })
    }
    /// release an open directory. For every [`opendir`][Filesystem::opendir] call there will
    /// be exactly one `releasedir` call. `fh` will contain the value set by the
    /// [`opendir`][Filesystem::opendir] method, or will be undefined if the
    /// [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn releasedir(&self, _req: Request, inode: Inode, fh: u64, _flags: u32) -> Result<()> {
        if self.no_opendir.load(Ordering::Relaxed) {
            info!("fuse: releasedir is not supported.");
            Err(io::Error::from_raw_os_error(libc::ENOSYS).into())
        } else {
            self.do_release(inode, fh).await.map_err(|e| e.into())
        }
    }

    /// synchronize directory contents. If the `datasync` is true, then only the directory contents
    /// should be flushed, not the metadata. `fh` will contain the value set by the
    /// [`opendir`][Filesystem::opendir] method, or will be undefined if the
    /// [`opendir`][Filesystem::opendir] method didn't set any value.
    async fn fsyncdir(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        self.fsync(req, inode, fh, datasync).await
    }

    /// check file access permissions. This will be called for the `access()` system call. If the
    /// `default_permissions` mount option is given, this method is not be called. This method is
    /// not called under Linux kernel versions 2.4.x.
    async fn access(&self, req: Request, inode: Inode, mask: u32) -> Result<()> {
        let data = self.inode_map.get(inode).await?;
        let st = stat_fd(&data.get_file()?, None)?;
        let mode = mask as i32 & (libc::R_OK | libc::W_OK | libc::X_OK);

        if mode == libc::F_OK {
            // The file exists since we were able to call `stat(2)` on it.
            return Ok(());
        }

        if (mode & libc::R_OK) != 0
            && req.uid != 0
            && (st.st_uid != req.uid || st.st_mode & 0o400 == 0)
            && (st.st_gid != req.gid || st.st_mode & 0o040 == 0)
            && st.st_mode & 0o004 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES).into());
        }

        if (mode & libc::W_OK) != 0
            && req.uid != 0
            && (st.st_uid != req.uid || st.st_mode & 0o200 == 0)
            && (st.st_gid != req.gid || st.st_mode & 0o020 == 0)
            && st.st_mode & 0o002 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES).into());
        }

        // root can only execute something if it is executable by one of the owner, the group, or
        // everyone.
        if (mode & libc::X_OK) != 0
            && (req.uid != 0 || st.st_mode & 0o111 == 0)
            && (st.st_uid != req.uid || st.st_mode & 0o100 == 0)
            && (st.st_gid != req.gid || st.st_mode & 0o010 == 0)
            && st.st_mode & 0o001 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES).into());
        }

        Ok(())
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
        _req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        flags: u32,
    ) -> Result<ReplyCreated> {
        let name = osstr_to_cstr(name).unwrap();
        let name = name.as_ref();
        self.validate_path_component(name)?;

        let dir = self.inode_map.get(parent).await?;
        let dir_file = dir.get_file()?;

        let new_file = {
            //let (_uid, _gid) = set_creds(req.uid, req.gid)?;

            let flags = self.get_writeback_open_flags(flags as i32).await;
            Self::create_file_excl(&dir_file, name, flags, mode)?
        };

        let entry = self.do_lookup(parent, name).await?;
        let file = match new_file {
            // File didn't exist, now created by create_file_excl()
            Some(f) => f,
            // File exists, and args.flags doesn't contain O_EXCL. Now let's open it with
            // open_inode().
            None => {
                // Cap restored when _killpriv is dropped
                // let _killpriv = if self.killpriv_v2.load().await
                //     && (args.fuse_flags & FOPEN_IN_KILL_SUIDGID != 0)
                // {
                //     self::drop_cap_fsetid()?
                // } else {
                //     None
                // };

                //let (_uid, _gid) = set_creds(req.uid, req.gid)?;
                self.open_inode(entry.attr.ino, flags as i32).await?
            }
        };

        let ret_handle = if !self.no_open.load(Ordering::Relaxed) {
            let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
            let data = HandleData::new(entry.attr.ino, file, flags);
            self.handle_map.insert(handle, data).await;
            handle
        } else {
            return Err(io::Error::from_raw_os_error(libc::EACCES).into());
        };

        let mut opts = OpenOptions::empty();
        match self.cfg.cache_policy {
            CachePolicy::Never => opts |= OpenOptions::DIRECT_IO,
            CachePolicy::Metadata => opts |= OpenOptions::DIRECT_IO,
            CachePolicy::Always => opts |= OpenOptions::KEEP_CACHE,
            _ => {}
        };
        Ok(ReplyCreated {
            ttl: entry.ttl,
            attr: entry.attr,
            generation: entry.generation,
            fh: ret_handle,
            flags: opts.bits(),
        })
    }

    /// handle interrupt. When a operation is interrupted, an interrupt request will send to fuse
    /// server with the unique id of the operation.
    async fn interrupt(&self, _req: Request, _unique: u64) -> Result<()> {
        Ok(())
    }

    /// forget more than one inode. This is a batch version [`forget`][Filesystem::forget]
    async fn batch_forget(&self, _req: Request, inodes: &[(Inode, u64)]) {
        let mut inodes_w = self.inode_map.inodes.write().await;

        for i in inodes {
            self.forget_one(&mut inodes_w, i.0, i.1);
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
        _req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        length: u64,
        mode: u32,
    ) -> Result<()> {
        // Let the Arc<HandleData> in scope, otherwise fd may get invalid.
        let data = self.get_data(fh, inode, libc::O_RDWR).await?;
        let fd = data.borrow_fd();

        //  if self.seal_size.load().await {
        //      let st = stat_fd(&fd, None)?;
        //      self.seal_size_check(
        //          Opcode::Fallocate,
        //          st.st_size as u64,
        //          offset,
        //          length,
        //          mode as i32,
        //      )?;
        //  }

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::fallocate64(
                fd.as_raw_fd(),
                mode as libc::c_int,
                offset as libc::off64_t,
                length as libc::off64_t,
            )
        };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// rename a file or directory.
    async fn rename(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        let oldname = osstr_to_cstr(name).unwrap();
        let oldname = oldname.as_ref();
        let newname = osstr_to_cstr(new_name).unwrap();
        let newname = newname.as_ref();
        self.validate_path_component(oldname)?;
        self.validate_path_component(newname)?;

        // Check if new_name exists and is a whiteout file
        let new_parent_data = self.inode_map.get(new_parent).await?;
        let new_parent_file = new_parent_data.get_file()?;

        // Try to lookup newname to see if it exists
        // Check if new_name exists and is a whiteout file
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        let res = unsafe {
            libc::fstatat(
                new_parent_file.as_raw_fd(),
                newname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };

        if res == 0 {
            // If file exists, check if it's a whiteout file
            let st = unsafe { st.assume_init() };
            if (st.st_mode & libc::S_IFMT) == libc::S_IFCHR && st.st_rdev == 0 {
                // It's a whiteout file, delete it
                let unlink_res =
                    unsafe { libc::unlinkat(new_parent_file.as_raw_fd(), newname.as_ptr(), 0) };
                if unlink_res < 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }
        } else {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENOENT) {
                return Err(err.into());
            }
        }

        let old_inode = self.inode_map.get(parent).await?;
        let new_inode = self.inode_map.get(new_parent).await?;
        let old_file = old_inode.get_file()?;
        let new_file = new_inode.get_file()?;

        //TODO: Switch to libc::renameat2 -> libc::renameat2(olddirfd, oldpath, newdirfd, newpath, flags)
        let res = unsafe {
            libc::renameat(
                old_file.as_raw_fd(),
                oldname.as_ptr(),
                new_file.as_raw_fd(),
                newname.as_ptr(),
            )
        };

        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// rename a file or directory with flags.
    async fn rename2(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
        flags: u32,
    ) -> Result<()> {
        let oldname = osstr_to_cstr(name).unwrap();
        let oldname = oldname.as_ref();
        let newname = osstr_to_cstr(new_name).unwrap();
        let newname = newname.as_ref();
        self.validate_path_component(oldname)?;
        self.validate_path_component(newname)?;

        let old_inode = self.inode_map.get(parent).await?;
        let new_inode = self.inode_map.get(new_parent).await?;
        let old_file = old_inode.get_file()?;
        let new_file = new_inode.get_file()?;
        //TODO: Switch to libc::renameat2 -> libc::renameat2(olddirfd, oldpath, newdirfd, newpath, flags)
        let res = unsafe {
            libc::renameat2(
                old_file.as_raw_fd(),
                oldname.as_ptr(),
                new_file.as_raw_fd(),
                newname.as_ptr(),
                flags,
            )
        };

        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// find next data or hole after the specified offset.
    async fn lseek(
        &self,
        _req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        whence: u32,
    ) -> Result<ReplyLSeek> {
        // Let the Arc<HandleData> in scope, otherwise fd may get invalid.
        let data = self.handle_map.get(fh, inode).await?;

        // Acquire the lock to get exclusive access, otherwise it may break do_readdir().
        let (_guard, file) = data.get_file_mut().await;

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::lseek(
                file.as_raw_fd(),
                offset as libc::off64_t,
                whence as libc::c_int,
            )
        };
        if res < 0 {
            Err(io::Error::last_os_error().into())
        } else {
            Ok(ReplyLSeek { offset: res as u64 })
        }
    }
}

/// trim all trailing nul terminators.
pub fn bytes_to_cstr(buf: &[u8]) -> Result<&CStr> {
    // There might be multiple 0s at the end of buf, find & use the first one and trim other zeros.
    match buf.iter().position(|x| *x == 0) {
        // Convert to a `CStr` so that we can drop the '\0' byte at the end and make sure
        // there are no interior '\0' bytes.
        Some(pos) => CStr::from_bytes_with_nul(&buf[0..=pos]).map_err(|_| Errno::from(5)),
        None => {
            // Invalid input, just call CStr::from_bytes_with_nul() for suitable error code
            CStr::from_bytes_with_nul(buf).map_err(|_| Errno::from(5))
        }
    }
}
