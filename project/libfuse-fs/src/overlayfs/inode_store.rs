// Copyright (C) 2023 Ant Group. All rights reserved.
// 2024 From [fuse_backend_rs](https://github.com/cloud-hypervisor/fuse-backend-rs)
// SPDX-License-Identifier: Apache-2.0

use std::io::{Error, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::HashMap, sync::Arc};

use crate::passthrough::VFS_MAX_INO;

use super::{Inode, OverlayInode};

use futures::future::join_all;
use radix_trie::Trie;

pub struct InodeStore {
    // Active inodes.
    inodes: HashMap<Inode, Arc<OverlayInode>>,
    // Deleted inodes which were unlinked but have non zero lookup count.
    deleted: HashMap<Inode, Arc<OverlayInode>>,
    // Path to inode mapping, used to reserve inode number for same path.
    path_mapping: Trie<String, Inode>,
    next_inode: u64,
    inode_limit: u64,
    // FUSE inode to nlink mapping
    nlinks: HashMap<Inode, Arc<AtomicU64>>,
}

impl InodeStore {
    pub(crate) fn new() -> Self {
        Self {
            inodes: HashMap::new(),
            deleted: HashMap::new(),
            path_mapping: Trie::new(),
            next_inode: 1,
            inode_limit: VFS_MAX_INO,
            nlinks: HashMap::new(),
        }
    }

    pub(crate) fn alloc_unique_inode(&mut self) -> Result<Inode> {
        // Iter VFS_MAX_INO times to find a free inode number.
        let mut ino = self.next_inode;
        for _ in 0..self.inode_limit {
            if ino > self.inode_limit {
                ino = 1;
            }
            if !self.inodes.contains_key(&ino) && !self.deleted.contains_key(&ino) {
                self.next_inode = ino + 1;
                return Ok(ino);
            }
            ino += 1;
        }
        error!("reached maximum inode number: {}", self.inode_limit);
        Err(Error::other(format!(
            "maximum inode number {} reached",
            self.inode_limit
        )))
    }

    pub(crate) fn alloc_inode(&mut self, path: &str) -> Result<Inode> {
        match self.path_mapping.get(path) {
            // If the path is already in the mapping, return the reserved inode number.
            Some(v) => Ok(*v),
            // Or allocate a new inode number.
            None => self.alloc_unique_inode(),
        }
    }

    pub(crate) async fn insert_inode(&mut self, inode: Inode, node: Arc<OverlayInode>) {
        self.path_mapping
            .insert(node.path.read().await.clone(), inode);
        self.nlinks
            .entry(inode)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(1, Ordering::Relaxed);
        self.inodes.entry(inode).or_insert(node);
    }

    pub(crate) fn get_inode(&self, inode: Inode) -> Option<Arc<OverlayInode>> {
        self.inodes.get(&inode).cloned()
    }

    pub(crate) fn get_deleted_inode(&self, inode: Inode) -> Option<Arc<OverlayInode>> {
        self.deleted.get(&inode).cloned()
    }

    // Return the inode only if it's permanently deleted from both self.inodes and self.deleted_inodes.
    pub(crate) async fn remove_inode(
        &mut self,
        inode: Inode,
        path_removed: Option<String>,
    ) -> Option<Arc<OverlayInode>> {
        let old_nlink = self.nlinks.get(&inode)?.fetch_sub(1, Ordering::Relaxed);

        if let Some(path) = path_removed {
            self.path_mapping.remove(&path);
        }

        if old_nlink == 1
            && let Some(inode_data) = self.inodes.remove(&inode)
        {
            if inode_data.lookups.load(Ordering::Relaxed) > 0 {
                trace!(
                    "InodeStore: inode {inode} unlinked but still in use, moving to deleted map."
                );
                self.deleted.insert(inode, inode_data);
                return None;
            } else {
                trace!("InodeStore: inode {inode} permanently removed (nlink=0, lookups=0).");
                self.nlinks.remove(&inode);
                return Some(inode_data);
            }
        }

        None
    }

    // As a debug function, print all inode numbers in hash table.
    // This function consumes quite lots of memory, so it's disabled by default.
    #[allow(dead_code)]
    pub(crate) async fn debug_print_all_inodes(&self) {
        // Convert the HashMap to Vector<(inode, pathname)>
        let all_inodes_f = self
            .inodes
            .iter()
            .map(|(inode, ovi)| {
                async move {
                    let path = ovi.path.read().await.clone();
                    (inode, path, ovi.lookups.load(Ordering::Relaxed)) // Read the Inode State.
                }
            })
            .collect::<Vec<_>>();
        let mut all_inodes = join_all(all_inodes_f).await;
        all_inodes.sort_by(|a, b| a.0.cmp(b.0));
        trace!("all active inodes: {all_inodes:?}");

        let to_delete = self
            .deleted
            .iter()
            .map(|(inode, ovi)| async move {
                (
                    inode,
                    ovi.path.read().await.clone(),
                    ovi.lookups.load(Ordering::Relaxed),
                )
            })
            .collect::<Vec<_>>();
        let mut delete_to = join_all(to_delete).await;
        delete_to.sort_by(|a, b| a.0.cmp(b.0));
        trace!("all deleted inodes: {delete_to:?}");
    }

    pub fn extend_inode_number(&mut self, next_inode: u64, limit_inode: u64) {
        self.next_inode = next_inode;
        self.inode_limit = limit_inode;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_alloc_unique() {
        let mut store = InodeStore::new();
        let empty_node = Arc::new(OverlayInode::new());
        store.insert_inode(1, empty_node.clone()).await;
        store.insert_inode(2, empty_node.clone()).await;
        store
            .insert_inode(VFS_MAX_INO - 1, empty_node.clone())
            .await;

        let inode = store.alloc_unique_inode().unwrap();
        assert_eq!(inode, 3);
        assert_eq!(store.next_inode, 4);

        store.next_inode = VFS_MAX_INO - 1;
        let inode = store.alloc_unique_inode().unwrap();
        assert_eq!(inode, VFS_MAX_INO);

        let inode = store.alloc_unique_inode().unwrap();
        assert_eq!(inode, 3);
    }

    #[tokio::test]
    async fn test_alloc_existing_path() {
        let mut store = InodeStore::new();
        let mut node_a = OverlayInode::new();
        node_a.path = tokio::sync::RwLock::new("/a".to_string());
        store.insert_inode(1, Arc::new(node_a)).await;
        let mut node_b = OverlayInode::new();
        node_b.path = tokio::sync::RwLock::new("/b".to_string());
        store.insert_inode(2, Arc::new(node_b)).await;
        let mut node_c = OverlayInode::new();
        node_c.path = tokio::sync::RwLock::new("/c".to_string());
        store.insert_inode(VFS_MAX_INO - 1, Arc::new(node_c)).await;

        let inode = store.alloc_inode("/a").unwrap();
        assert_eq!(inode, 1);

        let inode = store.alloc_inode("/b").unwrap();
        assert_eq!(inode, 2);

        let inode = store.alloc_inode("/c").unwrap();
        assert_eq!(inode, VFS_MAX_INO - 1);

        let inode = store.alloc_inode("/notexist").unwrap();
        assert_eq!(inode, 3);
    }
}
