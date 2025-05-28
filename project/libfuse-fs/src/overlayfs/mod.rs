// Copyright (C) 2023 Ant Group. All rights reserved.
//  2024 From [fuse_backend_rs](https://github.com/cloud-hypervisor/fuse-backend-rs)
// SPDX-License-Identifier: Apache-2.0

#![allow(missing_docs)]
mod async_io;
pub mod config;
mod inode_store;
mod layer;
mod utils;

//mod tempfile;
use core::panic;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::{Error, ErrorKind, Result};

use config::Config;
use fuse3::raw::reply::{
    DirectoryEntry, DirectoryEntryPlus, ReplyAttr, ReplyEntry, ReplyOpen, ReplyStatFs,
};
use fuse3::raw::{Filesystem, Request, Session};
use futures::StreamExt as _;
use std::sync::{Arc, Weak};

use fuse3::{Errno, FileType, MountOptions, mode_from_kind_and_perm};
const SLASH_ASCII: char = '/';
use futures::future::join_all;
use futures::stream::iter;

use inode_store::InodeStore;
use layer::Layer;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::passthrough::newlogfs::LoggingFileSystem;
use crate::passthrough::{PassthroughFs, new_passthroughfs_layer};

use tokio::sync::{Mutex, RwLock};

pub type Inode = u64;
pub type Handle = u64;

type BoxedLayer = PassthroughFs;
//type BoxedFileSystem = Box<dyn FileSystem<Inode = Inode, Handle = Handle> + Send + Sync>;
const INODE_ALLOC_BATCH: u64 = 0x1_0000_0000;
// RealInode represents one inode object in specific layer.
// Also, each RealInode maps to one Entry, which should be 'forgotten' after drop.
// Important note: do not impl Clone trait for it or refcount will be messed up.
pub(crate) struct RealInode {
    pub layer: Arc<PassthroughFs>,
    pub in_upper_layer: bool,
    pub inode: u64,
    // File is whiteouted, we need to hide it.
    pub whiteout: bool,
    // Directory is opaque, we need to hide all entries inside it.
    pub opaque: bool,
    pub stat: Option<ReplyAttr>,
}

// OverlayInode must be protected by lock, it can be operated by multiple threads.
#[derive(Default)]
pub(crate) struct OverlayInode {
    // Inode hash table, map from 'name' to 'OverlayInode'.
    pub childrens: Mutex<HashMap<String, Arc<OverlayInode>>>,
    pub parent: Mutex<Weak<OverlayInode>>,
    // Backend inodes from all layers.
    pub real_inodes: Mutex<Vec<Arc<RealInode>>>,
    // Inode number.
    pub inode: u64,
    pub path: String,
    pub name: String,
    pub lookups: AtomicU64,
    // Node is whiteout-ed.
    pub whiteout: AtomicBool,
    // Directory is loaded.
    pub loaded: AtomicBool,
}

#[derive(Default)]
pub enum CachePolicy {
    Never,
    #[default]
    Auto,
    Always,
}
pub struct OverlayFs {
    config: Config,
    lower_layers: Vec<Arc<PassthroughFs>>,
    upper_layer: Option<Arc<PassthroughFs>>,
    // All inodes in FS.
    inodes: RwLock<InodeStore>,
    // Open file handles.
    handles: Mutex<HashMap<u64, Arc<HandleData>>>,
    next_handle: AtomicU64,
    writeback: AtomicBool,
    no_open: AtomicBool,
    no_opendir: AtomicBool,
    killpriv_v2: AtomicBool,
    perfile_dax: AtomicBool,
    root_inodes: u64,
}

struct RealHandle {
    layer: Arc<PassthroughFs>,
    in_upper_layer: bool,
    inode: u64,
    handle: AtomicU64,
}

struct HandleData {
    node: Arc<OverlayInode>,
    //offset: libc::off_t,
    real_handle: Option<RealHandle>,
}

// RealInode is a wrapper of one inode in specific layer.
// All layer operations returning Entry should be wrapped in RealInode implementation
// so that we can increase the refcount(lookup count) of each inode and decrease it after Drop.
// Important: do not impl 'Copy' trait for it or refcount will be messed up.
impl RealInode {
    async fn new(
        layer: Arc<PassthroughFs>,
        in_upper_layer: bool,
        inode: u64,
        whiteout: bool,
        opaque: bool,
    ) -> Self {
        let mut ri = RealInode {
            layer,
            in_upper_layer,
            inode,
            whiteout,
            opaque,
            stat: None,
        };
        match ri.stat64_ignore_enoent(&Request::default()).await {
            Ok(v) => {
                ri.stat = v;
            }
            Err(e) => {
                error!("stat64 failed during RealInode creation: {}", e);
            }
        }
        ri
    }

    async fn stat64(&self, req: &Request) -> Result<ReplyAttr> {
        let layer = self.layer.as_ref();
        if self.inode == 0 {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }
        layer
            .getattr(*req, self.inode, None, 0)
            .await
            .map_err(|e| e.into())
    }

    async fn stat64_ignore_enoent(&self, req: &Request) -> Result<Option<ReplyAttr>> {
        match self.stat64(req).await {
            Ok(v1) => Ok(Some(v1)),
            Err(e) => match e.raw_os_error() {
                Some(raw_error) => {
                    if raw_error != libc::ENOENT || raw_error != libc::ENAMETOOLONG {
                        return Ok(None);
                    }
                    Err(e)
                }
                None => Err(e),
            },
        }
    }

    // Do real lookup action in specific layer, this call will increase Entry refcount which must be released later.
    async fn lookup_child_ignore_enoent(
        &self,
        ctx: Request,
        name: &str,
    ) -> Result<Option<ReplyEntry>> {
        let cname = OsStr::new(name);
        // Real inode must have a layer.
        let layer = self.layer.as_ref();
        match layer.lookup(ctx, self.inode, cname).await {
            Ok(v) => {
                // Negative entry also indicates missing entry.
                if v.attr.ino == 0 {
                    return Ok(None);
                }
                Ok(Some(v))
            }
            Err(e) => {
                let ioerror: std::io::Error = e.into();
                if let Some(raw_error) = ioerror.raw_os_error() {
                    if raw_error == libc::ENOENT || raw_error == libc::ENAMETOOLONG {
                        return Ok(None);
                    }
                }
                Err(e.into())
            }
        }
    }

    // Find child inode in same layer under this directory(Self).
    // Return None if not found.
    async fn lookup_child(&self, ctx: Request, name: &str) -> Result<Option<RealInode>> {
        if self.whiteout {
            return Ok(None);
        }

        let layer = self.layer.as_ref();

        // Find child Entry with <name> under directory with inode <self.inode>.
        match self.lookup_child_ignore_enoent(ctx, name).await? {
            Some(v) => {
                // The Entry must be forgotten in each layer, which will be done automatically by Drop operation.
                let (whiteout, opaque) = if v.attr.kind == FileType::Directory {
                    (false, layer.is_opaque(ctx, v.attr.ino).await?)
                } else {
                    (layer.is_whiteout(ctx, v.attr.ino).await?, false)
                };

                Ok(Some(RealInode {
                    layer: self.layer.clone(),
                    in_upper_layer: self.in_upper_layer,
                    inode: v.attr.ino,
                    whiteout,
                    opaque,
                    stat: Some(ReplyAttr {
                        ttl: v.ttl,
                        attr: v.attr,
                    }),
                }))
            }
            None => Ok(None),
        }
    }

    // Read directory entries from specific RealInode, error out if it's not directory.
    async fn readdir(&self, ctx: Request) -> Result<HashMap<String, RealInode>> {
        // Deleted inode should not be read.
        if self.whiteout {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let stat = match self.stat.clone() {
            Some(v) => v,
            None => self.stat64(&ctx).await?,
        };

        // Must be directory.
        if stat.attr.kind != FileType::Directory {
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        // Open the directory and load each entry.
        let opendir_res = self
            .layer
            .opendir(ctx, self.inode, libc::O_RDONLY as u32)
            .await;
        let handle = match opendir_res {
            Ok(handle) => handle,

            // opendir may not be supported if no_opendir is set, so we can ignore this error.
            Err(e) => {
                let ioerror: std::io::Error = e.into();
                match ioerror.raw_os_error() {
                    Some(raw_error) => {
                        if raw_error == libc::ENOSYS {
                            // We can still call readdir with inode if opendir is not supported in this layer.
                            ReplyOpen { fh: 0, flags: 0 }
                        } else {
                            return Err(e.into());
                        }
                    }
                    None => {
                        return Err(e.into());
                    }
                }
            }
        };

        let child_names = self.layer.readdir(ctx, self.inode, handle.fh, 0).await?;
        // Non-zero handle indicates successful 'open', we should 'release' it.????? DIFFierent
        if handle.fh > 0 {
            self.layer
                .releasedir(ctx, self.inode, handle.fh, handle.flags)
                .await?
            //DIFF
        }

        // Lookup all child and construct "RealInode"s.
        let child_real_inodes = Arc::new(Mutex::new(HashMap::new()));

        let a_map = child_names.entries.map(|entery| async {
            match entery {
                Ok(dire) => {
                    let dname = dire.name.into_string().unwrap();
                    if let Some(child) = self.lookup_child(ctx, &dname).await.unwrap() {
                        child_real_inodes.lock().await.insert(dname, child);
                    }
                    Ok(())
                }
                Err(err) => Err(err),
            }
        });
        let k = join_all(a_map.collect::<Vec<_>>().await).await;
        drop(k);
        // Now into_inner func is safety.
        let re = Arc::try_unwrap(child_real_inodes)
            .map_err(|_| Errno::new_not_exist())?
            .into_inner();

        Ok(re)
    }

    async fn create_whiteout(&self, ctx: Request, name: &str) -> Result<RealInode> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // from &str to &OsStr
        let name_osstr = OsStr::new(name);
        let entry = self
            .layer
            .create_whiteout(ctx, self.inode, name_osstr)
            .await?;

        // Wrap whiteout to RealInode.
        Ok(RealInode {
            layer: self.layer.clone(),
            in_upper_layer: true,
            inode: entry.attr.ino,
            whiteout: true,
            opaque: false,
            stat: Some(ReplyAttr {
                ttl: entry.ttl,
                attr: entry.attr,
            }),
        })
    }

    async fn mkdir(&self, ctx: Request, name: &str, mode: u32, umask: u32) -> Result<RealInode> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        let name_osstr = OsStr::new(name);
        let entry = self
            .layer
            .mkdir(ctx, self.inode, name_osstr, mode, umask)
            .await?;

        // update node's first_layer
        Ok(RealInode {
            layer: self.layer.clone(),
            in_upper_layer: true,
            inode: entry.attr.ino,
            whiteout: false,
            opaque: false,
            stat: Some(ReplyAttr {
                ttl: entry.ttl,
                attr: entry.attr,
            }),
        })
    }

