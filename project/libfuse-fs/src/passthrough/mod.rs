use config::{CachePolicy, Config};
use file_handle::{FileHandle, OpenableFileHandle};

use fuse3::{Errno, raw::reply::ReplyEntry};
use inode_store::{InodeId, InodeStore};

use crate::util::convert_stat64_to_file_attr;
use mount_fd::MountFds;
use statx::StatExt;
use std::io::Result;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::{
    collections::{BTreeMap, btree_map},
    ffi::{CStr, CString, OsString},
    fs::File,
    io::{self, Error},
    marker::PhantomData,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, RawFd},
        unix::ffi::OsStringExt,
    },
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use util::{
    UniqueInodeGenerator, ebadf, is_dir, openat, reopen_fd_through_proc, stat_fd,
    validate_path_component,
};
use vm_memory::bitmap::BitmapSlice;

mod async_io;
mod config;
mod file_handle;
mod inode_store;
pub mod logfs;
mod mount_fd;
pub mod newlogfs;
mod os_compat;
mod statx;
mod util;


/// Current directory
pub const CURRENT_DIR_CSTR: &[u8] = b".\0";
/// Parent directory
pub const PARENT_DIR_CSTR: &[u8] = b"..\0";
pub const VFS_MAX_INO: u64 = 0xff_ffff_ffff_ffff;
const MOUNT_INFO_FILE: &str = "/proc/self/mountinfo";
pub const EMPTY_CSTR: &[u8] = b"\0";
pub const PROC_SELF_FD_CSTR: &[u8] = b"/proc/self/fd\0";
pub const ROOT_ID: u64 = 1;
use tokio::sync::{Mutex, MutexGuard, RwLock};

pub async fn new_passthroughfs_layer(rootdir: &str) -> Result<PassthroughFs> {
    let config = Config {
        root_dir: String::from(rootdir),
        // enable xattr`
        xattr: true,
        do_import: true,
        ..Default::default()
    };

    let fs = PassthroughFs::<()>::new(config)?;

    fs.import().await?;
    Ok(fs)
}

type Inode = u64;
type Handle = u64;

/// Maximum host inode number supported by passthroughfs
const MAX_HOST_INO: u64 = 0x7fff_ffff_ffff;

/**
 * Represents the file associated with an inode (`InodeData`).
 *
 * When obtaining such a file, it may either be a new file (the `Owned` variant), in which case the
 * object's lifetime is static, or it may reference `InodeData.file` (the `Ref` variant), in which
 * case the object's lifetime is that of the respective `InodeData` object.
 */
#[derive(Debug)]
enum InodeFile<'a> {
    Owned(File),
    Ref(&'a File),
}

impl AsRawFd for InodeFile<'_> {
    /// Return a file descriptor for this file
    /// Note: This fd is only valid as long as the `InodeFile` exists.
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Self::Owned(file) => file.as_raw_fd(),
            Self::Ref(file_ref) => file_ref.as_raw_fd(),
        }
    }
}

impl AsFd for InodeFile<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Owned(file) => file.as_fd(),
            Self::Ref(file_ref) => file_ref.as_fd(),
        }
    }
}

#[derive(Debug)]
enum InodeHandle {
    File(File),
    Handle(Arc<OpenableFileHandle>),
}

impl InodeHandle {
    fn file_handle(&self) -> Option<&FileHandle> {
        match self {
            InodeHandle::File(_) => None,
            InodeHandle::Handle(h) => Some(h.file_handle()),
        }
    }

    fn get_file(&self) -> Result<InodeFile<'_>> {
        match self {
            InodeHandle::File(f) => Ok(InodeFile::Ref(f)),
            InodeHandle::Handle(h) => {
                let f = h.open(libc::O_PATH)?;
                Ok(InodeFile::Owned(f))
            }
        }
    }

    fn open_file(&self, flags: libc::c_int, proc_self_fd: &File) -> Result<File> {
        match self {
            InodeHandle::File(f) => reopen_fd_through_proc(f, flags, proc_self_fd),
            InodeHandle::Handle(h) => h.open(flags),
        }
    }

    fn stat(&self) -> Result<libc::stat64> {
        match self {
            InodeHandle::File(f) => stat_fd(f, None),
            InodeHandle::Handle(_h) => {
                let file = self.get_file()?;
                stat_fd(&file, None)
            }
        }
    }
}

