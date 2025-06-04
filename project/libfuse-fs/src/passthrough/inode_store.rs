// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD-3-Clause file.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::file_handle::FileHandle;
use super::statx::StatExt;
use super::{Inode, InodeData, InodeHandle};

#[derive(Clone, Copy, Default, PartialOrd, Ord, PartialEq, Eq, Debug)]
/// Identify an inode in `PassthroughFs` by `InodeId`.
pub struct InodeId {
    pub ino: libc::ino64_t,
    pub dev: libc::dev_t,
    pub mnt: u64,
}

impl InodeId {
    #[inline]
    pub(super) fn from_stat(st: &StatExt) -> Self {
        InodeId {
            ino: st.st.st_ino,
            dev: st.st.st_dev,
            mnt: st.mnt_id,
        }
    }
}

/// A store for inodes, which allows to map inodes to their data and vice versa.
///
/// It maintains a mapping of inodes to their data, as well as a mapping of inode IDs and file handles to inodes.
/// The store is designed to ensure that the same inodes are used for the same files, even if they are opened multiple times.
/// The store is not thread-safe, so it should be used in a single-threaded context or protected by a mutex.
/// The store uses `BTreeMap` to maintain the mappings, which allows for efficient lookups and insertions.
#[derive(Default)]
pub struct InodeStore {
    data: BTreeMap<Inode, Arc<InodeData>>, // Maps inodes to their data.
    by_id: BTreeMap<InodeId, Inode>,       // Maps real inode IDs to inodes.
    by_handle: BTreeMap<Arc<FileHandle>, Inode>, // Maps file handles to inodes.
}

impl InodeStore {
    /// Insert an inode into the manager
    ///
    /// The caller needs to ensure that no inode with the same key exists, otherwise the old inode
    /// will get lost.
    pub fn insert(&mut self, data: Arc<InodeData>) {
        self.by_id.insert(data.id, data.inode);
        if let InodeHandle::Handle(handle) = &data.handle {
            self.by_handle
                .insert(handle.file_handle().clone(), data.inode);
        }
        self.data.insert(data.inode, data);
    }

    /// Remove an inode from the manager, keeping the (key, ino) mapping if `remove_data_only` is true.
    #[allow(unused)]
    pub fn remove(&mut self, inode: &Inode, remove_data_only: bool) -> Option<Arc<InodeData>> {
        let data = self.data.remove(inode);
        if remove_data_only {
            // Don't remove by_id and by_handle, we need use it to store inode
            // record the mapping of inodes using these two structures to ensure
            // that the same files always use the same inode
            return data;
        }

        if let Some(data) = data.as_ref() {
            if let InodeHandle::Handle(handle) = &data.handle {
                self.by_handle.remove(handle.file_handle());
            }
            self.by_id.remove(&data.id);
        }
        data
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.by_handle.clear();
        self.by_id.clear();
    }

    pub fn get(&self, inode: &Inode) -> Option<&Arc<InodeData>> {
        self.data.get(inode)
    }

    pub fn get_by_id(&self, id: &InodeId) -> Option<&Arc<InodeData>> {
        let inode = self.inode_by_id(id)?;
        self.get(inode)
    }

    pub fn get_by_handle(&self, handle: &FileHandle) -> Option<&Arc<InodeData>> {
        let inode = self.inode_by_handle(handle)?;
        self.get(inode)
    }

    pub fn inode_by_id(&self, id: &InodeId) -> Option<&Inode> {
        self.by_id.get(id)
    }

    pub fn inode_by_handle(&self, handle: &FileHandle) -> Option<&Inode> {
        self.by_handle.get(handle)
    }
}

#[cfg(test)]
mod test {
    use super::super::*;
    use super::*;

    use std::mem::MaybeUninit;
    use std::os::unix::io::AsRawFd;
    use vmm_sys_util::tempfile::TempFile;

    // impl PartialEq for InodeData {
    //     asyncfn eq(&self, other: &Self) -> bool {
    //         let self_count = *self.refcount.lock().await;
    //         if self.inode != other.inode
    //             || self.id != other.id
    //             || self.mode != other.mode
    //             || self.refcount.load(Ordering::Relaxed) != other.refcount.load(Ordering::Relaxed)
    //         {
    //             return false;
    //         }

    //         match (&self.handle, &other.handle) {
    //             (InodeHandle::File(f1), InodeHandle::File(f2)) => f1.as_raw_fd() == f2.as_raw_fd(),
    //             (InodeHandle::Handle(h1), InodeHandle::Handle(h2)) => {
    //                 h1.file_handle() == h2.file_handle()
    //             }
    //             _ => false,
    //         }
    //     }
    // }

    fn stat_fd(fd: &impl AsRawFd) -> std::io::Result<libc::stat64> {
        let mut st = MaybeUninit::<libc::stat64>::zeroed();
        let null_path = c"";

        // Safe because the kernel will only write data in `st` and we check the return value.
        let res = unsafe {
            libc::fstatat64(
                fd.as_raw_fd(),
                null_path.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if res >= 0 {
            // Safe because the kernel guarantees that the struct is now fully initialized.
            Ok(unsafe { st.assume_init() })
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[test]
    fn test_inode_store() {
        let mut m = InodeStore::default();
        let tmpfile1 = TempFile::new().unwrap();
        let tmpfile2 = TempFile::new().unwrap();

        let inode1: Inode = 3;
        let inode2: Inode = 4;
        let inode_stat1 = StatExt {
            st: stat_fd(tmpfile1.as_file()).unwrap(),
            mnt_id: 0,
        };
        let inode_stat2 = StatExt {
            st: stat_fd(tmpfile2.as_file()).unwrap(),
            mnt_id: 0,
        };
        let id1 = InodeId::from_stat(&inode_stat1);
        let id2 = InodeId::from_stat(&inode_stat2);
        let file_or_handle1 = InodeHandle::File(tmpfile1.into_file());
        let file_or_handle2 = InodeHandle::File(tmpfile2.into_file());
        let data1 = InodeData::new(inode1, file_or_handle1, 2, id1, inode_stat1.st.st_mode);
        let data2 = InodeData::new(inode2, file_or_handle2, 2, id2, inode_stat2.st.st_mode);
        let data1 = Arc::new(data1);
        let data2 = Arc::new(data2);

        m.insert(data1.clone());

        // get not present key, expect none
        assert!(m.get(&1001).is_none());

        // get just inserted value by key, by id, by handle
        assert!(m.get_by_id(&InodeId::default()).is_none());
        assert!(m.get_by_handle(&FileHandle::default()).is_none());

        // insert another value, and check again
        m.insert(data2.clone());
        assert!(m.get(&1).is_none());
        assert!(m.get_by_id(&InodeId::default()).is_none());
        assert!(m.get_by_handle(&FileHandle::default()).is_none());

        // remove non-present key
        assert!(m.remove(&1, false).is_none());

        // clear the map
        m.clear();
        assert!(m.get(&1).is_none());
        assert!(m.get_by_id(&InodeId::default()).is_none());
        assert!(m.get_by_handle(&FileHandle::default()).is_none());
        assert!(m.get(&inode1).is_none());
        assert!(m.get_by_id(&id1).is_none());
        assert!(m.get(&inode2).is_none());
        assert!(m.get_by_id(&id2).is_none());
    }
}
