use std::{fmt::Display, path::PathBuf};

use fuse3::{FileType, Timestamp, raw::reply::FileAttr};
use libc::stat64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct GPath {
    pub path: Vec<String>,
}

impl GPath {
    pub fn new() -> GPath {
        GPath { path: Vec::new() }
    }
    pub fn push(&mut self, path: String) {
        self.path.push(path);
    }
    pub fn pop(&mut self) -> Option<String> {
        self.path.pop()
    }
    pub fn name(&self) -> String {
        self.path.last().unwrap().clone()
    }
    pub fn part(&self, i: usize, j: usize) -> String {
        self.path[i..j].join("/")
    }
}

impl From<String> for GPath {
    fn from(mut s: String) -> GPath {
        if s.starts_with('/') {
            s.remove(0);
        }
        GPath {
            path: s.split('/').map(String::from).collect(),
        }
    }
}

impl From<GPath> for PathBuf {
    fn from(val: GPath) -> Self {
        let path_str = val.path.join("/");
        PathBuf::from(path_str)
    }
}
impl Display for GPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.path.join("/"))
    }
}

pub fn convert_stat64_to_file_attr(stat: stat64) -> FileAttr {
    FileAttr {
        ino: stat.st_ino,
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        atime: Timestamp::new(stat.st_atime, stat.st_atime_nsec.try_into().unwrap()),
        mtime: Timestamp::new(stat.st_mtime, stat.st_mtime_nsec.try_into().unwrap()),
        ctime: Timestamp::new(stat.st_ctime, stat.st_ctime_nsec.try_into().unwrap()),
        #[cfg(target_os = "macos")]
        crtime: Timestamp::new(0, 0), // Set crtime to 0 for non-macOS platforms
        kind: filetype_from_mode(stat.st_mode),
        perm: stat.st_mode as u16 & 0o7777,
        nlink: stat.st_nlink as u32,
        uid: stat.st_uid,
        gid: stat.st_gid,
        rdev: stat.st_rdev as u32,
        #[cfg(target_os = "macos")]
        flags: 0, // Set flags to 0 for non-macOS platforms
        blksize: stat.st_blksize as u32,
    }
}

pub fn filetype_from_mode(st_mode: u32) -> FileType {
    let st_mode = st_mode & 0xfff000;
    match st_mode {
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFSOCK => FileType::Socket,
        _ => {
            error!("wrong st mode : {}", st_mode);
            unreachable!();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::GPath;

    #[test]
    fn test_from_string() {
        let path = String::from("/release");
        let gapth = GPath::from(path);
        assert_eq!(gapth.to_string(), String::from("release"))
    }
}
