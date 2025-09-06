use crate::network::{
    backend::route::RouteListOps, config::Config, iface::check_hostgw_compatibility,
    lease::LeaseWatcher, manager::LocalManager,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use common::{
    ExternalInterface,
    lease::{Event, EventType, Lease, LeaseAttrs},
};
use log::{error, info, warn};
use netlink_packet_route::AddressFamily;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use super::{Backend, Network, SimpleNetwork, route::RouteManager};

/// Host-GW backend implementation
/// This backend uses host routing table to implement container networking
/// without packet encapsulation (no VXLAN, etc.)
pub struct HostgwBackend {
    pub ext_iface: ExternalInterface,
    pub subnet_manager: Arc<Mutex<LocalManager>>,
}

impl HostgwBackend {
    /// Create a new Host-GW backend instance
    pub fn new(ext_iface: ExternalInterface, subnet_manager: LocalManager) -> Result<Self> {
        // Validate that the interface is compatible with host-gw
        check_hostgw_compatibility(&ext_iface)?;

        info!(
            "Initializing host-gw backend on interface {} ({:?})",
            ext_iface.iface.name, ext_iface.iface_addr
        );

        Ok(HostgwBackend {
            ext_iface,
            subnet_manager: Arc::new(Mutex::new(subnet_manager)),
        })
    }
}

#[async_trait]
impl Backend for HostgwBackend {
    async fn register_network(&self, _config: &Config) -> Result<Arc<Mutex<dyn Network>>> {
        info!("Registering host-gw network with config");

        let route_network = RouteNetwork::new(
            self.ext_iface.clone(),
            self.subnet_manager.clone(),
            "host-gw".to_string(),
        )?;

        let network: Arc<Mutex<dyn Network>> = Arc::new(Mutex::new(route_network));

        // Acquire lease for this node
        let lease_attrs = LeaseAttrs {
            public_ip: self.ext_iface.ext_addr.expect("Don't have the ext_addr"),
            public_ipv6: self.ext_iface.ext_v6_addr,
            backend_type: "host-gw".to_string(),
            ..Default::default()
        };

        let lease = {
            let manager = self.subnet_manager.lock().await;
            manager.acquire_lease(&lease_attrs).await?
        };

        {
            let mut net = network.lock().await;
            net.set_lease(lease).await?;
        }

        info!("Host-gw network registered successfully");
        Ok(network)
    }

    fn backend_type(&self) -> &str {
        "host-gw"
    }
}

/// Route-based network implementation for host-gw backend
pub struct RouteNetwork {
    pub simple_network: SimpleNetwork,
    pub route_manager: RouteManager,
    pub lease_watcher: Option<LeaseWatcher>,
    pub backend_type: String,
    pub subnet_manager: Arc<Mutex<LocalManager>>,
    pub running: bool,
}

impl RouteNetwork {
    pub fn new(
        ext_iface: ExternalInterface,
        subnet_manager: Arc<Mutex<LocalManager>>,
        backend_type: String,
    ) -> Result<Self> {
        let route_manager = RouteManager::new(ext_iface.iface.index, backend_type.clone());

        let simple_network = SimpleNetwork {
            ext_iface,
            lease: None,
        };

        Ok(RouteNetwork {
            simple_network,
            route_manager,
            lease_watcher: None,
            backend_type,
            subnet_manager,
            running: false,
        })
    }

    /// Handle lease events (add/remove routes)
    async fn handle_lease_event(&mut self, event: Event) -> Result<()> {
        match event.event_type {
            EventType::Added => {
                if let Some(lease) = event.lease {
                    if lease.attrs.backend_type != self.backend_type {
                        warn!(
                            "Ignoring non-{} subnet: type={}",
                            self.backend_type, lease.attrs.backend_type
                        );
                        return Ok(());
                    }

                    info!(
                        "Subnet added: {} -> {}",
                        lease.subnet, lease.attrs.public_ip
                    );

                    // Add routes for the new lease
                    if let Some(route) = self.route_manager.get_route_form_lease(&lease) {
                        self.route_manager.add_route(&route).await?;
                    }

                    if let Some(route_v6) = self.route_manager.get_v6_route_form_lease(&lease) {
                        self.route_manager.add_v6_route(&route_v6).await?;
                    }
                }
            }
            EventType::Removed => {
                if let Some(lease) = event.lease {
                    if lease.attrs.backend_type != self.backend_type {
                        warn!(
                            "Ignoring non-{} subnet: type={}",
                            self.backend_type, lease.attrs.backend_type
                        );
                        return Ok(());
                    }

                    info!(
                        "Subnet removed: {} -> {}",
                        lease.subnet, lease.attrs.public_ip
                    );

                    // Remove routes for the removed lease
                    if let Some(route) = self.route_manager.get_route_form_lease(&lease) {
                        self.route_manager
                            .remove_from_route_list(&route, AddressFamily::Inet);
                        self.route_manager.delete_route(&route).await?;
                    }

                    if let Some(route_v6) = self.route_manager.get_v6_route_form_lease(&lease) {
                        self.route_manager
                            .remove_from_route_list(&route_v6, AddressFamily::Inet6);
                        self.route_manager.delete_route(&route_v6).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Watch for lease changes and update routes accordingly
    async fn watch_leases(&mut self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(100);

        // Start watching for lease changes
        let manager = self.subnet_manager.clone();
        tokio::spawn(async move {
            let manager = manager.lock().await;
            if let Err(e) = manager.watch_leases(tx).await {
                error!("Failed to watch leases: {e}");
            }
        });

        // Process lease change events
        while let Some(results) = rx.recv().await {
            for result in results {
                // Handle reset events (initial lease list)
                if !result.snapshot.is_empty() {
                    info!(
                        "Received lease snapshot with {} leases",
                        result.snapshot.len()
                    );

                    if let Some(watcher) = &mut self.lease_watcher {
                        let events = watcher.reset(result.snapshot);
                        for event in events {
                            if let Err(e) = self.handle_lease_event(event).await {
                                error!("Failed to handle lease event: {e}");
                            }
                        }
                    }
                }

                // Handle individual events
                if let Some(watcher) = &mut self.lease_watcher {
                    let events = watcher.update(result.events);
                    for event in events {
                        if let Err(e) = self.handle_lease_event(event).await {
                            error!("Failed to handle lease event: {e}");
                        }
                    }
                }
            }

            if !self.running {
                break;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Network for RouteNetwork {
    async fn get_lease(&self) -> Result<Lease> {
        self.simple_network.get_lease().await
    }

    async fn set_lease(&mut self, lease: Lease) -> Result<()> {
        info!(
            "Setting lease for host-gw network: {} (IPv6: {:?})",
            lease.subnet, lease.ipv6_subnet
        );

        // Initialize lease watcher with our own lease
        self.lease_watcher = Some(LeaseWatcher {
            own_lease: lease.clone(),
            leases: vec![],
        });

        self.simple_network.set_lease(lease).await
    }

    async fn run(&mut self) -> Result<()> {
        if self.running {
            return Err(anyhow!("Network is already running"));
        }

        info!(
            "Starting host-gw network on interface {}",
            self.simple_network.ext_iface.iface.name
        );
        self.running = true;

        // Start the simple network
        self.simple_network.run().await?;

        // Start watching for leases changes
        if let Err(e) = self.watch_leases().await {
            error!("Lease watching failed: {e}");
            self.running = false;
            return Err(e);
        }

        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }

        info!("Stopping host-gw network");
        self.running = false;

        // Clean up routes for all known leases
        if let Some(watcher) = &self.lease_watcher {
            self.route_manager.cleanup_routes(&watcher.leases).await?;
        }

        self.simple_network.stop().await?;

        info!("Host-gw network stopped");
        Ok(())
    }

    fn mtu(&self) -> Option<u32> {
        self.simple_network.mtu()
    }

    fn backend_type(&self) -> &str {
        &self.backend_type
    }
}
