use axum::body::BodyDataStream;
use oci_spec::image::Digest;
use std::path::PathBuf;
use tokio::{fs::File, io};

pub mod driver;
pub mod paths;

#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    async fn read_by_tag(&self, name: &str, tag: &str) -> io::Result<File>;
    async fn read_by_digest(&self, digest: &Digest) -> io::Result<File>;
    async fn read_by_uuid(&self, uuid: &str) -> io::Result<File>;
    async fn write_by_digest(
        &self,
        digest: &Digest,
        stream: BodyDataStream,
        append: bool,
    ) -> io::Result<()>;
    async fn write_by_uuid(
        &self,
        uuid: &str,
        stream: BodyDataStream,
        append: bool,
    ) -> io::Result<()>;
    async fn move_to_digest(&self, session_id: &str, digest: &Digest) -> io::Result<()>;
    async fn crate_path(&self, path: &String) -> io::Result<PathBuf>;
    async fn link_to_tag(&self, name: &str, tag: &str, digest: &Digest) -> io::Result<()>;
    async fn walk_repo_dir(&self, name: &str) -> io::Result<Vec<String>>;
    async fn delete_by_tag(&self, name: &str, tag: &str) -> io::Result<()>;
    async fn delete_by_digest(&self, digest: &Digest) -> io::Result<()>;
}