    async fn create(
        &self,
        ctx: Request,
        name: &str,
        mode: u32,
        flags: u32,
    ) -> Result<(RealInode, Option<u64>)> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }
        let name = OsStr::new(name);
        let create_rep = self
            .layer
            .create(ctx, self.inode, name, mode, flags)
            .await?;

        Ok((
            RealInode {
                layer: self.layer.clone(),
                in_upper_layer: true,
                inode: create_rep.attr.ino,
                whiteout: false,
                opaque: false,
                stat: Some(ReplyAttr {
                    ttl: create_rep.ttl,
                    attr: create_rep.attr,
                }),
            },
            Some(create_rep.fh),
        ))
    }

    async fn mknod(
        &self,
        ctx: Request,
        name: &str,
        mode: u32,
        rdev: u32,
        _umask: u32,
    ) -> Result<RealInode> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }
        let name = OsStr::new(name);
        let rep = self.layer.mknod(ctx, self.inode, name, mode, rdev).await?;
        Ok(RealInode {
            layer: self.layer.clone(),
            in_upper_layer: true,
            inode: rep.attr.ino,
            whiteout: false,
            opaque: false,
            stat: Some(ReplyAttr {
                ttl: rep.ttl,
                attr: rep.attr,
            }),
        })
    }

    async fn link(&self, ctx: Request, ino: u64, name: &str) -> Result<RealInode> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }
        let name = OsStr::new(name);
        let entry = self.layer.link(ctx, ino, self.inode, name).await?;

        let opaque = if utils::is_dir(&entry.attr.kind) {
            self.layer.is_opaque(ctx, entry.attr.ino).await?
        } else {
            false
        };
        Ok(RealInode {
            layer: self.layer.clone(),
            in_upper_layer: true,
            inode: entry.attr.ino,
            whiteout: false,
            opaque,
            stat: Some(ReplyAttr {
                ttl: entry.ttl,
                attr: entry.attr,
            }),
        })
    }

    // Create a symlink in self directory.
    async fn symlink(&self, ctx: Request, link_name: &str, filename: &str) -> Result<RealInode> {
        if !self.in_upper_layer {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }
        let link_name = OsStr::new(link_name);
        let filename = OsStr::new(filename);
        let entry = self
            .layer
            .symlink(ctx, self.inode, filename, link_name)
            .await?;

        Ok(RealInode {
            layer: self.layer.clone(),
            in_upper_layer: true,
            inode: entry.attr.ino,
            whiteout: false,
            opaque: false,
            stat: Some(ReplyAttr {
                ttl: entry.ttl,
                attr: entry.attr,
            }),
        })
    }
}

impl Drop for RealInode {
    fn drop(&mut self) {
        let layer = Arc::clone(&self.layer);
        let inode = self.inode;
        tokio::spawn(async move {
            let ctx = Request::default();
            layer.forget(ctx, inode, 1).await;
        });
    }
}

// impl AsyncDrop for RealInode {
//     async fn drop(self: std::pin::Pin<&mut Self>) {
//         // Release refcount of inode in layer.
//         let layer = self.layer.clone();
//         let inode = self.inode;
//         debug!("ASYNCDrop forget inode {} by 1 for backend inode in layer ", inode);
//         let ctx = Request::default();
//         layer.forget(ctx, inode, 1).await;
//     }
// }

impl OverlayInode {
    pub fn new() -> Self {
        OverlayInode::default()
    }
    // Allocate new OverlayInode based on one RealInode,
    // inode number is always 0 since only OverlayFs has global unique inode allocator.
    pub async fn new_from_real_inode(
        name: &str,
        ino: u64,
        path: String,
        real_inode: RealInode,
    ) -> Self {
        let mut new = OverlayInode::new();
        new.inode = ino;
        new.path = path;
        new.name = name.to_string();
        new.whiteout.store(real_inode.whiteout, Ordering::Relaxed);
        new.lookups = AtomicU64::new(1);
        new.real_inodes = Mutex::new(vec![real_inode.into()]);
        new
    }

    pub async fn new_from_real_inodes(
        name: &str,
        ino: u64,
        path: String,
        real_inodes: Vec<RealInode>,
    ) -> Result<Self> {
        if real_inodes.is_empty() {
            error!("BUG: new_from_real_inodes() called with empty real_inodes");
            return Err(Error::from_raw_os_error(libc::EINVAL));
        }

        let mut first = true;
        let mut new = Self::new();
        for ri in real_inodes {
            let whiteout = ri.whiteout;
            let opaque = ri.opaque;
            let stat = match &ri.stat {
                Some(v) => v.clone(),
                None => ri.stat64(&Request::default()).await?,
            };

            if first {
                first = false;
                new = Self::new_from_real_inode(name, ino, path.clone(), ri).await;

                // This is whiteout, no need to check lower layers.
                if whiteout {
                    break;
                }

                // A non-directory file shadows all lower layers as default.
                if !utils::is_dir(&stat.attr.kind) {
                    break;
                }

                // Opaque directory shadows all lower layers.
                if opaque {
                    break;
                }
            } else {
                // This is whiteout, no need to record this, break directly.
                if ri.whiteout {
                    break;
                }

                // Only directory have multiple real inodes, so if this is non-first real-inode
                // and it's not directory, it should indicates some invalid layout. @weizhang555
                if !utils::is_dir(&stat.attr.kind) {
                    error!("invalid layout: non-directory has multiple real inodes");
                    break;
                }

                // Valid directory.
                new.real_inodes.lock().await.push(ri.into());
                // Opaque directory shadows all lower layers.
                if opaque {
                    break;
                }
            }
        }
        Ok(new)
    }

    pub async fn stat64(&self, ctx: Request) -> Result<ReplyAttr> {
        // try layers in order or just take stat from first layer?
        for l in self.real_inodes.lock().await.iter() {
            if let Some(v) = l.stat64_ignore_enoent(&ctx).await? {
                return Ok(v);
            }
        }

        // not in any layer
        Err(Error::from_raw_os_error(libc::ENOENT))
    }

    pub async fn count_entries_and_whiteout(&self, ctx: Request) -> Result<(u64, u64)> {
        let mut count = 0;
        let mut whiteouts = 0;

        let st = self.stat64(ctx).await?;

        // must be directory
        if !utils::is_dir(&st.attr.kind) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        for (_, child) in self.childrens.lock().await.iter() {
            if child.whiteout.load(Ordering::Relaxed) {
                whiteouts += 1;
            } else {
                count += 1;
            }
        }

        Ok((count, whiteouts))
    }

    pub async fn open(
        &self,
        ctx: Request,
        flags: u32,
        _fuse_flags: u32,
    ) -> Result<(Arc<BoxedLayer>, ReplyOpen)> {
        let (layer, _, inode) = self.first_layer_inode().await;
        let ro = layer.as_ref().open(ctx, inode, flags).await?;
        Ok((layer, ro))
    }

