use std::{fs, path::PathBuf};

use super::super::paths::PathManager;
use super::super::Storage;

use axum::body::BodyDataStream;
use futures::TryStreamExt;
use oci_spec::image::Digest;
use tokio::{
    fs::{File, OpenOptions},
    io::{self, BufWriter},
};
use tokio_util::io::StreamReader;


pub struct FilesystemStorage {
    path_manager: PathManager,
}

impl FilesystemStorage {
    pub fn new() -> Self {
        FilesystemStorage {
            path_manager: PathManager::new(),
        }
    }
}

#[async_trait::async_trait]
impl Storage for FilesystemStorage {
    async fn read_by_tag(&self, name: &str, tag: &str) -> io::Result<File> {
        File::open(self.path_manager.clone().manifest_tag_link_path(name, tag)).await
    }

    async fn read_by_digest(&self, digest: &Digest) -> io::Result<File> {
        File::open(self.path_manager.clone().blob_data_path(digest)).await
    }

    async fn read_by_uuid(&self, uuid: &str) -> io::Result<File> {
        File::open(self.path_manager.clone().upload_data_path(uuid)).await
    }

    async fn write_by_digest(
        &self,
        digest: &Digest,
        stream: BodyDataStream,
        append: bool,
    ) -> io::Result<()> {
        async {
            // Convert the stream into an `AsyncRead`.
            let body_with_io_error =
                stream.map_err(|err| io::Error::new(io::ErrorKind::Other, err));
            let body_reader = StreamReader::new(body_with_io_error);
            futures::pin_mut!(body_reader);

            // Create the file. `File` implements `AsyncWrite`.
            let file_path = self
                .crate_path(&self.path_manager.clone().blob_data_path(digest))
                .await?;
            let mut file_writer = if append {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(file_path)
                    .await?;
                BufWriter::new(file)
            } else {
                let file = File::create(file_path).await?;
                BufWriter::new(file)
            };

            // Copy the body into the file.
            tokio::io::copy(&mut body_reader, &mut file_writer).await?;

            Ok::<_, io::Error>(())
        }
        .await
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
    }

    async fn write_by_uuid(
        &self,
        uuid: &str,
        stream: BodyDataStream,
        append: bool,
    ) -> io::Result<()> {
        async {
            // Convert the stream into an `AsyncRead`.
            let body_with_io_error =
                stream.map_err(|err| io::Error::new(io::ErrorKind::Other, err));
            let body_reader = StreamReader::new(body_with_io_error);
            futures::pin_mut!(body_reader);

            // Create the file. `File` implements `AsyncWrite`.
            let file_path = self
                .crate_path(&self.path_manager.clone().upload_data_path(&uuid))
                .await?;
            let mut file_writer = if append {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(file_path)
                    .await?;
                BufWriter::new(file)
            } else {
                let file = File::create(file_path).await?;
                BufWriter::new(file)
            };

            // Copy the body into the file.
            tokio::io::copy(&mut body_reader, &mut file_writer).await?;

            Ok::<_, io::Error>(())
        }
        .await
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
    }

    async fn move_to_digest(&self, session_id: &str, digest: &Digest) -> io::Result<()> {
        let upload_data_path = self
            .crate_path(&self.path_manager.clone().upload_data_path(&session_id))
            .await?;
        let blob_data_path = self
            .crate_path(&self.path_manager.clone().blob_data_path(digest))
            .await?;
        fs::rename(upload_data_path, blob_data_path)?;
        Ok(())
    }

    async fn crate_path(&self, path: &String) -> io::Result<PathBuf> {
        let file_path = std::path::Path::new(&path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(file_path.to_path_buf())
    }

    async fn link_to_tag(&self, name: &str, tag: &str, digest: &Digest) -> io::Result<()> {
        let tag_path = self
            .crate_path(&self.path_manager.clone().manifest_tag_link_path(name, tag))
            .await?;
        let digest_path = self
            .crate_path(&self.path_manager.clone().blob_data_path(digest))
            .await?;

        // Remove the existing symlink if it exists
        if let Ok(metadata) = fs::symlink_metadata(&tag_path) {
            if metadata.file_type().is_symlink() {
                fs::remove_file(&tag_path)?;
            }
        }

        #[cfg(not(unix))]
        std::os::windows::fs::symlink_file(digest_path, tag_path)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(digest_path, tag_path)?;

        Ok(())
    }

    async fn walk_repo_dir(&self, name: &str) -> io::Result<Vec<String>> {
        let mut entries = vec![];
        let path = self.path_manager.clone().manifest_tags_path(name);
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            entries.push(path.file_name().unwrap().to_str().unwrap().to_string());
        }
        entries.sort();
        Ok(entries)
    }

    async fn delete_by_tag(&self, name: &str, tag: &str) -> io::Result<()> {
        let tag_path = self.path_manager.clone().manifest_tag_path(name, tag);
        fs::remove_dir_all(tag_path)?;
        Ok(())
    }

    async fn delete_by_digest(&self, digest: &Digest) -> io::Result<()> {
        let blob_path = self.path_manager.clone().blob_path(digest);
        fs::remove_dir_all(blob_path)?;
        Ok(())
    }
}
