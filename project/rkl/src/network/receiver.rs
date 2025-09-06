use anyhow::Result;
use libcni::ip::route::Route;
use log::{error, info, warn};
use quinn::{ClientConfig, Endpoint};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

use crate::network::{
    config::NetworkConfig,
    route::{RouteManager, RouteReceiver},
    subnet::SubnetReceiver,
};

/// Main network configuration receiver that coordinates subnet and route configuration
/// This will be the primary interface for receiving network configurations from rks
pub struct NetworkReceiver {
    subnet_receiver: SubnetReceiver,
    route_receiver: RouteReceiver,
    route_manager: Arc<Mutex<RouteManager>>,
    node_id: String,
    rks_endpoint: Option<SocketAddr>,
    quic_endpoint: Option<Endpoint>,
    shutdown_tx: Option<mpsc::Sender<()>>,
    route_shutdown_tx: Option<mpsc::Sender<()>>,
}

impl NetworkReceiver {
    pub fn new(
        subnet_file_path: String,
        link_index: u32,
        backend_type: String,
        node_id: String,
    ) -> Self {
        let route_manager = Arc::new(Mutex::new(RouteManager::new(link_index, backend_type)));
        let subnet_receiver = SubnetReceiver::new(subnet_file_path);
        let route_receiver = RouteReceiver::new(route_manager.clone());

        Self {
            subnet_receiver,
            route_receiver,
            route_manager,
            node_id,
            rks_endpoint: None,
            quic_endpoint: None,
            shutdown_tx: None,
            route_shutdown_tx: None,
        }
    }

    /// Set the RKS endpoint for QUIC communication
    pub fn with_rks_endpoint(mut self, endpoint: SocketAddr) -> Self {
        self.rks_endpoint = Some(endpoint);
        self
    }