    // Self is directory, fill all childrens.
    pub async fn scan_childrens(self: &Arc<Self>, ctx: Request) -> Result<Vec<OverlayInode>> {
        let st = self.stat64(ctx).await?;
        if !utils::is_dir(&st.attr.kind) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        let mut all_layer_inodes: HashMap<String, Vec<RealInode>> = HashMap::new();
        // read out directories from each layer
        let mut counter = 1;
        let layers_count = self.real_inodes.lock().await.len();
        // Scan from upper layer to lower layer.
        for ri in self.real_inodes.lock().await.iter() {
            debug!(
                "loading Layer {}/{} for dir '{}', is_upper_layer: {}",
                counter,
                layers_count,
                self.path.as_str(),
                ri.in_upper_layer
            );
            counter += 1;
            if ri.whiteout {
                // Node is deleted from some upper layer, skip it.
                debug!("directory is whiteout");
                break;
            }

            let stat = match &ri.stat {
                Some(v) => v.clone(),
                None => ri.stat64(&ctx).await?,
            };

            if !utils::is_dir(&stat.attr.kind) {
                debug!("{} is not a directory", self.path.as_str());
                // not directory
                break;
            }

            // Read all entries from one layer.
            let entries: HashMap<String, RealInode> = ri.readdir(ctx).await?;

            // Merge entries from one layer to all_layer_inodes.
            for (name, inode) in entries {
                if name == "." || name == ".." {
                    continue;
                }
                match all_layer_inodes.get_mut(&name) {
                    Some(v) => {
                        // Append additional RealInode to the end of vector.
                        v.push(inode)
                    }
                    None => {
                        all_layer_inodes.insert(name, vec![inode]);
                    }
                }
            }

            // if opaque, stop here
            if ri.opaque {
                debug!("directory {} is opaque", self.path.as_str());
                break;
            }
        }

        // Construct OverlayInode for each entry.
        let mut childrens = vec![];
        for (name, real_inodes) in all_layer_inodes {
            // Inode numbers are not allocated yet.
            let path = format!("{}/{}", self.path, name);
            let new = Self::new_from_real_inodes(name.as_str(), 0, path, real_inodes).await?;
            childrens.push(new);
        }

        Ok(childrens)
    }

    // Create a new directory in upper layer for node, node must be directory.
    pub async fn create_upper_dir(
        self: Arc<Self>,
        ctx: Request,
        mode_umask: Option<(u32, u32)>,
    ) -> Result<()> {
        let st = self.stat64(ctx).await?;
        if !utils::is_dir(&st.attr.kind) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        // If node already has upper layer, we can just return here.
        if self.in_upper_layer().await {
            return Ok(());
        }

        // not in upper layer, check parent.
        let pnode = if let Some(n) = self.parent.lock().await.upgrade() {
            Arc::clone(&n)
        } else {
            return Err(Error::new(ErrorKind::Other, "no parent?"));
        };

        if !pnode.in_upper_layer().await {
            Box::pin(pnode.clone().create_upper_dir(ctx, None)).await?; // recursive call
        }
        let child: Arc<Mutex<Option<RealInode>>> = Arc::new(Mutex::new(None));
        let c_name = self.name.clone();
        let _ = pnode
            .handle_upper_inode_locked(&mut |parent_upper_inode: Option<Arc<RealInode>>| async  {
                match parent_upper_inode {
                    Some(parent_ri) => {
                        let ri = match mode_umask {
                            Some((mode, umask)) => {
                                parent_ri
                                    .mkdir(ctx, &c_name, mode, umask)
                                    .await?
                            }
                            None => {
                                parent_ri
                                    .mkdir(
                                        ctx,
                                        &c_name,
                                        mode_from_kind_and_perm(st.attr.kind, st.attr.perm),
                                        0,
                                    )
                                    .await?
                            }
                        };
                        // create directory here
                        child.lock().await.replace(ri);
                    }
                    None => {
                        error!(
                            "BUG: parent {} has no upper inode after create_upper_dir",
                            pnode.inode
                        );
                        return Err(Error::from_raw_os_error(libc::EINVAL));
                    }
                }
                Ok(false)
            })
            .await?;

        if let Some(ri) = child.lock().await.take() {
            // Push the new real inode to the front of vector.
            self.add_upper_inode(ri, false).await;
        }

        Ok(())
    }

    // Add new upper RealInode to OverlayInode, clear all lower RealInodes if 'clear_lowers' is true.
    async fn add_upper_inode(self: &Arc<Self>, ri: RealInode, clear_lowers: bool) {
        let mut inodes = self.real_inodes.lock().await;
        // Update self according to upper attribute.
        self.whiteout.store(ri.whiteout, Ordering::Relaxed);

        // Push the new real inode to the front of vector.
        let mut new = vec![Arc::new(ri)];
        // Drain lower RealInodes.
        let lowers = inodes.drain(..).collect::<Vec<Arc<RealInode>>>();
        if !clear_lowers {
            // If not clear lowers, append them to the end of vector.
            new.extend(lowers);
        }
        inodes.extend(new);
    }

    // return the uppder layer fs.
    pub async fn in_upper_layer(&self) -> bool {
        let all_inodes = self.real_inodes.lock().await;
        let first = all_inodes.first();
        match first {
            Some(v) => v.in_upper_layer,
            None => false,
        }
    }

    pub async fn upper_layer_only(&self) -> bool {
        let real_inodes = self.real_inodes.lock().await;
        let first = real_inodes.first();
        match first {
            Some(v) => {
                if !v.in_upper_layer {
                    false
                } else {
                    real_inodes.len() == 1
                }
            }
            None => false,
        }
    }

    pub async fn first_layer_inode(&self) -> (Arc<BoxedLayer>, bool, u64) {
        let all_inodes = self.real_inodes.lock().await;
        let first = all_inodes.first();
        match first {
            Some(v) => (v.layer.clone(), v.in_upper_layer, v.inode),
            None => panic!("BUG: dangling OverlayInode"),
        }
    }

    pub async fn child(&self, name: &str) -> Option<Arc<OverlayInode>> {
        self.childrens.lock().await.get(name).cloned()
    }

    pub async fn remove_child(&self, name: &str) {
        self.childrens.lock().await.remove(name);
    }

    pub async fn insert_child(&self, name: &str, node: Arc<OverlayInode>) {
        self.childrens.lock().await.insert(name.to_string(), node);
    }