/// Represents an inode in `PassthroughFs`.
#[derive(Debug)]
pub struct InodeData {
    inode: Inode,
    // Most of these aren't actually files but ¯\_(ツ)_/¯.
    handle: InodeHandle,
    id: InodeId,
    refcount: AtomicU64,
    // File type and mode
    mode: u32,
}

impl InodeData {
    fn new(inode: Inode, f: InodeHandle, refcount: u64, id: InodeId, mode: u32) -> Self {
        InodeData {
            inode,
            handle: f,
            id,
            refcount: AtomicU64::new(refcount),
            mode,
        }
    }

    fn get_file(&self) -> Result<InodeFile<'_>> {
        self.handle.get_file()
    }

    fn open_file(&self, flags: libc::c_int, proc_self_fd: &File) -> Result<File> {
        self.handle.open_file(flags, proc_self_fd)
    }
}

/// Data structures to manage accessed inodes.
struct InodeMap {
    pub inodes: RwLock<InodeStore>,
}

impl InodeMap {
    fn new() -> Self {
        InodeMap {
            inodes: RwLock::new(Default::default()),
        }
    }

    async fn clear(&self) {
        // Do not expect poisoned lock here, so safe to unwrap().
        self.inodes.write().await.clear();
    }

    async fn get(&self, inode: Inode) -> Result<Arc<InodeData>> {
        // Do not expect poisoned lock here, so safe to unwrap().
        self.inodes
            .read()
            .await
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)
    }

    fn get_inode_locked(
        inodes: &InodeStore,
        id: &InodeId,
        handle: Option<&FileHandle>,
    ) -> Option<Inode> {
        match handle {
            Some(h) => inodes.inode_by_handle(h).copied(),
            None => inodes.inode_by_id(id).copied(),
        }
    }

    async fn get_alt(&self, id: &InodeId, handle: Option<&FileHandle>) -> Option<Arc<InodeData>> {
        // Do not expect poisoned lock here, so safe to unwrap().
        let inodes = self.inodes.read().await;

        Self::get_alt_locked(&inodes, id, handle)
    }

    fn get_alt_locked(
        inodes: &InodeStore,
        id: &InodeId,
        handle: Option<&FileHandle>,
    ) -> Option<Arc<InodeData>> {
        handle
            .and_then(|h| inodes.get_by_handle(h))
            .or_else(|| {
                inodes.get_by_id(id).filter(|data| {
                    // When we have to fall back to looking up an inode by its IDs, ensure that
                    // we hit an entry that does not have a file handle.  Entries with file
                    // handles must also have a handle alt key, so if we have not found it by
                    // that handle alt key, we must have found an entry with a mismatching
                    // handle; i.e. an entry for a different file, even though it has the same
                    // inode ID.
                    // (This can happen when we look up a new file that has reused the inode ID
                    // of some previously unlinked inode we still have in `.inodes`.)
                    handle.is_none() || data.handle.file_handle().is_none()
                })
            })
            .cloned()
    }

    async fn insert(&self, data: Arc<InodeData>) {
        let mut inodes = self.inodes.write().await;

        Self::insert_locked(&mut inodes, data)
    }

    fn insert_locked(inodes: &mut InodeStore, data: Arc<InodeData>) {
        inodes.insert(data);
    }
}

struct HandleData {
    inode: Inode,
    file: File,
    lock: Mutex<()>,
    open_flags: AtomicU32,
}

impl HandleData {
    fn new(inode: Inode, file: File, flags: u32) -> Self {
        HandleData {
            inode,
            file,
            lock: Mutex::new(()),
            open_flags: AtomicU32::new(flags),
        }
    }

    fn get_file(&self) -> &File {
        &self.file
    }

    async fn get_file_mut(&self) -> (MutexGuard<()>, &File) {
        (self.lock.lock().await, &self.file)
    }

    fn borrow_fd(&self) -> BorrowedFd {
        self.file.as_fd()
    }

    async fn get_flags(&self) -> u32 {
        self.open_flags.load(Ordering::Relaxed)
    }

    async fn set_flags(&self, flags: u32) {
        self.open_flags.store(flags, Ordering::Relaxed);
    }
}