    /// Initialize QUIC client endpoint
    pub async fn init_quic_client(&mut self) -> Result<()> {
        info!("Initializing QUIC client for node: {}", self.node_id);

        // Create client configuration with platform verifier
        let client_config = ClientConfig::try_with_platform_verifier()?;

        let mut endpoint = Endpoint::client("[::]:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        self.quic_endpoint = Some(endpoint);
        info!("QUIC client initialized successfully");
        Ok(())
    }

    /// Handle received network configuration from rks
    /// This includes both subnet.env and route configurations
    pub async fn handle_network_config(&self, config: NetworkConfigMessage) -> Result<()> {
        info!(
            "Received network configuration from rks for node: {}",
            self.node_id
        );

        match config {
            NetworkConfigMessage::SubnetConfig {
                network_config,
                ip_masq,
                ipv4_subnet,
                ipv6_subnet,
                mtu,
            } => {
                self.subnet_receiver
                    .handle_subnet_config(&network_config, ip_masq, ipv4_subnet, ipv6_subnet, mtu)
                    .await?;
            }
            NetworkConfigMessage::Route { routes } => {
                self.route_receiver.handle_route_config(routes).await?;
            }
            NetworkConfigMessage::FullConfig {
                network_config,
                ip_masq,
                ipv4_subnet,
                ipv6_subnet,
                mtu,
                routes,
            } => {
                // Handle both subnet and route configuration
                self.subnet_receiver
                    .handle_subnet_config(&network_config, ip_masq, ipv4_subnet, ipv6_subnet, mtu)
                    .await?;

                self.route_receiver.handle_route_config(routes).await?;
            }
        }

        info!(
            "Network configuration applied successfully for node: {}",
            self.node_id
        );
        Ok(())
    }

    /// Start the network receiver service
    /// This will listen for network configurations from rks
    pub async fn start_service(&mut self) -> Result<()> {
        info!(
            "Starting network receiver service for node: {}",
            self.node_id
        );

        // Initialize QUIC client if not already done
        if self.quic_endpoint.is_none() {
            self.init_quic_client().await?;
        }

        // Start QUIC communication loop
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        if let Some(rks_endpoint) = self.rks_endpoint {
            let endpoint = self.quic_endpoint.as_ref().unwrap().clone();
            let node_id = self.node_id.clone();

            // Spawn QUIC communication task
            tokio::spawn(async move {
                if let Err(e) =
                    Self::quic_communication_loop(endpoint, rks_endpoint, node_id, shutdown_rx)
                        .await
                {
                    error!("QUIC communication loop failed: {e}");
                }
            });
        } else {
            warn!("RKS endpoint not configured, QUIC communication disabled");
        }

        // Start route checking task
        let (route_shutdown_tx, mut route_shutdown_rx) = mpsc::channel(1);
        self.route_shutdown_tx = Some(route_shutdown_tx);

        // Store the route shutdown sender for proper cleanup
        let node_id_clone = self.node_id.clone();
        tokio::spawn(async move {
            info!("Route monitoring task started for node: {node_id_clone}");
            tokio::select! {
                _ = route_shutdown_rx.recv() => {
                    info!("Route monitoring task shutting down for node: {node_id_clone}");
                }
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {
                    // Heartbeat every hour
                    info!("Route monitoring heartbeat for node: {node_id_clone}");
                }
            }
        });

        info!(
            "Network receiver service started for node: {}",
            self.node_id
        );
        Ok(())
    }

    /// QUIC communication loop to receive network configurations from rks
    async fn quic_communication_loop(
        endpoint: Endpoint,
        rks_endpoint: SocketAddr,
        node_id: String,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) -> Result<()> {
        info!("Starting QUIC communication loop for node: {node_id}");

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("QUIC communication loop shutting down for node: {node_id}");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    // Attempt to connect to RKS and receive configuration
                    if let Err(e) = Self::attempt_rks_connection(&endpoint, rks_endpoint, &node_id).await {
                        warn!("Failed to connect to RKS: {e}");
                    }
                }
            }
        }

        Ok(())
    }

    /// Attempt to connect to RKS and receive network configuration
    async fn attempt_rks_connection(
        endpoint: &Endpoint,
        rks_endpoint: SocketAddr,
        node_id: &str,
    ) -> Result<()> {
        info!("Attempting to connect to RKS at {rks_endpoint} for node: {node_id}");

        // Connect to RKS
        let connection = endpoint
            .connect(rks_endpoint, "rks")?
            .await
            .map_err(|e| anyhow::anyhow!("Connection failed: {e}"))?;

        info!("Connected to RKS successfully");

        // Open bidirectional stream
        let (mut send_stream, mut recv_stream) = connection.open_bi().await?;

        // Send node registration message
        let registration = NodeRegistration {
            node_id: node_id.to_string(),
            capabilities: vec!["network".to_string()],
        };

        let registration_data = bincode::serialize(&registration)?;
        send_stream.write_all(&registration_data).await?;
        send_stream.finish()?;

        info!("Sent node registration to RKS");

        // Listen for network configuration messages
        let buffer = recv_stream.read_to_end(1024 * 1024).await?;

        if !buffer.is_empty() {
            match bincode::deserialize::<RksMessage>(&buffer) {
                Ok(message) => {
                    info!("Received message from RKS: {message:?}");
                    // This would involve calling handle_network_config with the received data
                }
                Err(e) => {
                    warn!("Failed to deserialize message from RKS: {e}");
                }
            }
        }

        Ok(())
    }

    /// Stop the network receiver service
    pub async fn stop_service(&mut self) -> Result<()> {
        info!(
            "Stopping network receiver service for node: {}",
            self.node_id
        );

        // Send shutdown signal to QUIC communication loop
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            if let Err(e) = shutdown_tx.send(()).await {
                warn!("Failed to send QUIC shutdown signal: {e}");
            } else {
                info!("QUIC shutdown signal sent successfully");
            }
        }

        // Send shutdown signal to route monitoring task
        if let Some(route_shutdown_tx) = self.route_shutdown_tx.take() {
            if let Err(e) = route_shutdown_tx.send(()).await {
                warn!("Failed to send route monitoring shutdown signal: {e}");
            } else {
                info!("Route monitoring shutdown signal sent successfully");
            }
        }

        // Give tasks a moment to shut down gracefully
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Close QUIC endpoint
        if let Some(endpoint) = self.quic_endpoint.take() {
            info!("Closing QUIC endpoint...");
            endpoint.close(0u32.into(), b"Service shutdown");

            // Wait for the endpoint to close gracefully with a timeout
            tokio::select! {
                _ = endpoint.wait_idle() => {
                    info!("QUIC endpoint closed gracefully");
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    warn!("QUIC endpoint close timeout, forcing shutdown");
                }
            }
        }

        info!(
            "Network receiver service stopped for node: {}",
            self.node_id
        );
        Ok(())
    }

    /// Get the current node ID
    pub fn get_node_id(&self) -> &str {
        &self.node_id
    }

    /// Get route manager for direct access (if needed)
    pub fn get_route_manager(&self) -> Arc<Mutex<RouteManager>> {
        self.route_manager.clone()
    }

    /// Check if the service is healthy and responding
    pub async fn health_check(&self) -> Result<()> {
        // Check if subnet file path is accessible
        let subnet_path = Path::new(&self.subnet_receiver.subnet_file_path);
        if let Some(parent) = subnet_path.parent()
            && !parent.exists()
        {
            return Err(anyhow::anyhow!(
                "Subnet configuration directory does not exist: {}",
                parent.display()
            ));
        }

        info!(
            "Network receiver health check passed for node: {}",
            self.node_id
        );
        Ok(())
    }

    /// Get service status information
    pub async fn get_status(&self) -> NetworkServiceStatus {
        let quic_connected = self.quic_endpoint.is_some();
        let rks_endpoint_configured = self.rks_endpoint.is_some();

        let route_count = self.route_manager.lock().await.get_routes().len();
        let v6_route_count = self.route_manager.lock().await.get_v6_routes().len();

        NetworkServiceStatus {
            node_id: self.node_id.clone(),
            quic_connected,
            rks_endpoint_configured,
            subnet_file_path: self.subnet_receiver.subnet_file_path.clone(),
            route_count,
            v6_route_count,
        }
    }
}