    /// Handles operations on the upper layer inode of an `OverlayInode` in a thread-safe manner.
    ///
    /// This function locks the `real_inodes` field of the `OverlayInode` and retrieves the first
    /// real inode (if any). If the first inode exists and belongs to the upper layer (`in_upper_layer` is true),
    /// the provided callback `f` is invoked with the inode wrapped in `Some`. Otherwise, `f` is invoked with `None`.
    ///
    /// # Arguments
    /// * `f`: A closure that takes an `Option<RealInode>` and returns a future. The future resolves to a `Result<bool>`.
    ///
    /// # Returns
    /// * `Ok(bool)`: The result of invoking the callback `f`.
    /// * `Err(Erron)`: An error is returned if:
    ///   - There are no backend inodes (`real_inodes` is empty), indicating a dangling `OverlayInode`.
    ///   - The callback `f` itself returns an error.
    ///
    /// # Behavior
    /// 1. Locks the `real_inodes` field to ensure thread safety.
    /// 2. Checks if the first inode exists:
    ///    - If it exists and is in the upper layer, invokes `f(Some(inode))`.
    ///    - If it exists but is not in the upper layer, invokes `f(None)`.
    /// 3. If no inodes exist, returns an error indicating a dangling `OverlayInode`.
    ///
    /// # Example Use Case
    /// This function is typically used to perform operations on the upper layer inode of an `OverlayInode`,
    /// such as creating, modifying, or deleting files/directories in the overlay filesystem's upper layer.
    pub async fn handle_upper_inode_locked<F, Fut>(
        &self,
        f: F,
    ) -> Result<bool>
    where
    // 对任意生命周期 'a，都能传一个 &RealInode（或 None）给 f
    F: FnOnce(Option<Arc<RealInode>>) -> Fut,
    // f 的返回 Future 最少要活到 'a
    Fut: Future<Output = Result<bool>> ,
    {
        let all_inodes = self.real_inodes.lock().await;
        let first = all_inodes.first();
        match first {
            Some(v) => {
                if v.in_upper_layer {
                    f(Some(v.clone())).await
                } else {
                    f(None).await
                }
            }
            None => Err(Error::new(
                ErrorKind::Other,
                format!(
                    "BUG: dangling OverlayInode {} without any backend inode",
                    self.inode
                ),
            )),
        }
    }
}
#[allow(unused)]
fn entry_type_from_mode(mode: libc::mode_t) -> u8 {
    match mode & libc::S_IFMT {
        libc::S_IFBLK => libc::DT_BLK,
        libc::S_IFCHR => libc::DT_CHR,
        libc::S_IFDIR => libc::DT_DIR,
        libc::S_IFIFO => libc::DT_FIFO,
        libc::S_IFLNK => libc::DT_LNK,
        libc::S_IFREG => libc::DT_REG,
        libc::S_IFSOCK => libc::DT_SOCK,
        _ => libc::DT_UNKNOWN,
    }
}
impl OverlayFs {
    pub fn new(
        upper: Option<Arc<BoxedLayer>>,
        lowers: Vec<Arc<BoxedLayer>>,
        params: Config,
        root_inode: u64,
    ) -> Result<Self> {
        // load root inode
        Ok(OverlayFs {
            config: params,
            lower_layers: lowers,
            upper_layer: upper,
            inodes: RwLock::new(InodeStore::new()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            writeback: AtomicBool::new(false),
            no_open: AtomicBool::new(false),
            no_opendir: AtomicBool::new(false),
            killpriv_v2: AtomicBool::new(false),
            perfile_dax: AtomicBool::new(false),
            root_inodes: root_inode,
        })
    }

    pub fn root_inode(&self) -> Inode {
        self.root_inodes
    }

    async fn alloc_inode(&self, path: &str) -> Result<u64> {
        self.inodes.write().await.alloc_inode(path)
    }

    /// Add a file layer and stack and merge the previous file layers.
    pub async fn push_layer(&mut self, layer: Arc<BoxedLayer>) -> Result<()> {
        let upper = self.upper_layer.take();
        if let Some(upper) = upper {
            self.lower_layers.push(upper);
        }
        self.upper_layer = Some(layer);
        // TODO: merge previous file layers. need optimization
        self.import().await?;
        Ok(())
    }

    pub async fn import(&self) -> Result<()> {
        let mut root = OverlayInode::new();
        root.inode = self.root_inode();
        root.path = String::from("");
        root.name = String::from("");
        root.lookups = AtomicU64::new(2);
        root.real_inodes = Mutex::new(vec![]);
        let ctx = Request::default();

        // Update upper inode
        if let Some(layer) = self.upper_layer.as_ref() {
            let ino = layer.root_inode();
            let real = RealInode::new(
                layer.clone(),
                true,
                ino,
                false,
                layer.is_opaque(ctx, ino).await?,
            )
            .await;
            root.real_inodes.lock().await.push(real.into());
        }

        // Update lower inodes.
        for layer in self.lower_layers.iter() {
            let ino = layer.root_inode();
            let real: RealInode = RealInode::new(
                layer.clone(),
                false,
                ino,
                false,
                layer.is_opaque(ctx, ino).await?,
            )
            .await;
            root.real_inodes.lock().await.push(real.into());
        }
        let root_node = Arc::new(root);

        // insert root inode into hash
        self.insert_inode(self.root_inode(), Arc::clone(&root_node))
            .await;

        info!("loading root directory\n");
        self.load_directory(ctx, &root_node).await?;

        Ok(())
    }

    async fn root_node(&self) -> Arc<OverlayInode> {
        // Root node must exist.
        self.get_active_inode(self.root_inode()).await.unwrap()
    }

    async fn insert_inode(&self, inode: u64, node: Arc<OverlayInode>) {
        self.inodes.write().await.insert_inode(inode, node);
    }

    async fn get_active_inode(&self, inode: u64) -> Option<Arc<OverlayInode>> {
        self.inodes.read().await.get_inode(inode)
    }

    // Get inode which is active or deleted.
    async fn get_all_inode(&self, inode: u64) -> Option<Arc<OverlayInode>> {
        let inode_store = self.inodes.read().await;
        match inode_store.get_inode(inode) {
            Some(n) => Some(n),
            None => inode_store.get_deleted_inode(inode),
        }
    }

    // Return the inode only if it's permanently deleted from both self.inodes and self.deleted_inodes.
    async fn remove_inode(
        &self,
        inode: u64,
        path_removed: Option<String>,
    ) -> Option<Arc<OverlayInode>> {
        self.inodes
            .write()
            .await
            .remove_inode(inode, path_removed)
            .await
    }

    // Lookup child OverlayInode with <name> under <parent> directory.
    // If name is empty, return parent itself.
    // Parent dir will be loaded, but returned OverlayInode won't.
    async fn lookup_node(
        &self,
        ctx: Request,
        parent: Inode,
        name: &str,
    ) -> Result<Arc<OverlayInode>> {
        if name.contains(SLASH_ASCII) {
            return Err(Error::from_raw_os_error(libc::EINVAL));
        }

        // Parent inode is expected to be loaded before this function is called.
        let pnode = match self.get_active_inode(parent).await {
            Some(v) => v,
            None => return Err(Error::from_raw_os_error(libc::ENOENT)),
        };

        // Parent is whiteout-ed, return ENOENT.
        if pnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let st = pnode.stat64(ctx).await?;
        if utils::is_dir(&st.attr.kind) && !pnode.loaded.load(Ordering::Relaxed) {
            // Parent is expected to be directory, load it first.
            self.load_directory(ctx, &pnode).await?;
        }

        // Current file or dir.
        if name.eq(".")  
            // Root directory has no parent.
            || (parent == self.root_inode() && name.eq("..")) 
            // Special convention: empty name indicates current dir.
            || name.is_empty()
        {
            return Ok(Arc::clone(&pnode));
        }

        match pnode.child(name).await {
            // Child is found.
            Some(v) => Ok(v),
            None => Err(Error::from_raw_os_error(libc::ENOENT)),
        }
    }

    async fn lookup_node_ignore_enoent(
        &self,
        ctx: Request,
        parent: u64,
        name: &str,
    ) -> Result<Option<Arc<OverlayInode>>> {
        match self.lookup_node(ctx, parent, name).await {
            Ok(n) => Ok(Some(Arc::clone(&n))),
            Err(e) => {
                if let Some(raw_error) = e.raw_os_error() {
                    if raw_error == libc::ENOENT {
                        return Ok(None);
                    }
                }
                Err(e)
            }
        }
    }

    // Load entries of the directory from all layers, if node is not directory, return directly.
    async fn load_directory(&self, ctx: Request, node: &Arc<OverlayInode>) -> Result<()> {
        if node.loaded.load(Ordering::Relaxed) {
            return Ok(());
        }

        // We got all childrens without inode.
        let childrens = node.scan_childrens(ctx).await?;

        // =============== Start Lock Area ===================
        // Lock OverlayFs inodes.
        let mut inode_store = self.inodes.write().await;
        // Lock the OverlayInode and its childrens.
        let mut node_children = node.childrens.lock().await;

        // Check again in case another 'load_directory' function call gets locks and want to do duplicated work.
        if node.loaded.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Now we have two locks' protection, Fs inodes lock and OverlayInode's childrens lock.
        for mut child in childrens.into_iter() {
            // Allocate inode for each child.
            let ino = inode_store.alloc_inode(&child.path)?;

            let name = child.name.clone();
            child.inode = ino;
            // Create bi-directional link between parent and child.
            child.parent = Mutex::new(Arc::downgrade(node));

            let arc_child = Arc::new(child);
            node_children.insert(name, arc_child.clone());
            // Record overlay inode in whole OverlayFs.
            inode_store.insert_inode(ino, arc_child);

        }

        node.loaded.store(true, Ordering::Relaxed);

        Ok(())
    }

    async fn forget_one(&self, inode: Inode, count: u64) {
        if inode == self.root_inode() || inode == 0 {
            return;
        }

        let v = match self.get_all_inode(inode).await {
            Some(n) => n,
            None => {
                trace!("forget unknown inode: {}", inode);
                return;
            }
        };

        // Use fetch_update to atomically update lookups in a loop until it succeeds
        let lookups = v.lookups.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            // If count is larger than current lookups, return 0
            // Otherwise subtract count from current lookups
            if current < count {
                Some(0)
            } else {
                Some(current - count) 
            }
        }).expect("fetch_update failed");