struct HandleMap {
    handles: RwLock<BTreeMap<Handle, Arc<HandleData>>>,
}

impl HandleMap {
    fn new() -> Self {
        HandleMap {
            handles: RwLock::new(BTreeMap::new()),
        }
    }

    async fn clear(&self) {
        // Do not expect poisoned lock here, so safe to unwrap().
        self.handles.write().await.clear();
    }

    async fn insert(&self, handle: Handle, data: HandleData) {
        // Do not expect poisoned lock here, so safe to unwrap().
        self.handles.write().await.insert(handle, Arc::new(data));
    }

    async fn release(&self, handle: Handle, inode: Inode) -> Result<()> {
        // Do not expect poisoned lock here, so safe to unwrap().
        let mut handles = self.handles.write().await;

        if let btree_map::Entry::Occupied(e) = handles.entry(handle) {
            if e.get().inode == inode {
                // We don't need to close the file here because that will happen automatically when
                // the last `Arc` is dropped.
                e.remove();
                return Ok(());
            }
        }

        Err(ebadf())
    }

    async fn get(&self, handle: Handle, inode: Inode) -> Result<Arc<HandleData>> {
        // Do not expect poisoned lock here, so safe to unwrap().
        self.handles
            .read()
            .await
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .cloned()
            .ok_or_else(ebadf)
    }
}

/// A file system that simply "passes through" all requests it receives to the underlying file
/// system.
///
/// To keep the implementation simple it servers the contents of its root directory. Users
/// that wish to serve only a specific directory should set up the environment so that that
/// directory ends up as the root of the file system process. One way to accomplish this is via a
/// combination of mount namespaces and the pivot_root system call.
pub struct PassthroughFs<S: BitmapSlice + Send + Sync = ()> {
    // File descriptors for various points in the file system tree. These fds are always opened with
    // the `O_PATH` option so they cannot be used for reading or writing any data. See the
    // documentation of the `O_PATH` flag in `open(2)` for more details on what one can and cannot
    // do with an fd opened with this flag.
    inode_map: InodeMap,
    next_inode: AtomicU64,

    // File descriptors for open files and directories. Unlike the fds in `inodes`, these _can_ be
    // used for reading and writing data.
    handle_map: HandleMap,
    next_handle: AtomicU64,

    // Use to generate unique inode
    ino_allocator: UniqueInodeGenerator,
    // Maps mount IDs to an open FD on the respective ID for the purpose of open_by_handle_at().
    mount_fds: MountFds,

    // File descriptor pointing to the `/proc/self/fd` directory. This is used to convert an fd from
    // `inodes` into one that can go into `handles`. This is accomplished by reading the
    // `/proc/self/fd/{}` symlink. We keep an open fd here in case the file system tree that we are meant
    // to be serving doesn't have access to `/proc/self/fd`.
    proc_self_fd: File,

    // Whether writeback caching is enabled for this directory. This will only be true when
    // `cfg.writeback` is true and `init` was called with `FsOptions::WRITEBACK_CACHE`.
    writeback: AtomicBool,

    // Whether no_open is enabled.
    no_open: AtomicBool,

    // Whether no_opendir is enabled.
    no_opendir: AtomicBool,

    // Whether kill_priv_v2 is enabled.
    //killpriv_v2: AtomicBool,

    // Whether no_readdir is enabled.
    no_readdir: AtomicBool,

    // Whether seal_size is enabled.
    seal_size: AtomicBool,

    // Whether per-file DAX feature is enabled.
    // Init from guest kernel Init cmd of fuse fs.
    //perfile_dax: AtomicBool,
    dir_entry_timeout: Duration,
    dir_attr_timeout: Duration,

    cfg: Config,

    phantom: PhantomData<S>,
}