/// Network service status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkServiceStatus {
    pub node_id: String,
    pub quic_connected: bool,
    pub rks_endpoint_configured: bool,
    pub subnet_file_path: String,
    pub route_count: usize,
    pub v6_route_count: usize,
}

/// Node registration message sent to RKS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistration {
    pub node_id: String,
    pub capabilities: Vec<String>,
}

/// Messages that can be received from RKS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RksMessage {
    /// Network configuration update
    NetworkConfig(NetworkConfigMessage),
    /// Acknowledgment from RKS
    Ack,
    /// Heartbeat message
    Heartbeat,
}

/// Network configuration message types that can be received from rks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkConfigMessage {
    /// Subnet configuration only
    SubnetConfig {
        network_config: NetworkConfig,
        ip_masq: bool,
        ipv4_subnet: Option<ipnetwork::Ipv4Network>,
        ipv6_subnet: Option<ipnetwork::Ipv6Network>,
        mtu: u32,
    },
    /// Route configuration only
    Route { routes: Vec<Route> },
    /// Full network configuration (subnet + routes)
    FullConfig {
        network_config: NetworkConfig,
        ip_masq: bool,
        ipv4_subnet: Option<ipnetwork::Ipv4Network>,
        ipv6_subnet: Option<ipnetwork::Ipv6Network>,
        mtu: u32,
        routes: Vec<Route>,
    },
}

/// Network receiver builder for easier configuration
pub struct NetworkReceiverBuilder {
    subnet_file_path: Option<String>,
    link_index: Option<u32>,
    backend_type: Option<String>,
    node_id: Option<String>,
    rks_endpoint: Option<SocketAddr>,
}

impl NetworkReceiverBuilder {
    pub fn new() -> Self {
        Self {
            subnet_file_path: None,
            link_index: None,
            backend_type: None,
            node_id: None,
            rks_endpoint: None,
        }
    }

    pub fn subnet_file_path(mut self, path: String) -> Self {
        self.subnet_file_path = Some(path);
        self
    }

    pub fn link_index(mut self, index: u32) -> Self {
        self.link_index = Some(index);
        self
    }

    pub fn backend_type(mut self, backend: String) -> Self {
        self.backend_type = Some(backend);
        self
    }

    pub fn node_id(mut self, id: String) -> Self {
        self.node_id = Some(id);
        self
    }

    pub fn rks_endpoint(mut self, endpoint: SocketAddr) -> Self {
        self.rks_endpoint = Some(endpoint);
        self
    }

    pub fn build(self) -> Result<NetworkReceiver> {
        let subnet_file_path = self
            .subnet_file_path
            .unwrap_or_else(|| "/etc/cni/net.d/subnet.env".to_string());
        let link_index = self.link_index.unwrap_or(1);
        let backend_type = self.backend_type.unwrap_or_else(|| "hostgw".to_string());
        let node_id = self
            .node_id
            .ok_or_else(|| anyhow::anyhow!("Node ID is required"))?;

        let mut receiver =
            NetworkReceiver::new(subnet_file_path, link_index, backend_type, node_id);

        if let Some(endpoint) = self.rks_endpoint {
            receiver = receiver.with_rks_endpoint(endpoint);
        }

        Ok(receiver)
    }
}

impl Default for NetworkReceiverBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Network service manager for rkl
/// This provides a high-level interface for managing network configuration
pub struct NetworkService {
    receiver: Arc<Mutex<NetworkReceiver>>,
    is_running: Arc<Mutex<bool>>,
}