        if lookups == 0 {
            debug!("inode is forgotten: {}, name {}", inode, v.name);
            let _ = self.remove_inode(inode, None).await;
            let parent = v.parent.lock().await;

            if let Some(p) = parent.upgrade() {
                // remove it from hashmap
                p.remove_child(v.name.as_str()).await;
            }
        }
    }

    async fn do_lookup(&self, ctx: Request, parent: Inode, name: &str) -> Result<ReplyEntry> {
        let node = self.lookup_node(ctx, parent, name).await?;

        if node.whiteout.load(Ordering::Relaxed) {
            eprintln!("Error: node.whiteout.load() called.");
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let mut st = node.stat64(ctx).await?;
        st.attr.ino = node.inode;
        if utils::is_dir(&st.attr.kind) && !node.loaded.load(Ordering::Relaxed) {
            self.load_directory(ctx, &node).await?;
        }

        // FIXME: can forget happen between found and increase reference counter?
        let tmp =  node.lookups.fetch_add(1, Ordering::Relaxed);
        trace!("lookup count: {}", tmp + 1);
        Ok(ReplyEntry {
            ttl: st.ttl,
            attr: st.attr,
            generation: 0,
        })
    }

    async fn do_statvfs(&self, ctx: Request, inode: Inode) -> Result<ReplyStatFs> {
        match self.get_active_inode(inode).await {
            Some(ovi) => {
                let all_inodes = ovi.real_inodes.lock().await;
                let real_inode = all_inodes
                    .first()
                    .ok_or(Error::new(ErrorKind::Other, "backend inode not found"))?;
                Ok(real_inode.layer.statfs(ctx, real_inode.inode).await?)
            }
            None => Err(Error::from_raw_os_error(libc::ENOENT)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_readdir<'a>(
        &self,
        ctx: Request,
        inode: Inode,
        handle: u64,
        offset: u64,
    ) -> Result<<OverlayFs as fuse3::raw::Filesystem>::DirEntryStream<'a>> {
        // lookup the directory
        let ovl_inode = match self.handles.lock().await.get(&handle) {
            Some(dir) => dir.node.clone(),
            None => {
                // Try to get data with inode.
                let node = self.lookup_node(ctx, inode, ".").await?;

                let st = node.stat64(ctx).await?;
                if !utils::is_dir(&st.attr.kind) {
                    return Err(Error::from_raw_os_error(libc::ENOTDIR));
                }

                node.clone()
            }
        };
        self.load_directory(ctx, &ovl_inode).await?;
        let mut childrens = Vec::new();
        //add myself as "."
        childrens.push((".".to_string(), ovl_inode.clone()));

        //add parent
        let parent_node = match ovl_inode.parent.lock().await.upgrade() {
            Some(p) => p.clone(),
            None => self.root_node().await,
        };
        childrens.push(("..".to_string(), parent_node));

        for (_, child) in ovl_inode.childrens.lock().await.iter() {
            // skip whiteout node
            if child.whiteout.load(Ordering::Relaxed) {
                continue;
            }
            childrens.push((child.name.clone(), child.clone()));
        }

        if offset >= childrens.len() as u64 {
            return Ok(iter(vec![].into_iter()));
        }
        let mut d: Vec<std::result::Result<DirectoryEntry, Errno>> = Vec::new();

        for (index, (name, child)) in (0_u64..).zip(childrens.into_iter()) {
            // make struct DireEntry and Entry
            let st = child.stat64(ctx).await?;
            let dir_entry = DirectoryEntry {
                inode: child.inode,
                kind: st.attr.kind,
                name: name.into(),
                offset: (index + 1) as i64,
            };
            d.push(Ok(dir_entry));
        }

        Ok(iter(d.into_iter()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_readdirplus<'a>(
        &self,
        ctx: Request,
        inode: Inode,
        handle: u64,
        offset: u64,
    ) -> Result<<OverlayFs as fuse3::raw::Filesystem>::DirEntryPlusStream<'a>> {
        // lookup the directory
        let ovl_inode = match self.handles.lock().await.get(&handle) {
            Some(dir) => dir.node.clone(),
            None => {
                // Try to get data with inode.
                let node = self.lookup_node(ctx, inode, ".").await?;

                let st = node.stat64(ctx).await?;
                if !utils::is_dir(&st.attr.kind) {
                    return Err(Error::from_raw_os_error(libc::ENOTDIR));
                }

                node.clone()
            }
        };
        self.load_directory(ctx, &ovl_inode).await?;

        let mut childrens = Vec::new();
        //add myself as "."
        childrens.push((".".to_string(), ovl_inode.clone()));

        //add parent
        let parent_node = match ovl_inode.parent.lock().await.upgrade() {
            Some(p) => p.clone(),
            None => self.root_node().await,
        };
        childrens.push(("..".to_string(), parent_node));

        for (_, child) in ovl_inode.childrens.lock().await.iter() {
            // skip whiteout node
            if child.whiteout.load(Ordering::Relaxed) {
                continue;
            }
            childrens.push((child.name.clone(), child.clone()));
        }

        if offset >= childrens.len() as u64 {
            return Ok(iter(vec![].into_iter()));
        }
        let mut d: Vec<std::result::Result<DirectoryEntryPlus, Errno>> = Vec::new();

        for (index, (name, child)) in (0_u64..).zip(childrens.into_iter()) {
            if index >= offset {
                // make struct DireEntry and Entry
                let mut st = child.stat64(ctx).await?;
                child.lookups.fetch_add(1,Ordering::Relaxed);
                st.attr.ino = child.inode;
                let dir_entry = DirectoryEntryPlus {
                    inode: child.inode,
                    generation: 0,
                    kind: st.attr.kind,
                    name: name.into(),
                    offset: (index + 1) as i64,
                    attr: st.attr,
                    entry_ttl: st.ttl,
                    attr_ttl: st.ttl,
                };
                d.push(Ok(dir_entry));
            }
        }

        Ok(iter(d.into_iter()))
    }
    async fn do_mkdir(
        &self,
        ctx: Request,
        parent_node: Arc<OverlayInode>,
        name: &str,
        mode: u32,
        umask: u32,
    ) -> Result<()> {
        if self.upper_layer.is_none() {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // Parent node was deleted.
        if parent_node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let mut delete_whiteout = false;
        let mut set_opaque = false;
        if let Some(n) = self
            .lookup_node_ignore_enoent(ctx, parent_node.inode, name)
            .await?
        {
            // Node with same name exists, let's check if it's whiteout.
            if !n.whiteout.load(Ordering::Relaxed) {
                return Err(Error::from_raw_os_error(libc::EEXIST));
            }

            if n.in_upper_layer().await {
                delete_whiteout = true;
            }

            // Set opaque if child dir has lower layers.
            if !n.upper_layer_only().await {
                set_opaque = true;
            }
        }

        // Copy parent node up if necessary.
        let pnode = self.copy_node_up(ctx, parent_node).await?;

        let path = format!("{}/{}", pnode.path, name);
        let path_ref = &path;
        let new_node = Arc::new(Mutex::new(None));
        pnode
            .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                let parent_real_inode = match parent_real_inode {
                    Some(inode) => inode,
                    None => {
                        error!("BUG: parent doesn't have upper inode after copied up");
                        return Err(Error::from_raw_os_error(libc::EINVAL));
                    }
                };
                let osstr = OsStr::new(name);
                if delete_whiteout {
                    let _ = parent_real_inode
                        .layer
                        .delete_whiteout(ctx, parent_real_inode.inode, osstr)
                        .await;
                }

                // Allocate inode number.
                let ino = self.alloc_inode(path_ref).await?;
                let child_dir = parent_real_inode.mkdir(ctx, name, mode, umask).await?;
                // Set opaque if child dir has lower layers.
                if set_opaque {
                    parent_real_inode
                        .layer
                        .set_opaque(ctx, child_dir.inode)
                        .await?;
                }
                let ovi =
                    OverlayInode::new_from_real_inode(name, ino, path_ref.clone(), child_dir).await;
                new_node.lock().await.replace(ovi);
                Ok(false)
            })
            .await?;

        // new_node is always 'Some'
        let nn = new_node.lock().await.take();
        let arc_node = Arc::new(nn.unwrap());
        self.insert_inode(arc_node.inode, arc_node.clone()).await;
        pnode.insert_child(name, arc_node).await;
        Ok(())
    }

    async fn do_mknod(
        &self,
        ctx: Request,
        parent_node: &Arc<OverlayInode>,
        name: &str,
        mode: u32,
        rdev: u32,
        umask: u32,
    ) -> Result<()> {
        if self.upper_layer.is_none() {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // Parent node was deleted.
        if parent_node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        match self
            .lookup_node_ignore_enoent(ctx, parent_node.inode, name)
            .await?
        {
            Some(n) => {
                // Node with same name exists, let's check if it's whiteout.
                if !n.whiteout.load(Ordering::Relaxed) {
                    return Err(Error::from_raw_os_error(libc::EEXIST));
                }

                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };
                        let osstr = OsStr::new(name);
                        if n.in_upper_layer().await {
                            let _ = parent_real_inode
                                .layer
                                .delete_whiteout(ctx, parent_real_inode.inode, osstr)
                                .await;
                        }

                        let child_ri = parent_real_inode
                            .mknod(ctx, name, mode, rdev, umask)
                            .await?;

                        // Replace existing real inodes with new one.
                        n.add_upper_inode(child_ri, true).await;
                        Ok(false)
                    })
                    .await?;
            }
            None => {
                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                let new_node = Arc::new(Mutex::new(None));
                let path = format!("{}/{}", pnode.path, name);
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        // Allocate inode number.
                        let ino = self.alloc_inode(&path).await?;
                        let child_ri = parent_real_inode
                            .mknod(ctx, name, mode, rdev, umask)
                            .await?;
                        let ovi =
                            OverlayInode::new_from_real_inode(name, ino, path.clone(), child_ri)
                                .await;

                        new_node.lock().await.replace(ovi);
                        Ok(false)
                    })
                    .await?;

                let nn = new_node.lock().await.take();
                let arc_node = Arc::new(nn.unwrap());
                self.insert_inode(arc_node.inode, arc_node.clone()).await;
                pnode.insert_child(name, arc_node).await;
            }
        }

        Ok(())
    }

    async fn do_create(
        &self,
        ctx: Request,
        parent_node: &Arc<OverlayInode>,
        name: &OsStr,
        mode: u32,
        flags: u32,
    ) -> Result<Option<u64>> {
        let name_str = name.to_str().unwrap();
        let upper = self
            .upper_layer
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::from_raw_os_error(libc::EROFS))?;

        // Parent node was deleted.
        if parent_node.whiteout.load(Ordering::Relaxed){
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let handle: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let real_ino: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let new_ovi = match self
            .lookup_node_ignore_enoent(ctx, parent_node.inode, name_str)
            .await?
        {
            Some(n) => {
                // Node with same name exists, let's check if it's whiteout.
                if !n.whiteout.load(Ordering::Relaxed){
                    return Err(Error::from_raw_os_error(libc::EEXIST));
                }

                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        if n.in_upper_layer().await {
                            let _ = parent_real_inode
                                .layer
                                .delete_whiteout(ctx, parent_real_inode.inode, name)
                                .await;
                        }

                        let (child_ri, hd) =
                            parent_real_inode.create(ctx, name_str, mode, flags).await?;
                        real_ino.lock().await.replace(child_ri.inode);
                        handle.lock().await.replace(hd.unwrap());

                        // Replace existing real inodes with new one.
                        n.add_upper_inode(child_ri, true).await;
                        Ok(false)
                    })
                    .await?;
                n.clone()
            }
            None => {
                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                let new_node = Arc::new(Mutex::new(None));
                let path = format!("{}/{}", pnode.path, name_str);
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        let (child_ri, hd) =
                            parent_real_inode.create(ctx, name_str, mode, flags).await?;
                        real_ino.lock().await.replace(child_ri.inode);
                        handle.lock().await.replace(hd.unwrap());
                        // Allocate inode number.
                        let ino = self.alloc_inode(&path).await?;
                        let ovi = OverlayInode::new_from_real_inode(
                            name_str,
                            ino,
                            path.clone(),
                            child_ri,
                        )
                        .await;

                        new_node.lock().await.replace(ovi);
                        Ok(false)
                    })
                    .await?;

                // new_node is always 'Some'
                let nn = new_node.lock().await.take();
                let arc_node = Arc::new(nn.unwrap());
                self.insert_inode(arc_node.inode, arc_node.clone()).await;
                pnode.insert_child(name_str, arc_node.clone()).await;
                arc_node
            }
        };

        let final_handle = match *handle.lock().await {
            Some(hd) => {
                if self.no_open.load(Ordering::Relaxed) {
                    None
                } else {
                    let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
                    let handle_data = HandleData {
                        node: new_ovi,
                        real_handle: Some(RealHandle {
                            layer: upper.clone(),
                            in_upper_layer: true,
                            inode: real_ino.lock().await.unwrap(),
                            handle: AtomicU64::new(hd),
                        }),
                    };
                    self.handles
                        .lock()
                        .await
                        .insert(handle, Arc::new(handle_data));
                    Some(handle)
                }
            }
            None => None,
        };
        Ok(final_handle)
    }

    async fn do_rename(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        let name_str = name.to_str().unwrap();
        let new_name_str = new_name.to_str().unwrap();

        // Ensure parent exists
        let parent_node = match self.get_all_inode(parent).await {
            Some(node) => node,
            None => return Err(Error::from_raw_os_error(libc::ENOENT)),
        };

        //  Ensure name exists
        let src_node = self.lookup_node(req, parent, name_str).await?;
        let src_node_attr = src_node.stat64(req).await?;
        let src_node_type = src_node_attr.attr.kind;

        // Ensure new_parent is a directory
        let new_parent_node = match self.get_all_inode(new_parent).await {
            Some(node) => node,
            None => return Err(Error::from_raw_os_error(libc::ENOENT)),
        };

        let new_parent_type = new_parent_node.stat64(req).await?.attr.kind;
        if !utils::is_dir(&new_parent_type) {
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        // Ensure new_name does not exist in new_parent
        // Check if new_name exists in new_parent
        if let Some(existing_node) = self
            .lookup_node_ignore_enoent(req, new_parent, new_name_str)
            .await?
        {
            // If the node exists and is not whiteout, return EEXIST
            if !existing_node.whiteout.load(Ordering::Relaxed) {
                return Err(Error::from_raw_os_error(libc::EEXIST));
            }
            // If it's a whiteout, allow rename to proceed (overwrite whiteout)
        }

        if src_node_type == FileType::Directory {
            // Directory can't be renamed.
            self.copy_directory_up(req, src_node.clone()).await?;
        } else {
            self.copy_node_up(req, src_node.clone()).await?;
        }

        // copy new_parent up if necessary.
        self.copy_node_up(req, new_parent_node.clone()).await?;
        let (src_lay, src_t, src_true_inode) = parent_node.first_layer_inode().await;
        let (dst_lay, dst_t, dst_true_inode) = new_parent_node.first_layer_inode().await;

        // Assert that both layers are the same.
        assert!(Arc::ptr_eq(&src_lay, &dst_lay));
        assert!(src_t);
        assert!(dst_t);

        // try to delete whiteout if exist.
        let _ = src_lay.delete_whiteout(req, dst_true_inode, new_name).await;
        // rename in PassthroughFS.
        src_lay
            .rename(req, src_true_inode, name, dst_true_inode, new_name)
            .await?;

        // create whiteout or set_opaque for befor inode.
        // TODO: After renaming, check if lower layers have an entry with the same name.
        // If so, create whiteout (for files) or set opaque (for directories) in the destination.
        let _ = src_lay.create_whiteout(req, src_true_inode, name).await;

        // Mark parent node as dirty.
        parent_node.loaded.store(false,Ordering::Relaxed);
        new_parent_node.loaded.store(false,Ordering::Relaxed);
        Ok(())
    }

    async fn do_link(
        &self,
        ctx: Request,
        src_node: &Arc<OverlayInode>,
        new_parent: &Arc<OverlayInode>,
        name: &str,
    ) -> Result<()> {
        let name_os = OsStr::new(name);
        if self.upper_layer.is_none() {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // Node is whiteout.
        if src_node.whiteout.load(Ordering::Relaxed) || new_parent.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        let st = src_node.stat64(ctx).await?;
        if utils::is_dir(&st.attr.kind) {
            // Directory can't be hardlinked.
            return Err(Error::from_raw_os_error(libc::EPERM));
        }

        let src_node = self.copy_node_up(ctx, Arc::clone(src_node)).await?;
        let new_parent = self.copy_node_up(ctx, Arc::clone(new_parent)).await?;
        let src_ino = src_node.first_layer_inode().await.2;

        match self
            .lookup_node_ignore_enoent(ctx, new_parent.inode, name)
            .await?
        {
            Some(n) => {
                // Node with same name exists, let's check if it's whiteout.
                if !n.whiteout.load(Ordering::Relaxed) {
                    return Err(Error::from_raw_os_error(libc::EEXIST));
                }

                // Node is definitely a whiteout now.
                new_parent
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        // Whiteout file exists in upper level, let's delete it.
                        if n.in_upper_layer().await {
                            let _ = parent_real_inode
                                .layer
                                .delete_whiteout(ctx, parent_real_inode.inode, name_os)
                                .await;
                        }

                        let child_ri = parent_real_inode.link(ctx, src_ino, name).await?;

                        // Replace existing real inodes with new one.
                        n.add_upper_inode(child_ri, true).await;
                        Ok(false)
                    })
                    .await?;
            }
            None => {
                // Copy parent node up if necessary.
                let new_node: Arc<Mutex<Option<OverlayInode>>> = Arc::new(Mutex::new(None));
                new_parent
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        // Allocate inode number.
                        let path = format!("{}/{}", new_parent.path, name);
                        let ino = self.alloc_inode(&path).await?;
                        let child_ri = parent_real_inode.link(ctx, src_ino, name).await?;
                        let ovi =
                            OverlayInode::new_from_real_inode(name, ino, path, child_ri).await;

                        new_node.lock().await.replace(ovi);
                        Ok(false)
                    })
                    .await?;

                // new_node is always 'Some'
                let arc_node = Arc::new(new_node.lock().await.take().unwrap());
                self.insert_inode(arc_node.inode, arc_node.clone()).await;
                new_parent.insert_child(name, arc_node).await;
            }
        }

        Ok(())
    }

    async fn do_symlink(
        &self,
        ctx: Request,
        linkname: &str,
        parent_node: &Arc<OverlayInode>,
        name: &str,
    ) -> Result<()> {
        let name_os = OsStr::new(name);
        if self.upper_layer.is_none() {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // parent was deleted.
        if parent_node.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        match self
            .lookup_node_ignore_enoent(ctx, parent_node.inode, name)
            .await?
        {
            Some(n) => {
                // Node with same name exists, let's check if it's whiteout.
                if !n.whiteout.load(Ordering::Relaxed) {
                    return Err(Error::from_raw_os_error(libc::EEXIST));
                }

                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        if n.in_upper_layer().await {
                            let _ = parent_real_inode
                                .layer
                                .delete_whiteout(ctx, parent_real_inode.inode, name_os)
                                .await;
                        }

                        let child_ri = parent_real_inode.symlink(ctx, linkname, name).await?;

                        // Replace existing real inodes with new one.
                        n.add_upper_inode(child_ri, true).await;
                        Ok(false)
                    })
                    .await?;
            }
            None => {
                // Copy parent node up if necessary.
                let pnode = self.copy_node_up(ctx, Arc::clone(parent_node)).await?;
                let new_node: Arc<Mutex<Option<OverlayInode>>> = Arc::new(Mutex::new(None));
                let path = format!("{}/{}", pnode.path, name);
                pnode
                    .handle_upper_inode_locked(&mut |parent_real_inode:  Option<Arc<RealInode>>| async {
                        let parent_real_inode = match parent_real_inode {
                            Some(inode) => inode,
                            None => {
                                error!("BUG: parent doesn't have upper inode after copied up");
                                return Err(Error::from_raw_os_error(libc::EINVAL));
                            }
                        };

                        // Allocate inode number.
                        let ino = self.alloc_inode(&path).await?;
                        let child_ri = parent_real_inode.symlink(ctx, linkname, name).await?;
                        let ovi =
                            OverlayInode::new_from_real_inode(name, ino, path.clone(), child_ri)
                                .await;

                        new_node.lock().await.replace(ovi);
                        Ok(false)
                    })
                    .await?;

                // new_node is always 'Some'
                let arc_node = Arc::new(new_node.lock().await.take().unwrap());
                self.insert_inode(arc_node.inode, arc_node.clone()).await;
                pnode.insert_child(name, arc_node).await;
            }
        }

        Ok(())
    }

    async fn copy_symlink_up(
        &self,
        ctx: Request,
        node: Arc<OverlayInode>,
    ) -> Result<Arc<OverlayInode>> {
        if node.in_upper_layer().await {
            return Ok(node);
        }

        let parent_node = if let Some(ref n) = node.parent.lock().await.upgrade() {
            Arc::clone(n)
        } else {
            return Err(Error::new(ErrorKind::Other, "no parent?"));
        };

        let (self_layer, _, self_inode) = node.first_layer_inode().await;

        if !parent_node.in_upper_layer().await {
            parent_node.clone().create_upper_dir(ctx, None).await?;
        }

        // Read the linkname from lower layer.
        let reply_data = self_layer.readlink(ctx, self_inode).await?;
        // Convert path to &str.
        let path = std::str::from_utf8(&reply_data.data)
            .map_err(|_| Error::from_raw_os_error(libc::EINVAL))?;

        let new_upper_real: Arc<Mutex<Option<RealInode>>> = Arc::new(Mutex::new(None));
        parent_node
            .handle_upper_inode_locked(&mut |parent_upper_inode:  Option<Arc<RealInode>>| async {
                // We already create upper dir for parent_node above.
                let parent_real_inode =
                    parent_upper_inode.ok_or_else(|| Error::from_raw_os_error(libc::EROFS))?;
                new_upper_real.lock().await.replace(
                    parent_real_inode
                        .symlink(ctx, path, node.name.as_str())
                        .await?,
                );
                Ok(false)
            })
            .await?;

        if let Some(real_inode) = new_upper_real.lock().await.take() {
            // update upper_inode and first_inode()
            node.add_upper_inode(real_inode, true).await;
        }

        Ok(node)
    }

    // Copy regular file from lower layer to upper layer.
    // Caller must ensure node doesn't have upper layer.
    async fn copy_regfile_up(
        &self,
        ctx: Request,
        node: Arc<OverlayInode>,
    ) -> Result<Arc<OverlayInode>> {
        if node.in_upper_layer().await {
            return Ok(node);
        }
        //error...
        let parent_node = if let Some(ref n) = node.parent.lock().await.upgrade() {
            Arc::clone(n)
        } else {
            return Err(Error::new(ErrorKind::Other, "no parent?"));
        };

        let st = node.stat64(ctx).await?;
        let (lower_layer, _, lower_inode) = node.first_layer_inode().await;

        if !parent_node.in_upper_layer().await {
            parent_node.clone().create_upper_dir(ctx, None).await?;
        }

        // create the file in upper layer using information from lower layer

        let flags = libc::O_WRONLY;
        let mode = mode_from_kind_and_perm(st.attr.kind, st.attr.perm);

        let upper_handle = Arc::new(Mutex::new(0));
        let upper_real_inode = Arc::new(Mutex::new(None));
        parent_node
            .handle_upper_inode_locked(&mut |parent_upper_inode:  Option<Arc<RealInode>>| async {
                // We already create upper dir for parent_node.
                let parent_real_inode = parent_upper_inode.ok_or_else(|| {
                    error!("parent {} has no upper inode", parent_node.inode);
                    Error::from_raw_os_error(libc::EINVAL)
                })?;
                let (inode, h) = parent_real_inode
                    .create(ctx, node.name.as_str(), mode, flags.try_into().unwrap())
                    .await?;
                *upper_handle.lock().await = h.unwrap_or(0);
                upper_real_inode.lock().await.replace(inode);
                Ok(false)
            })
            .await?;

        let rep = lower_layer
            .open(ctx, lower_inode, libc::O_RDONLY as u32)
            .await?;

        let lower_handle = rep.fh;

        // need to use work directory and then rename file to
        // final destination for atomic reasons.. not deal with it for now,
        // use stupid copy at present.
        // FIXME: this need a lot of work here, ntimes, xattr, etc.

        // Copy from lower real inode to upper real inode.
        // TODO: use sendfile here.

        let mut _offset: usize = 0;
        let size = 4 * 1024 * 1024;

        let ret = lower_layer
            .read(ctx, lower_inode, lower_handle, _offset as u64, size)
            .await?;

        _offset += ret.data.len();

        // close handles
        lower_layer
            .release(ctx, lower_inode, lower_handle, 0, 0, true)
            .await?;

        _offset = 0;
        let u_handle = *upper_handle.lock().await;
        while let Some(ref ri) = upper_real_inode.lock().await.take() {
            let ret = ri
                .layer
                .write(ctx, ri.inode, u_handle, _offset as u64, &ret.data, 0, 0)
                .await?;
            if ret.written == 0 {
                break;
            }

            _offset += ret.written as usize;
        }

        if let Some(ri) = upper_real_inode.lock().await.take() {
            if let Err(e) = ri.layer.release(ctx, ri.inode, u_handle, 0, 0, true).await {
                let e: std::io::Error = e.into();
                // Ignore ENOSYS.
                if e.raw_os_error() != Some(libc::ENOSYS) {
                    return Err(e);
                }
            }

            // update upper_inode and first_inode()
            node.add_upper_inode(ri, true).await;
        }

        Ok(node)
    }

    /// Copies the specified node to the upper layer of the filesystem
    ///
    /// Performs different operations based on the node type:
    /// - **Directory**: Creates a corresponding directory in the upper layer
    /// - **Symbolic link**: Recursively copies to the upper layer
    /// - **Regular file**: Copies file content to the upper layer
    ///
    /// # Parameters
    /// * `ctx`: FUSE request context
    /// * `node`: Reference to the node to be copied
    ///
    /// # Returns
    /// Returns a reference to the upper-layer node on success, or an error on failure
    async fn copy_node_up(
        &self,
        ctx: Request,
        node: Arc<OverlayInode>,
    ) -> Result<Arc<OverlayInode>> {
        if node.in_upper_layer().await {
            return Ok(node);
        }

        let st = node.stat64(ctx).await?;
        match st.attr.kind {
            FileType::Directory => {
                node.clone().create_upper_dir(ctx, None).await?;
                Ok(node)
            }
            FileType::Symlink => {
                // For symlink.
                self.copy_symlink_up(ctx, node).await
            }
            FileType::RegularFile => {
                // For regular file.
                self.copy_regfile_up(ctx, node).await
            }
            _ => {
                // For other file types. return error.
                Err(Error::from_raw_os_error(libc::EINVAL))
            }
        }
    }

    /// recursively copy directory and all its contents to upper layer
    async fn copy_directory_up(
        &self,
        ctx: Request,
        node: Arc<OverlayInode>,
    ) -> Result<Arc<OverlayInode>> {
        // Ensure the directory itself is copied up first
        self.copy_node_up(ctx, node.clone()).await?;

        // load directory to cache
        self.load_directory(ctx, &node).await?;

        // go through all children
        let children = node.childrens.lock().await.clone();
        for (_name, child) in children.iter() {
            println!("{}", _name);
            if child.name == "." || child.name == ".." {
                continue;
            }
            // jump over whiteout
            if child.whiteout.load(Ordering::Relaxed) {
                continue;
            }
            let st = child.stat64(ctx).await?;
            if !child.in_upper_layer().await {
                match st.attr.kind {
                    FileType::Directory => {
                        // recursively copy subdirectory
                        Box::pin(self.copy_directory_up(ctx, child.clone())).await?;
                    }
                    FileType::Symlink | FileType::RegularFile => {
                        // copy node up symlink or regular file
                        Box::pin(self.copy_node_up(ctx, child.clone())).await?;
                    }
                    _ => {
                        // other file types are ignored
                    }
                }
            } else if utils::is_dir(&st.attr.kind) {
                // If it is already in the upper layer, but the directory is not loaded,
                // ensure that its contents are also copied up recursively.
                Box::pin(self.copy_directory_up(ctx, child.clone())).await?;
            }
        }

        Ok(node)
    }

    async fn do_rm(&self, ctx: Request, parent: u64, name: &OsStr, dir: bool) -> Result<()> {
        if self.upper_layer.is_none() {
            return Err(Error::from_raw_os_error(libc::EROFS));
        }

        // Find parent Overlay Inode.
        let pnode = self.lookup_node(ctx, parent, "").await?;
        if pnode.whiteout.load(Ordering::Relaxed) {
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }
        let to_name = name.to_str().unwrap();
        // Find the Overlay Inode for child with <name>.
        let node = self.lookup_node(ctx, parent, to_name).await?;
        if node.whiteout.load(Ordering::Relaxed) {
            // already deleted.
            return Err(Error::from_raw_os_error(libc::ENOENT));
        }

        if dir {
            self.load_directory(ctx, &node).await?;
            let (count, whiteouts) = node.count_entries_and_whiteout(ctx).await?;
            trace!("entries: {}, whiteouts: {}\n", count, whiteouts);
            if count > 0 {
                return Err(Error::from_raw_os_error(libc::ENOTEMPTY));
            }

            // Delete all whiteouts.
            if whiteouts > 0 && node.in_upper_layer().await {
                self.empty_node_directory(ctx, Arc::clone(&node)).await?;
            }

            trace!("whiteouts deleted!\n");
        }

        let need_whiteout = Arc::new(Mutex::new(true));
        let pnode = self.copy_node_up(ctx, Arc::clone(&pnode)).await?;

        if node.upper_layer_only().await {
            *need_whiteout.lock().await = false;
        }

        
        // lookups decrease by 1.
        node.lookups.fetch_sub(1, Ordering::Relaxed);
        let mut df =  |parent_upper_inode:  Option<Arc<RealInode>>| async {
            let parent_real_inode = parent_upper_inode.ok_or_else(|| {
                error!(
                    "BUG: parent {} has no upper inode after copy up",
                    pnode.inode
                );
                Error::from_raw_os_error(libc::EINVAL)
            })?;

            // Parent is opaque, it shadows everything in lower layers so no need to create extra whiteouts.
            if parent_real_inode.opaque {
                *need_whiteout.lock().await = false;
            }
            if dir {
                parent_real_inode
                    .layer
                    .rmdir(ctx, parent_real_inode.inode, name)
                    .await?;
            } else {
                parent_real_inode
                    .layer
                    .unlink(ctx, parent_real_inode.inode, name)
                    .await?;
            }

            Ok(false)
        };

        if node.in_upper_layer().await {
           
            let hh = pnode.handle_upper_inode_locked(&mut df);
            // remove it from hashmap
            let ops = tokio::join!(
                hh,
                self.remove_inode(node.inode, Some(node.path.clone())), 
                pnode.remove_child(node.name.as_str()),
               
            );
           
            
        }else{
              let ops = tokio::join!(
                self.remove_inode(node.inode, None), 
                pnode.remove_child(node.name.as_str())
            );
        }


        trace!(
            "Remove inode {} from global hashmap and parent's children hashmap\n",
            node.inode
        );

        
        if *need_whiteout.lock().await {
            trace!("do_rm: creating whiteout\n");
            // pnode is copied up, so it has upper layer.
            pnode
                .handle_upper_inode_locked(&mut |parent_upper_inode:  Option<Arc<RealInode>>| async {
                    let parent_real_inode = parent_upper_inode.ok_or_else(|| {
                        error!(
                            "BUG: parent {} has no upper inode after copy up",
                            pnode.inode
                        );
                        Error::from_raw_os_error(libc::EINVAL)
                    })?;

                    let child_ri = parent_real_inode.create_whiteout(ctx, to_name).await?; //FIXME..............
                    let path = format!("{}/{}", pnode.path, to_name);
                    let ino: u64 = self.alloc_inode(&path).await?;
                    let ovi = Arc::new(
                        OverlayInode::new_from_real_inode(to_name, ino, path.clone(), child_ri)
                            .await,
                    );

                    self.insert_inode(ino, ovi.clone()).await;
                    pnode.insert_child(to_name, ovi.clone()).await;
                    Ok(false)
                })
                .await?;
        }

        Ok(())
    }

    async fn do_fsync(
        &self,
        ctx: Request,
        inode: Inode,
        datasync: bool,
        handle: Handle,
        syncdir: bool,
    ) -> Result<()> {
        // Use O_RDONLY flags which indicates no copy up.
        let data = self
            .get_data(ctx, Some(handle), inode, libc::O_RDONLY as u32)
            .await?;

        match data.real_handle {
            // FIXME: need to test if inode matches corresponding handle?
            None => Err(Error::from_raw_os_error(libc::ENOENT)),
            Some(ref rh) => {
                let real_handle = rh.handle.load(Ordering::Relaxed);
                // TODO: check if it's in upper layer? @weizhang555
                if syncdir {
                    rh.layer
                        .fsyncdir(ctx, rh.inode, real_handle, datasync)
                        .await
                        .map_err(|e| e.into())
                } else {
                    rh.layer
                        .fsync(ctx, rh.inode, real_handle, datasync)
                        .await
                        .map_err(|e| e.into())
                }
            }
        }
    }

    // Delete everything in the directory only on upper layer, ignore lower layers.
    async fn empty_node_directory(&self, ctx: Request, node: Arc<OverlayInode>) -> Result<()> {
        let st = node.stat64(ctx).await?;
        if !utils::is_dir(&st.attr.kind) {
            // This function can only be called on directories.
            return Err(Error::from_raw_os_error(libc::ENOTDIR));
        }

        let (layer, in_upper, inode) = node.first_layer_inode().await;
        if !in_upper {
            return Ok(());
        }

        // Copy node.childrens Hashmap to Vector, the Vector is also used as temp storage,
        // Without this, Rust won't allow us to remove them from node.childrens.
        let iter = node
            .childrens
            .lock()
            .await
            .iter()
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();

        for child in iter {
            // We only care about upper layer, ignore lower layers.
            if child.in_upper_layer().await {
                if child.whiteout.load(Ordering::Relaxed){
                    let child_name_os = OsStr::new(child.name.as_str());
                    layer.delete_whiteout(ctx, inode, child_name_os).await?
                } else {
                    let s = child.stat64(ctx).await?;
                    let cname: &OsStr = OsStr::new(&child.name);
                    if utils::is_dir(&s.attr.kind) {
                        let (count, whiteouts) = child.count_entries_and_whiteout(ctx).await?;
                        if count + whiteouts > 0 {
                            let cb = child.clone();
                            Box::pin(async move { self.empty_node_directory(ctx, cb).await })
                                .await?;
                        }
                        layer.rmdir(ctx, inode, cname).await?
                    } else {
                        layer.unlink(ctx, inode, cname).await?;
                    }
                }

                // delete the child
                self.remove_inode(child.inode, Some(child.path.clone()))
                    .await;
                node.remove_child(child.name.as_str()).await;
            }
        }

        Ok(())
    }

    async fn find_real_info_from_handle(
        &self,
        handle: Handle,
    ) -> Result<(Arc<BoxedLayer>, Inode, Handle)> {
        match self.handles.lock().await.get(&handle) {
            Some(h) => match h.real_handle {
                Some(ref rhd) => Ok((rhd.layer.clone(), rhd.inode, rhd.handle.load(Ordering::Relaxed))),
                None => Err(Error::from_raw_os_error(libc::ENOENT)),
            },

            None => Err(Error::from_raw_os_error(libc::ENOENT)),
        }
    }

    async fn find_real_inode(&self, inode: Inode) -> Result<(Arc<BoxedLayer>, Inode)> {
        if let Some(n) = self.get_active_inode(inode).await {
            let (first_layer, _, first_inode) = n.first_layer_inode().await;
            return Ok((first_layer, first_inode));
        }

        Err(Error::from_raw_os_error(libc::ENOENT))
    }

    async fn get_data(
        &self,
        ctx: Request,
        handle: Option<Handle>,
        inode: Inode,
        flags: u32,
    ) -> Result<Arc<HandleData>> {
        let no_open = self.no_open.load(Ordering::Relaxed);
        if !no_open {
            if let Some(h) = handle {
                if let Some(v) = self.handles.lock().await.get(&h) {
                    if v.node.inode == inode {
                        return Ok(Arc::clone(v));
                    }
                }
            }
        } else {
            let readonly: bool = flags
                & (libc::O_APPEND | libc::O_CREAT | libc::O_TRUNC | libc::O_RDWR | libc::O_WRONLY)
                    as u32
                == 0;

            // lookup node
            let node = self.lookup_node(ctx, inode, "").await?;

            // whiteout node
            if node.whiteout.load(Ordering::Relaxed) {
                return Err(Error::from_raw_os_error(libc::ENOENT));
            }

            if !readonly {
                // Check if upper layer exists, return EROFS is not exists.
                self.upper_layer
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| Error::from_raw_os_error(libc::EROFS))?;
                // copy up to upper layer
                self.copy_node_up(ctx, Arc::clone(&node)).await?;
            }

            let (layer, in_upper_layer, inode) = node.first_layer_inode().await;
            let handle_data = HandleData {
                node: Arc::clone(&node),
                real_handle: Some(RealHandle {
                    layer,
                    in_upper_layer,
                    inode,
                    handle: AtomicU64::new(0),
                }),
            };
            return Ok(Arc::new(handle_data));
        }

        Err(Error::from_raw_os_error(libc::ENOENT))
    }

    // extend or init the inodes number to one overlay if the current number is done.
    pub async fn extend_inode_alloc(&self, key: u64) {
        let next_inode = key * INODE_ALLOC_BATCH;
        let limit_inode = next_inode + INODE_ALLOC_BATCH - 1;
        self.inodes
            .write()
            .await
            .extend_inode_number(next_inode, limit_inode);
    }
}

