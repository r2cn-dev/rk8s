#[derive(Debug, Clone)]
pub struct LayerCompressionResult {
    pub tar_sha256sum: String,
    pub tar_size: u64,
    pub gz_sha256sum: String,
    pub gz_size: u64,
}

impl LayerCompressionResult {
    pub fn new(tar_sha256sum: String, tar_size: u64, gz_sha256sum: String, gz_size: u64) -> Self {
        Self {
            tar_sha256sum,
            tar_size,
            gz_sha256sum,
            gz_size,
        }
    }
}