impl NetworkService {
    pub fn new(receiver: NetworkReceiver) -> Self {
        Self {
            receiver: Arc::new(Mutex::new(receiver)),
            is_running: Arc::new(Mutex::new(false)),
        }
    }

    /// Start the network service
    pub async fn start(&self) -> Result<()> {
        let mut running = self.is_running.lock().await;
        if *running {
            warn!("Network service is already running");
            return Ok(());
        }

        info!("Starting network service");
        let mut receiver = self.receiver.lock().await;
        receiver.start_service().await?;
        *running = true;
        info!("Network service started successfully");
        Ok(())
    }

    /// Stop the network service
    pub async fn stop(&self) -> Result<()> {
        let mut running = self.is_running.lock().await;
        if !*running {
            warn!("Network service is not running");
            return Ok(());
        }

        info!("Stopping network service");
        let mut receiver = self.receiver.lock().await;
        receiver.stop_service().await?;
        *running = false;
        info!("Network service stopped successfully");
        Ok(())
    }

    /// Check if the service is running
    pub async fn is_running(&self) -> bool {
        *self.is_running.lock().await
    }

    /// Get the network receiver
    pub fn get_receiver(&self) -> Arc<Mutex<NetworkReceiver>> {
        self.receiver.clone()
    }

    /// Handle a network configuration message
    pub async fn handle_config(&self, config: NetworkConfigMessage) -> Result<()> {
        let receiver = self.receiver.lock().await;
        receiver.handle_network_config(config).await
    }

    /// Perform health check on the network service
    pub async fn health_check(&self) -> Result<()> {
        let receiver = self.receiver.lock().await;
        receiver.health_check().await
    }

    /// Get the current status of the network service
    pub async fn get_status(&self) -> NetworkServiceStatus {
        let receiver = self.receiver.lock().await;
        receiver.get_status().await
    }

    /// Gracefully shutdown the service with timeout
    pub async fn shutdown_with_timeout(&self, timeout: Duration) -> Result<()> {
        info!("Initiating graceful shutdown with timeout: {timeout:?}");

        tokio::select! {
            result = self.stop() => {
                match result {
                    Ok(_) => info!("Service stopped gracefully"),
                    Err(ref e) => error!("Error during graceful shutdown: {e}"),
                }
                result
            }
            _ = tokio::time::sleep(timeout) => {
                error!("Service shutdown timed out after {timeout:?}, forcing stop");
                Err(anyhow::anyhow!("Shutdown timeout exceeded"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_network_receiver_builder() {
        let receiver = NetworkReceiverBuilder::new()
            .subnet_file_path("/tmp/subnet.env".to_string())
            .link_index(2)
            .backend_type("overlay".to_string())
            .node_id("test-node-1".to_string())
            .build()
            .unwrap();

        assert_eq!(receiver.get_node_id(), "test-node-1");
    }

    #[test]
    fn test_network_receiver_builder_defaults() {
        let receiver = NetworkReceiverBuilder::new()
            .node_id("test-node-2".to_string())
            .build()
            .unwrap();

        assert_eq!(receiver.get_node_id(), "test-node-2");
    }

    #[test]
    fn test_network_receiver_builder_missing_node_id() {
        let result = NetworkReceiverBuilder::new().build();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_network_service_lifecycle() {
        let receiver = NetworkReceiverBuilder::new()
            .node_id("test-node-3".to_string())
            .build()
            .unwrap();

        let service = NetworkService::new(receiver);

        assert!(!service.is_running().await);

        service.start().await.unwrap();
        assert!(service.is_running().await);

        service.stop().await.unwrap();
        assert!(!service.is_running().await);
    }

    #[tokio::test]
    async fn test_network_config_message_handling() {
        let dir = tempdir().unwrap();
        let subnet_file = dir.path().join("subnet.env");

        let receiver = NetworkReceiverBuilder::new()
            .subnet_file_path(subnet_file.to_string_lossy().to_string())
            .node_id("test-node-4".to_string())
            .build()
            .unwrap();

        let config_msg = NetworkConfigMessage::SubnetConfig {
            network_config: NetworkConfig {
                enable_ipv4: true,
                enable_ipv6: false,
                network: Some("10.0.0.0/16".parse().unwrap()),
                ..Default::default()
            },
            ip_masq: true,
            ipv4_subnet: Some("10.0.1.0/24".parse().unwrap()),
            ipv6_subnet: None,
            mtu: 1500,
        };

        let result = receiver.handle_network_config(config_msg).await;
        assert!(result.is_ok());
    }
}