/// Mounts the filesystem using the given parameters and returns the mount handle.
///
/// # Parameters
/// - `mountpoint`: Path to the mount point.
/// - `upperdir`: Path to the upper directory.
/// - `lowerdir`: Paths to the lower directories.
/// - `not_unprivileged`: If true, use privileged mount; otherwise, unprivileged mount.
///
/// # Returns
/// A mount handle on success.
pub async fn mount_fs(
    mountpoint: String,
    upperdir: String,
    lowerdir: Vec<String>,
    not_unprivileged: bool,
) -> fuse3::raw::MountHandle {
    // Create lower layers
    let mut lower_layers = Vec::new();
    for lower in &lowerdir {
        let layer = new_passthroughfs_layer(lower)
            .await
            .expect("Failed to create lower filesystem layer");
        lower_layers.push(Arc::new(layer));
    }
    // Create upper layer
    let upper_layer = Arc::new(
        new_passthroughfs_layer(&upperdir)
            .await
            .expect("Failed to create upper filesystem layer"),
    );

    // Configure overlay filesystem
    let config = Config {
        mountpoint: mountpoint.clone(),
        do_import: true,
        ..Default::default()
    };
    let overlayfs = OverlayFs::new(Some(upper_layer), lower_layers, config, 1)
        .expect("Failed to initialize OverlayFs");
    let logfs = LoggingFileSystem::new(overlayfs);

    let mount_path: OsString = OsString::from(mountpoint);

    // Obtain the current user's uid and gid
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut mount_options = MountOptions::default();
    mount_options.force_readdir_plus(true).uid(uid).gid(gid);

    // Mount filesystem based on privilege flag and return the mount handle
    if !not_unprivileged {
        Session::new(mount_options)
            .mount_with_unprivileged(logfs, mount_path)
            .await
            .expect("Unprivileged mount failed")
    } else {
        Session::new(mount_options)
            .mount(logfs, mount_path)
            .await
            .expect("Privileged mount failed")
    }
}
