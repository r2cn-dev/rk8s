//! 高层对象客户端，封装后端 put/get。

use async_trait::async_trait;

#[async_trait]
pub trait ObjectBackend: Send + Sync {
    async fn put_object(
        &self,
        key: &str,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn get_object(
        &self,
        key: &str,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>;
    async fn get_etag(&self, key: &str)
    -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

pub struct ObjectClient<B: ObjectBackend> {
    backend: B,
}

impl<B: ObjectBackend> ObjectClient<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub async fn put_object(
        &self,
        key: &str,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.backend.put_object(key, data).await
    }

    pub async fn get_object(
        &self,
        key: &str,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        self.backend.get_object(key).await
    }

    pub async fn get_etag(
        &self,
        key: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.backend.get_etag(key).await
    }
}
