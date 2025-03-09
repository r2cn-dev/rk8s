// PathManager maps paths based on "object names" and their ids.
// The "object names" mapped by are internal to the storage system.
//
// The path layout in the storage backend is roughly as follows:
//
//	<root>/v2
//	├── blobs
//	│   └── <algorithm>
//	│       └── <split directory content addressable storage>
//  ├── uploads
//  │   └── <uuid>
//  │       └── data
//	└── repositories
//	    └── <name>
//	        └── _manifests
//	            └── tags
//	                └── <tag>
//	                    └── link
//
// The storage backend layout is broken up into a content-addressable blob
// store and repositories. The content-addressable blob store holds most data
// throughout the backend, keyed by algorithm and digests of the underlying
// content. Access to the blob store is controlled through links from the
// repository to blobstore.


use oci_spec::image::Digest;
use std::env;

#[derive(Clone)]
pub struct PathManager {
    root_path: String,
}

impl PathManager {
    pub fn new() -> Self {
        PathManager {
            root_path: env::var("OCI_REGISTRY_ROOTDIR")
                .unwrap_or_else(|_| "/var/lib/registry".to_string()),
        }
    }

    /// Returns the path to the root of the repositories,
    /// (e.g. `<root>/v2/repositories`).
    pub fn repository_path(self) -> String {
        format!("{}/v2/repositories", self.root_path)
    }

    /// Returns the path to the root of the manifests,
    /// (e.g. `<root>/v2/repositories/<name>/_manifests`).
    pub fn manifest_path(self, name: &str) -> String {
        format!("{}/{}/_manifests", self.repository_path(), name)
    }

    /// Returns the path to the root of the manifest tags,
    /// (e.g. `<root>/v2/repositories/<name>/_manifests/tags/`).
    pub fn manifest_tags_path(self, name: &str) -> String {
        format!("{}/tags", self.manifest_path(name))
    }

    /// Returns the path to a single manifest tag,
    /// (e.g. `<root>/v2/repositories/<name>/_manifests/tags/<tag>/`).
    pub fn manifest_tag_path(self, name: &str, tag: &str) -> String {
        format!("{}/{}", self.manifest_tags_path(name), tag)
    }

    /// Returns the path to the link of a single manifest tag,
    /// (e.g. `<root>/v2/repositories/<name>/_manifests/tags/<tag>/link`).
    pub fn manifest_tag_link_path(self, name: &str, tag: &str) -> String {
        format!("{}/link", self.manifest_tag_path(name, tag))
    }

    /// Returns the path to the root of uploads,
    /// (e.g. `<root>/v2/uploads`).
    pub fn uploads_path(self) -> String {
        format!("{}/v2/uploads", self.root_path)
    }

    /// Returns the path to a single upload session,
    /// (e.g. `<root>/v2/uploads/<id>`).
    pub fn upload_path(self, id: &str) -> String {
        format!("{}/{}", self.uploads_path(), id)
    }

    /// Returns the path to the data of a single upload session,
    /// (e.g. `<root>/v2/uploads/<id>/data`).
    pub fn upload_data_path(self, id: &str) -> String {
        format!("{}/data", self.upload_path(id))
    }

    /// Returns the path to the root of the blob binarys,
    /// (e.g. `<root>/v2/blobs/`).
    pub fn blobs_path(self) -> String {
        format!("{}/v2/blobs", self.root_path)
    }

    /// Returns the path to a single blob binary,
    /// (e.g. `<root>/v2/blobs/<algorithm>/<first two hex bytes of digest>/<hex digest>`).
    pub fn blob_path(self, digest: &Digest) -> String {
        format!(
            "{}/{}/{}/{}",
            self.blobs_path(),
            digest.algorithm(),
            digest.digest()[..2].to_string(),
            digest.digest()
        )
    }

    /// Returns the path to the data of a single blob binary
    /// (e.g. `<root>/v2/blobs/<algorithm>/<first two hex bytes of digest>/<hex digest>/data`).
    pub fn blob_data_path(self, digest: &Digest) -> String {
        format!("{}/data", self.blob_path(digest))
    }
}
