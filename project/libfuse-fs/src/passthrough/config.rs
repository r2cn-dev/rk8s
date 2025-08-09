// Copyright (C) 2020-2022 Alibaba Cloud. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::time::Duration;

/// The caching policy that the file system should report to the FUSE client. By default the FUSE
/// protocol uses close-to-open consistency. This means that any cached contents of the file are
/// invalidated the next time that file is opened.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub enum CachePolicy {
    /// The client should never cache file data and all I/O should be directly forwarded to the
    /// server. This policy must be selected when file contents may change without the knowledge of
    /// the FUSE client (i.e., the file system does not have exclusive access to the directory).
    Never,

    /// This is almost same as Never, but it allows page cache of directories, dentries and attr
    /// cache in guest. In other words, it acts like cache=never for normal files, and like
    /// cache=always for directories, besides, metadata like dentries and attrs are kept as well.
    /// This policy can be used if:
    /// 1. the client wants to use Never policy but it's performance in I/O is not good enough
    /// 2. the file system has exclusive access to the directory
    /// 3. cache directory content and other fs metadata can make a difference on performance.
    Metadata,

    /// The client is free to choose when and how to cache file data. This is the default policy and
    /// uses close-to-open consistency as described in the enum documentation.
    #[default]
    Auto,

    /// The client should always cache file data. This means that the FUSE client will not
    /// invalidate any cached data that was returned by the file system the last time the file was
    /// opened. This policy should only be selected when the file system has exclusive access to the
    /// directory.
    Always,
}

impl FromStr for CachePolicy {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" | "Never" | "NEVER" | "none" | "None" | "NONE" => Ok(CachePolicy::Never),
            "metadata" => Ok(CachePolicy::Metadata),
            "auto" | "Auto" | "AUTO" => Ok(CachePolicy::Auto),
            "always" | "Always" | "ALWAYS" => Ok(CachePolicy::Always),
            _ => Err("invalid cache policy"),
        }
    }
}

/// Options that configure the behavior of the passthrough fuse file system.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Config {
    /// How long the FUSE client should consider file and directory attributes to be valid. If the
    /// attributes of a file or directory can only be modified by the FUSE client (i.e., the file
    /// system has exclusive access), then this should be set to a large value.
    ///
    /// The default value for this option is 5 seconds.
    pub attr_timeout: Duration,

    /// How long the FUSE client should consider directory entries to be valid. If the contents of a
    /// directory can only be modified by the FUSE client (i.e., the file system has exclusive
    /// access), then this should be a large value.
    ///
    /// The default value for this option is 5 seconds.
    pub entry_timeout: Duration,

    /// Same as `attr_timeout`, override `attr_timeout` config, but only take effect on directories
    /// when specified. This is useful to set different timeouts for directories and regular files.
    pub dir_attr_timeout: Option<Duration>,

    /// Same as `entry_timeout`, override `entry_timeout` config, but only take effect on
    /// directories when specified. This is useful to set different timeouts for directories and
    /// regular files.
    pub dir_entry_timeout: Option<Duration>,

    /// The caching policy the file system should use. See the documentation of `CachePolicy` for
    /// more details.
    pub cache_policy: CachePolicy,

    /// Whether the file system should enable writeback caching. This can improve performance as it
    /// allows the FUSE client to cache and coalesce multiple writes before sending them to the file
    /// system. However, enabling this option can increase the risk of data corruption if the file
    /// contents can change without the knowledge of the FUSE client (i.e., the server does **NOT**
    /// have exclusive access). Additionally, the file system should have read access to all files
    /// in the directory it is serving as the FUSE client may send read requests even for files
    /// opened with `O_WRONLY`.
    ///
    /// Therefore callers should only enable this option when they can guarantee that: 1) the file
    /// system has exclusive access to the directory and 2) the file system has read permissions for
    /// all files in that directory.
    ///
    /// The default value for this option is `false`.
    pub writeback: bool,

    /// The path of the root directory.
    ///
    /// The default is `/`.
    pub root_dir: String,

    /// Whether the file system should support Extended Attributes (xattr). Enabling this feature may
    /// have a significant impact on performance, especially on write parallelism. This is the result
    /// of FUSE attempting to remove the special file privileges after each write request.
    ///
    /// The default value for this options is `false`.
    pub xattr: bool,

    /// To be compatible with Vfs and PseudoFs, PassthroughFs needs to prepare
    /// root inode before accepting INIT request.
    ///
    /// The default value for this option is `true`.
    pub do_import: bool,

    /// Control whether no_open is allowed.
    ///
    /// The default value for this option is `false`.
    pub no_open: bool,

    /// Control whether no_opendir is allowed.
    ///
    /// The default value for this option is `false`.
    pub no_opendir: bool,

    /// Control whether kill_priv_v2 is enabled.
    ///
    /// The default value for this option is `false`.
    pub killpriv_v2: bool,

    /// Control whether readdir/readdirplus requests return zero dirent to client, as if the
    /// directory is empty even if it has children.
    pub no_readdir: bool,

    /// Control whether to refuse operations which modify the size of the file. For a share memory
    /// file mounted from host, seal_size can prohibit guest to increase the size of
    /// share memory file to attack the host.
    pub seal_size: bool,

    /// Whether count mount ID or not when comparing two inodes. By default we think two inodes
    /// are same if their inode number and st_dev are the same. When `enable_mntid` is set as
    /// 'true', inode's mount ID will be taken into account as well. For example, bindmount the
    /// same file into virtiofs' source dir, the two bindmounted files will be identified as two
    /// different inodes when this option is true, so the don't share pagecache.
    ///
    /// The default value for this option is `false`.
    pub enable_mntid: bool,

    /// What size file supports dax
    /// * If dax_file_size == None, DAX will disable to all files.
    /// * If dax_file_size == 0, DAX will enable all files.
    /// * If dax_file_size == N, DAX will enable only when the file size is greater than or equal
    ///   to N Bytes.
    pub dax_file_size: Option<u64>,

    /// Reduce memory consumption by directly use host inode when possible.
    ///
    /// When set to false, a virtual inode number will be allocated for each file managed by
    /// the passthroughfs driver. A map is used to maintain the relationship between virtual
    /// inode numbers and host file objects.
    /// When set to true, the host inode number will be directly used as virtual inode number
    /// if it's less than the threshold (1 << 47), so reduce memory consumed by the map.
    /// A virtual inode number will still be allocated and maintained if the host inode number
    /// is bigger than the threshold.
    /// The default value for this option is `false`.
    pub use_host_ino: bool,

    /// Whether the file system should honor the O_DIRECT flag. If this option is disabled,
    /// that flag will be filtered out at `open_inode`.
    ///
    /// The default is `true`.
    pub allow_direct_io: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            entry_timeout: Duration::from_secs(5),
            attr_timeout: Duration::from_secs(5),
            cache_policy: Default::default(),
            writeback: false,
            root_dir: String::from("/"),
            xattr: false,
            do_import: true,
            no_open: false,
            no_opendir: false,
            killpriv_v2: false,
            no_readdir: false,
            seal_size: false,
            enable_mntid: false,
            dax_file_size: None,
            dir_entry_timeout: None,
            dir_attr_timeout: None,
            use_host_ino: false,
            allow_direct_io: true,
        }
    }
}
