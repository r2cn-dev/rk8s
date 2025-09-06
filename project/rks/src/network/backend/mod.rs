#![allow(dead_code)]
use crate::network::config::Config;
use anyhow::Result;
use async_trait::async_trait;
use common::{ExternalInterface, lease::Lease};
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod hostgw;
pub mod interface;
pub mod route;

#[cfg(test)]
pub mod tests;

/// Backend trait for different networking implementations
#[async_trait]
pub trait Backend: Send + Sync {
    /// Register a network and return a Network instance
    async fn register_network(&self, config: &Config) -> Result<Arc<Mutex<dyn Network>>>;

    /// Get backend type name
    fn backend_type(&self) -> &str;
}

/// Network trait for managing network operations
#[async_trait]
pub trait Network: Send + Sync {
    /// Get the lease associated with this network
    async fn get_lease(&self) -> Result<Lease>;

    /// Set the lease for this network
    async fn set_lease(&mut self, lease: Lease) -> Result<()>;

    /// Start the network operations
    async fn run(&mut self) -> Result<()>;

    /// Stop the network operations
    async fn stop(&mut self) -> Result<()>;

    /// Get network MTU
    fn mtu(&self) -> Option<u32>;

    /// Get backend type
    fn backend_type(&self) -> &str;
}

/// Simple network implementation providing basic functionality
#[derive(Debug)]
pub struct SimpleNetwork {
    pub ext_iface: ExternalInterface,
    pub lease: Option<Lease>,
}

#[async_trait]
impl Network for SimpleNetwork {
    async fn get_lease(&self) -> Result<Lease> {
        self.lease
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No lease available"))
    }

    async fn set_lease(&mut self, lease: Lease) -> Result<()> {
        self.lease = Some(lease);
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        log::info!(
            "Starting simple network on interface {}",
            self.ext_iface.iface.name
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        log::info!(
            "Stopping simple network on interface {}",
            self.ext_iface.iface.name
        );
        Ok(())
    }

    fn mtu(&self) -> Option<u32> {
        self.ext_iface.iface.mtu
    }

    fn backend_type(&self) -> &str {
        "simple"
    }
}