impl<S: BitmapSlice + Send + Sync> PassthroughFs<S> {
    /// Create a Passthrough file system instance.
    pub fn new(mut cfg: Config) -> Result<PassthroughFs<S>> {
        if cfg.no_open && cfg.cache_policy != CachePolicy::Always {
            warn!("passthroughfs: no_open only work with cache=always, reset to open mode");
            cfg.no_open = false;
        }
        if cfg.writeback && cfg.cache_policy == CachePolicy::Never {
            warn!(
                "passthroughfs: writeback cache conflicts with cache=none, reset to no_writeback"
            );
            cfg.writeback = false;
        }

        // Safe because this is a constant value and a valid C string.
        let proc_self_fd_cstr = unsafe { CStr::from_bytes_with_nul_unchecked(PROC_SELF_FD_CSTR) };
        let proc_self_fd = Self::open_file(
            &libc::AT_FDCWD,
            proc_self_fd_cstr,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;

        let (dir_entry_timeout, dir_attr_timeout) =
            match (cfg.dir_entry_timeout, cfg.dir_attr_timeout) {
                (Some(e), Some(a)) => (e, a),
                (Some(e), None) => (e, cfg.attr_timeout),
                (None, Some(a)) => (cfg.entry_timeout, a),
                (None, None) => (cfg.entry_timeout, cfg.attr_timeout),
            };

        let mount_fds = MountFds::new(None)?;

        Ok(PassthroughFs {
            inode_map: InodeMap::new(),
            next_inode: AtomicU64::new(ROOT_ID + 1),
            ino_allocator: UniqueInodeGenerator::new(),

            handle_map: HandleMap::new(),
            next_handle: AtomicU64::new(1),

            mount_fds,
            proc_self_fd,

            writeback: AtomicBool::new(false),
            no_open: AtomicBool::new(false),
            no_opendir: AtomicBool::new(false),
            //killpriv_v2: AtomicBool::new(false),
            no_readdir: AtomicBool::new(cfg.no_readdir),
            seal_size: AtomicBool::new(cfg.seal_size),
            //perfile_dax: AtomicBool::new(false),
            dir_entry_timeout,
            dir_attr_timeout,
            cfg,

            phantom: PhantomData,
        })
    }

    /// Initialize the Passthrough file system.
    pub async fn import(&self) -> Result<()> {
        let root = CString::new(self.cfg.root_dir.as_str()).expect("CString::new failed");

        let (path_fd, handle_opt, st) = Self::open_file_and_handle(self, &libc::AT_FDCWD, &root)
            .map_err(|e| {
                error!("fuse: import: failed to get file or handle: {:?}", e);
                e
            })?;
        let id = InodeId::from_stat(&st);
        let handle = if let Some(h) = handle_opt {
            InodeHandle::Handle(self.to_openable_handle(h)?)
        } else {
            InodeHandle::File(path_fd)
        };

        // Safe because this doesn't modify any memory and there is no need to check the return
        // value because this system call always succeeds. We need to clear the umask here because
        // we want the client to be able to set all the bits in the mode.
        unsafe { libc::umask(0o000) };

        // Not sure why the root inode gets a refcount of 2 but that's what libfuse does.
        self.inode_map
            .insert(Arc::new(InodeData::new(
                ROOT_ID,
                handle,
                2,
                id,
                st.st.st_mode,
            )))
            .await;

        Ok(())
    }

    /// Get the list of file descriptors which should be reserved across live upgrade.
    pub fn keep_fds(&self) -> Vec<RawFd> {
        vec![self.proc_self_fd.as_raw_fd()]
    }

    fn readlinkat(dfd: i32, pathname: &CStr) -> Result<PathBuf> {
        let mut buf = Vec::with_capacity(libc::PATH_MAX as usize);

        // Safe because the kernel will only write data to buf and we check the return value
        let buf_read = unsafe {
            libc::readlinkat(
                dfd,
                pathname.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.capacity(),
            )
        };
        if buf_read < 0 {
            error!("fuse: readlinkat error");
            return Err(Error::last_os_error());
        }

        // Safe because we trust the value returned by kernel.
        unsafe { buf.set_len(buf_read as usize) };
        buf.shrink_to_fit();

        // Be careful:
        // - readlink() does not append a terminating null byte to buf
        // - OsString instances are not NUL terminated
        Ok(PathBuf::from(OsString::from_vec(buf)))
    }

    /// Get the file pathname corresponding to the Inode
    /// This function is used by Nydus blobfs
    pub async fn readlinkat_proc_file(&self, inode: Inode) -> Result<PathBuf> {
        let data = self.inode_map.get(inode).await?;
        let file = data.get_file()?;
        let pathname = CString::new(format!("{}", file.as_raw_fd()))
            .map_err(|e| Error::new(io::ErrorKind::InvalidData, e))?;

        Self::readlinkat(self.proc_self_fd.as_raw_fd(), &pathname)
    }

    fn create_file_excl(
        dir: &impl AsRawFd,
        pathname: &CStr,
        flags: i32,
        mode: u32,
    ) -> io::Result<Option<File>> {
        match openat(dir, pathname, flags | libc::O_CREAT | libc::O_EXCL, mode) {
            Ok(file) => Ok(Some(file)),
            Err(err) => {
                // Ignore the error if the file exists and O_EXCL is not present in `flags`.
                if err.kind() == io::ErrorKind::AlreadyExists {
                    if (flags & libc::O_EXCL) != 0 {
                        return Err(err);
                    }
                    return Ok(None);
                }
                Err(err)
            }
        }
    }

    fn open_file(dfd: &impl AsRawFd, pathname: &CStr, flags: i32, mode: u32) -> io::Result<File> {
        openat(dfd, pathname, flags, mode)
    }

    fn open_file_restricted(
        &self,
        dir: &impl AsRawFd,
        pathname: &CStr,
        flags: i32,
        mode: u32,
    ) -> io::Result<File> {
        let flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | flags;

        // TODO
        //if self.os_facts.has_openat2 {
        //    oslib::do_open_relative_to(dir, pathname, flags, mode)
        //} else {
        openat(dir, pathname, flags, mode)
        //}
    }

    /// Create a File or File Handle for `name` under directory `dir_fd` to support `lookup()`.
    fn open_file_and_handle(
        &self,
        dir: &impl AsRawFd,
        name: &CStr,
    ) -> io::Result<(File, Option<FileHandle>, StatExt)> {
        let path_file = self.open_file_restricted(dir, name, libc::O_PATH, 0)?;
        let st = statx::statx(&path_file, None)?;
        let handle = if self.cfg.inode_file_handles {
            FileHandle::from_fd(&path_file)?
        } else {
            None
        };

        Ok((path_file, handle, st))
    }

    fn to_openable_handle(&self, fh: FileHandle) -> io::Result<Arc<OpenableFileHandle>> {
        fh.into_openable(&self.mount_fds, |fd, flags, _mode| {
            reopen_fd_through_proc(&fd, flags, &self.proc_self_fd)
        })
        .map(Arc::new)
        .map_err(|e| {
            if !e.silent() {
                error!("{}", e);
            }
            e.into_inner()
        })
    }

    async fn allocate_inode(
        &self,
        inodes: &InodeStore,
        id: &InodeId,
        handle_opt: Option<&FileHandle>,
    ) -> io::Result<Inode> {
        if !self.cfg.use_host_ino {
            // If the inode has already been assigned before, the new inode is not reassigned,
            // ensuring that the same file is always the same inode
            match InodeMap::get_inode_locked(inodes, id, handle_opt) {
                Some(a) => Ok(a),
                None => Ok(self.next_inode.fetch_add(1, Ordering::Relaxed)),
            }
        } else {
            let inode = if id.ino > MAX_HOST_INO {
                // Prefer looking for previous mappings from memory
                match InodeMap::get_inode_locked(inodes, id, handle_opt) {
                    Some(ino) => ino,
                    None => self.ino_allocator.get_unique_inode(id)?,
                }
            } else {
                self.ino_allocator.get_unique_inode(id)?
            };

            Ok(inode)
        }
    }

    async fn do_lookup(
        &self,
        parent: Inode,
        name: &CStr,
    ) -> std::result::Result<ReplyEntry, Errno> {
        let name = if parent == ROOT_ID && name.to_bytes_with_nul().starts_with(PARENT_DIR_CSTR) {
            // Safe as this is a constant value and a valid C string.
            CStr::from_bytes_with_nul(CURRENT_DIR_CSTR).unwrap()
        } else {
            name
        };

        let dir = self.inode_map.get(parent).await?;
        let dir_file = dir.get_file()?;
        let (path_fd, handle_opt, st) = Self::open_file_and_handle(self, &dir_file, name)?;
        let id = InodeId::from_stat(&st);

        let mut found = None;
        'search: loop {
            match self.inode_map.get_alt(&id, handle_opt.as_ref()).await {
                // No existing entry found
                None => break 'search,
                Some(data) => {
                    let curr = data.refcount.load(Ordering::Acquire);
                    // forgot_one() has just destroyed the entry, retry...
                    if curr == 0 {
                        continue 'search;
                    }

                    // Saturating add to avoid integer overflow, it's not realistic to saturate u64.
                    let new = curr.saturating_add(1);

                    // Synchronizes with the forgot_one()
                    if data
                        .refcount
                        .compare_exchange(curr, new, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        found = Some(data.inode);
                        break;
                    }
                }
            }
        }

        let inode = if let Some(v) = found {
            v
        } else {
            let handle = if let Some(h) = handle_opt.clone() {
                InodeHandle::Handle(self.to_openable_handle(h)?)
            } else {
                InodeHandle::File(path_fd)
            };

            // Write guard get_alt_locked() and insert_lock() to avoid race conditions.
            let inodes = self.inode_map.inodes.read().await;

            // Lookup inode_map again after acquiring the inode_map lock, as there might be another
            // racing thread already added an inode with the same id while we're not holding
            // the lock. If so just use the newly added inode, otherwise the inode will be replaced
            // and results in EBADF.
            match InodeMap::get_alt_locked(&inodes, &id, handle_opt.as_ref()) {
                Some(data) => {
                    // An inode was added concurrently while we did not hold a lock on
                    // `self.inodes_map`, so we use that instead. `handle` will be dropped.
                    let vl = data.refcount.fetch_add(1, Ordering::Relaxed);
                   
                    data.inode
                }
                None => {
                    let inode = self
                        .allocate_inode(&inodes, &id, handle_opt.as_ref())
                        .await?;

                    if inode > VFS_MAX_INO {
                        error!("fuse: max inode number reached: {}", VFS_MAX_INO);
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("max inode number reached: {VFS_MAX_INO}"),
                        )
                        .into());
                    }
                    drop(inodes);
                 
                    self.inode_map.insert(
                        Arc::new(InodeData::new(
                            inode,
                            handle,
                            1,
                            id,
                            st.st.st_mode,
                        )
                    )).await;

                    inode
                }
            }
        };

        let (entry_timeout, _) = if is_dir(st.st.st_mode) {
            (self.dir_entry_timeout, self.dir_attr_timeout)
        } else {
            (self.cfg.entry_timeout, self.cfg.attr_timeout)
        };

        // // Whether to enable file DAX according to the value of dax_file_size
        // let mut attr_flags: u32 = 0;
        // if let Some(dax_file_size) = self.cfg.dax_file_size {
        //     // st.stat.st_size is i64
        //     if self.perfile_dax.load().await
        //         && st.st.st_size >= 0x0
        //         && st.st.st_size as u64 >= dax_file_size
        //     {
        //         attr_flags |= FUSE_ATTR_DAX;
        //     }
        // }
        let mut attr_temp = convert_stat64_to_file_attr(st.st);
        attr_temp.ino = inode;
        Ok(ReplyEntry {
            ttl: entry_timeout,
            attr: attr_temp,
            generation: 0,
        })
    }

    fn forget_one(&self, inodes: &mut InodeStore, inode: Inode, count: u64) {
        
        // ROOT_ID should not be forgotten, or we're not able to access to files any more.
        if inode == ROOT_ID {
            return;
        }

        if let Some(data) = inodes.get(&inode) {
            // Acquiring the write lock on the inode map prevents new lookups from incrementing the
            // refcount but there is the possibility that a previous lookup already acquired a
            // reference to the inode data and is in the process of updating the refcount so we need
            // to loop here until we can decrement successfully.
            loop {
                let curr = data.refcount.load(Ordering::Acquire);

                // Saturating sub because it doesn't make sense for a refcount to go below zero and
                // we don't want misbehaving clients to cause integer overflow.
                let new = curr.saturating_sub(count);
                
                // Synchronizes with the acquire load in `do_lookup`.
                if data
                    .refcount
                    .compare_exchange(curr, new, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok() {
                    
                    if new == 0 {
                        // We just removed the last refcount for this inode.
                        // The allocated inode number should be kept in the map when use_host_ino
                        // is false or host inode(don't use the virtual 56bit inode) is bigger than MAX_HOST_INO.
                        let keep_mapping = !self.cfg.use_host_ino || data.id.ino > MAX_HOST_INO;
                        inodes.remove(&inode, keep_mapping);
                    }
                    break;
                }
            }
        }
    }

    async fn do_release(&self, inode: Inode, handle: Handle) -> io::Result<()> {
        self.handle_map.release(handle, inode).await
    }

    // Validate a path component, same as the one in vfs layer, but only do the validation if this
    // passthroughfs is used without vfs layer, to avoid double validation.
    fn validate_path_component(&self, name: &CStr) -> io::Result<()> {
        // !self.cfg.do_import means we're under vfs, and vfs has already done the validation
        if !self.cfg.do_import {
            return Ok(());
        }
        validate_path_component(name)
    }

    //TODO: When seal_size is set, we don't allow operations that could change file size nor allocate
    // space beyond EOF
    // fn seal_size_check(
    //     &self,
    //     opcode: Opcode,
    //     file_size: u64,
    //     offset: u64,
    //     size: u64,
    //     mode: i32,
    // ) -> io::Result<()> {
    //     if offset.checked_add(size).is_none() {
    //         error!(
    //             "fuse: {:?}: invalid `offset` + `size` ({}+{}) overflows u64::MAX",
    //             opcode, offset, size
    //         );
    //         return Err(einval());
    //     }

    //     match opcode {
    //         // write should not exceed the file size.
    //         Opcode::Write => {
    //             if size + offset > file_size {
    //                 return Err(eperm());
    //             }
    //         }

    //         Opcode::Fallocate => {
    //             let op = mode & !(libc::FALLOC_FL_KEEP_SIZE | libc::FALLOC_FL_UNSHARE_RANGE);
    //             match op {
    //                 // Allocate, punch and zero, must not change file size.
    //                 0 | libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_ZERO_RANGE => {
    //                     if size + offset > file_size {
    //                         return Err(eperm());
    //                     }
    //                 }
    //                 // collapse and insert will change file size, forbid.
    //                 libc::FALLOC_FL_COLLAPSE_RANGE | libc::FALLOC_FL_INSERT_RANGE => {
    //                     return Err(eperm());
    //                 }
    //                 // Invalid operation
    //                 _ => return Err(einval()),
    //             }
    //         }

    //         // setattr operation should be handled in setattr handler.
    //         _ => return Err(enosys()),
    //     }

    //     Ok(())
    // }

    async fn get_writeback_open_flags(&self, flags: i32) -> i32 {
        let mut new_flags = flags;
        let writeback = self.writeback.load(Ordering::Relaxed);

        // When writeback caching is enabled, the kernel may send read requests even if the
        // userspace program opened the file write-only. So we need to ensure that we have opened
        // the file for reading as well as writing.
        if writeback && flags & libc::O_ACCMODE == libc::O_WRONLY {
            new_flags &= !libc::O_ACCMODE;
            new_flags |= libc::O_RDWR;
        }

        // When writeback caching is enabled the kernel is responsible for handling `O_APPEND`.
        // However, this breaks atomicity as the file may have changed on disk, invalidating the
        // cached copy of the data in the kernel and the offset that the kernel thinks is the end of
        // the file. Just allow this for now as it is the user's responsibility to enable writeback
        // caching only for directories that are not shared. It also means that we need to clear the
        // `O_APPEND` flag.
        if writeback && flags & libc::O_APPEND != 0 {
            new_flags &= !libc::O_APPEND;
        }

        new_flags
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use fuse3::{MountOptions, raw::Session};
    use tokio::signal;

    use crate::passthrough::{PassthroughFs, config::Config, newlogfs::LoggingFileSystem};

    #[tokio::test]
    #[ignore]
    async fn test_passthrough() {
        let cfg = Config {
            xattr: false,
            do_import: true,
            root_dir: String::from("/home/luxian/github/buck2-rust-third-party"),
            ..Default::default()
        };

        let fs = PassthroughFs::<()>::new(cfg).unwrap();
        let logfs = LoggingFileSystem::new(fs);

        let mount_path = OsString::from("/home/luxian/pass");

        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let not_unprivileged = false;

        let mut mount_options = MountOptions::default();
        // .allow_other(true)
        mount_options.force_readdir_plus(true).uid(uid).gid(gid);

        let mut mount_handle: fuse3::raw::MountHandle = if !not_unprivileged {
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
