use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

use crate::storage::{Storage, driver::filesystem::FilesystemStorage};

#[derive(Clone, Debug)]
pub struct UploadSession {
    pub length: u64,
    pub uploaded: u64, // the last uploaded byte index
}

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<RwLock<HashMap<String, UploadSession>>>,
    pub storage: Arc<dyn Storage>,
    pub registry: Arc<String>, // Registry URL
}

impl AppState {
    pub fn new(storage_type: &str, root: &str, registry: &str) -> Self {
        let storage_backend: Arc<dyn Storage + Send + Sync> = match storage_type {
            "FILESYSTEM" => Arc::new(FilesystemStorage::new(root)),
            _ => Arc::new(FilesystemStorage::new(root)),
        };
        AppState {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            storage: storage_backend,
            registry: Arc::new(registry.to_string()),
        }
    }

    pub async fn get_session(&self, id: &str) -> Option<UploadSession> {
        let sessions = self.sessions.read().await;
        sessions.get(id).cloned()
    }

    pub async fn create_session(&self) -> Result<String, String> {
        let mut sessions = self.sessions.write().await;
        let session_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            session_id.clone(),
            UploadSession {
                length: 0,
                uploaded: 0,
            },
        );
        Ok(session_id)
    }

    pub async fn close_session(&self, id: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(id);
    }

    pub async fn update_session(&self, id: &str, length: u64) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(id) {
            session.length += length;
            if session.uploaded == 0 {
                session.uploaded += length - 1;
            } else {
                session.uploaded += length;
            }
        }
    }
}
