use anyhow::Result;
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use libcni::ip::route::{self, Route, RouteFilterMask};
use log::{debug, error, info, warn};
use netlink_packet_route::{AddressFamily, route::RouteType};
use std::{net::IpAddr, sync::Arc};
use tokio::{
    select,
    sync::mpsc,
    time::{Duration, sleep},
};

use common::lease::Lease;
/// Route manager for handling system routing table operations on rkl node
pub struct RouteManager {
    link_index: u32,
    backend_type: String,
    routes: Vec<Route>,
    v6routes: Vec<Route>,
}

pub trait RouteListOps {
    fn add_to_route_list(&mut self, route: Route, family: AddressFamily);
    fn remove_from_route_list(&mut self, route: &Route, family: AddressFamily);
}

impl RouteListOps for RouteManager {
    fn add_to_route_list(&mut self, route: Route, family: AddressFamily) {
        match family {
            AddressFamily::Inet => {
                self.routes = add_to_route_list(route, std::mem::take(&mut self.routes));
            }
            AddressFamily::Inet6 => {
                self.v6routes = add_to_route_list(route, std::mem::take(&mut self.v6routes));
            }
            _ => {}
        }
    }

    fn remove_from_route_list(&mut self, route: &Route, family: AddressFamily) {
        match family {
            AddressFamily::Inet => {
                self.routes = remove_from_route_list(route, &self.routes);
            }
            AddressFamily::Inet6 => {
                self.v6routes = remove_from_route_list(route, &self.v6routes);
            }
            _ => {}
        }
    }
}

impl RouteManager {
    pub fn new(link_index: u32, backend_type: String) -> Self {
        let routes: Vec<Route> = vec![];
        let v6routes: Vec<Route> = vec![];
        Self {
            link_index,
            backend_type,
            routes,
            v6routes,
        }
    }

    /// Generate IPv4 route form a lease
    pub fn get_route_form_lease(&self, lease: &Lease) -> Option<Route> {
        if !lease.enable_ipv4 {
            return None;
        }

        Some(Route {
            dst: Some(IpNetwork::V4(lease.subnet)),
            gateway: Some(IpAddr::V4(lease.attrs.public_ip)),
            oif_index: Some(self.link_index),
            metric: None,
            ..Default::default()
        })
    }

    /// Generate IPv6 route form a lease
    pub fn get_v6_route_form_lease(&self, lease: &Lease) -> Option<Route> {
        if !lease.enable_ipv6 {
            return None;
        }

        let subnet_v6 = lease.ipv6_subnet?;
        let gateway_v6 = lease.attrs.public_ipv6?;

        Some(Route {
            dst: Some(IpNetwork::V6(subnet_v6)),
            gateway: Some(IpAddr::V6(gateway_v6)),
            oif_index: Some(self.link_index),
            metric: None,
            ..Default::default()
        })
    }

    /// Add a route to the system routing table
    pub async fn add_route(&mut self, route: &Route) -> Result<()> {
        info!(
            "Adding IPv4 route: {:?} via {:?} dev {:?} ({})",
            route.dst, route.gateway, route.oif_index, self.backend_type
        );
        route_add_with_check(route.clone(), AddressFamily::Inet, self).await
    }

    /// Add an IPv6 route to the system routing table
    pub async fn add_v6_route(&mut self, route: &Route) -> Result<()> {
        info!(
            "Adding IPv6 route: {:?} via {:?} dev {:?} ({})",
            route.dst, route.gateway, route.oif_index, self.backend_type
        );

        route_add_with_check(route.clone(), AddressFamily::Inet6, self).await
    }

    /// Remove a route from the system routing table
    pub async fn delete_route(&self, route: &Route) -> Result<()> {
        info!(
            "Removing IPv4 route: {:?} via {:?} dev {:?} ({})",
            route.dst, route.gateway, route.oif_index, self.backend_type
        );
        route::route_del(route.clone()).await
    }

    /// Synchronize routes with a list of leases
    pub async fn sync_routes(&mut self, leases: &[Lease]) -> Result<()> {
        debug!("Synchronizing routes for {} leases", leases.len());

        for lease in leases {
            if let Some(route) = self.get_route_form_lease(lease)
                && let Err(e) = self.add_route(&route).await
            {
                warn!("Failed to add IPv4 route for lease {}: {}", lease.subnet, e);
            }

            if let Some(route_v6) = self.get_v6_route_form_lease(lease)
                && let Err(e) = self.add_v6_route(&route_v6).await
            {
                warn!(
                    "Failed to add IPv6 route for lease {:?}: {}",
                    lease.ipv6_subnet, e
                );
            }
        }
        Ok(())
    }

    /// Clean up all routes for a list of leases
    pub async fn cleanup_routes(&self, leases: &[Lease]) -> Result<()> {
        debug!("Cleaning up routes for {} leases", leases.len());

        for lease in leases {
            if let Some(route) = self.get_route_form_lease(lease)
                && let Err(e) = self.delete_route(&route).await
            {
                warn!(
                    "Failed to remove IPv4 route for lease {}: {}",
                    lease.subnet, e
                );
            }

            if let Some(route_v6) = self.get_v6_route_form_lease(lease)
                && let Err(e) = self.delete_route(&route_v6).await
            {
                warn!(
                    "Failed to remove IPv6 route for lease {:?}: {}",
                    lease.ipv6_subnet, e
                );
            }
        }

        Ok(())
    }

    /// Check if routes exist in the routing table and recover them if missing
    pub async fn check_subnet_exist_in_v4_routes(&self) {
        if let Err(e) = self
            .check_subnet_exist_in_routes(&self.routes, AddressFamily::Inet)
            .await
        {
            error!("Error checking v4 routes: {e:?}");
        }
    }

    pub async fn check_subnet_exist_in_v6_routes(&self) {
        if let Err(e) = self
            .check_subnet_exist_in_routes(&self.v6routes, AddressFamily::Inet6)
            .await
        {
            error!("Error checking v6 routes: {e:?}");
        }
    }

    async fn check_subnet_exist_in_routes(
        &self,
        routes: &[Route],
        family: AddressFamily,
    ) -> Result<()> {
        let route_list = match route::route_list(family).await {
            Ok(list) => list,
            Err(err) => {
                error!("Error fetching route list. Will automatically retry: {err:?}");
                return Err(err);
            }
        };

        for route in routes {
            if route.dst.is_none() {
                continue;
            }

            let exists = route_list.iter().any(|r| route::route_equal(r, route));

            if !exists {
                match route::route_add(route.clone()).await {
                    Ok(_) => {
                        info!("Route recovered: {:?} -> {:?}", route.dst, route.gateway);
                    }
                    Err(e) => {
                        error!(
                            "Error recovering route to {:?} {:?}: {:?}",
                            route.dst, route.gateway, e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Periodic route checking task
    pub async fn route_check(
        self: Arc<Self>,
        mut shutdown_rx: mpsc::Receiver<()>,
        interval_secs: u64,
    ) {
        loop {
            select! {
                _ = shutdown_rx.recv() => {
                    info!("Route check task shutting down");
                    break;
                }
                _ = sleep(Duration::from_secs(interval_secs)) => {
                    self.check_subnet_exist_in_v4_routes().await;
                    self.check_subnet_exist_in_v6_routes().await;
                }
            }
        }
    }

    /// Get current routes
    pub fn get_routes(&self) -> &[Route] {
        &self.routes
    }

    /// Get current IPv6 routes
    pub fn get_v6_routes(&self) -> &[Route] {
        &self.v6routes
    }
}

pub fn add_to_route_list(route: Route, mut routes: Vec<Route>) -> Vec<Route> {
    for r in &routes {
        if route::route_equal(r, &route) {
            return routes;
        }
    }
    routes.push(route);
    routes
}

pub fn remove_from_route_list(target: &Route, routes: &[Route]) -> Vec<Route> {
    let mut result = Vec::with_capacity(routes.len());
    let mut removed = false;

    for r in routes {
        if !removed && route::route_equal(r, target) {
            removed = true;
            continue;
        }
        result.push(r.clone());
    }

    result
}

pub async fn route_add_with_check<T>(route: Route, family: AddressFamily, ops: &mut T) -> Result<()>
where
    T: RouteListOps,
{
    ops.add_to_route_list(route.clone(), family);

    let filter = Route {
        dst: route.dst,
        gateway: None,
        oif_index: None,
        src: None,
        route_type: None,
        metric: None,
    };

    let mask = RouteFilterMask {
        dst: true,
        ..Default::default()
    };

    let mut route_list = route::route_list_filtered_vec(family, Some(&filter), mask)
        .await
        .unwrap_or_else(|err| {
            warn!("Unable to list routes: {err:?}");
            vec![]
        });

    if let Some(existing) = route_list.first()
        && !route::route_equal(existing, &route)
    {
        warn!(
            "Replacing existing route to {:?} with {:?}",
            existing.dst, route.dst
        );
        if let Err(err) = route::route_del(existing.clone()).await {
            error!("Error deleting route to {:?}: {:?}", existing.dst, err);
            return Ok(());
        }
        ops.remove_from_route_list(existing, family);
    }

    route_list = route::route_list_filtered_vec(family, Some(&filter), mask)
        .await
        .unwrap_or_else(|err| {
            warn!("Unable to list routes: {err:?}");
            vec![]
        });

    if let Some(existing) = route_list.first()
        && route::route_equal(existing, &route)
    {
        info!("Route to {:?} already exists, skipping.", route.dst);
        return Ok(());
    }

    if let Err(err) = route::route_add(route.clone()).await {
        error!("Error adding route to {:?}: {:?}", route.dst, err);
        return Ok(());
    }

    let _ = route::route_list_filtered_vec(family, Some(&filter), mask)
        .await
        .map_err(|err| {
            warn!("Unable to list routes: {err:?}");
        });

    Ok(())
}

pub async fn add_blackhole_v4_route(dst: Ipv4Network) -> Result<()> {
    let dst = IpNetwork::V4(dst);
    let route = Route {
        dst: Some(dst),
        route_type: Some(RouteType::BlackHole),
        ..Default::default()
    };

    let mask = RouteFilterMask {
        dst: true,
        route_type: true,
        ..Default::default()
    };

    let routes = route::route_list_filtered_vec(AddressFamily::Inet, Some(&route), mask).await?;

    if routes.is_empty() {
        route::route_add(route).await?;
        info!("Blackhole route added for {dst}");
    }

    Ok(())
}

pub async fn add_blackhole_v6_route(dst: Ipv6Network) -> Result<()> {
    let dst = IpNetwork::V6(dst);
    let route = Route {
        dst: Some(dst),
        route_type: Some(RouteType::BlackHole),
        ..Default::default()
    };

    let mask = RouteFilterMask {
        dst: true,
        route_type: true,
        ..Default::default()
    };

    let routes = route::route_list_filtered_vec(AddressFamily::Inet6, Some(&route), mask).await?;

    if routes.is_empty() {
        route::route_add(route).await?;
        info!("Blackhole route added for {dst}");
    }

    Ok(())
}

/// Route receiver for handling route configurations from rks
/// This will be called when receiving route updates from rks
pub struct RouteReceiver {
    route_manager: Arc<tokio::sync::Mutex<RouteManager>>,
}

impl RouteReceiver {
    pub fn new(route_manager: Arc<tokio::sync::Mutex<RouteManager>>) -> Self {
        Self { route_manager }
    }

    /// Handle received route configuration from rks
    /// This function will be called when rks sends route configuration
    pub async fn handle_route_config(&self, routes: Vec<Route>) -> Result<()> {
        info!("Received {} route configurations from rks", routes.len());

        let mut manager = self.route_manager.lock().await;

        for route in routes {
            match &route.dst {
                Some(IpNetwork::V4(_)) => {
                    if let Err(e) = manager.add_route(&route).await {
                        error!("Failed to add IPv4 route {route:?}: {e}");
                    } else {
                        info!("Successfully added IPv4 route: {route:?}");
                    }
                }
                Some(IpNetwork::V6(_)) => {
                    if let Err(e) = manager.add_route(&route).await {
                        error!("Failed to add IPv6 route {route:?}: {e}");
                    } else {
                        info!("Successfully added IPv6 route: {route:?}");
                    }
                }
                None => {
                    warn!("Skipping route with no destination: {route:?}");
                }
            }
        }

        info!("Route configuration applied successfully");
        Ok(())
    }

    /// Receive route configurations from rks via QUIC
    /// This method will be called by the main NetworkReceiver QUIC loop
    pub async fn receive_from_rks(&self) -> Result<()> {
        // This method is now integrated with the main NetworkReceiver QUIC communication
        // The actual QUIC communication is handled by NetworkReceiver.quic_communication_loop()
        // When route configurations are received, they will be passed to handle_route_config()
        info!("Route receiver is ready to receive configurations from rks");
        Ok(())
    }

    /// Remove specific routes
    pub async fn remove_routes(&self, routes: Vec<Route>) -> Result<()> {
        info!("Removing {} route configurations", routes.len());

        let manager = self.route_manager.lock().await;

        for route in routes {
            if let Err(e) = manager.delete_route(&route).await {
                error!("Failed to remove route {route:?}: {e}");
            } else {
                info!("Successfully removed route: {route:?}");
            }
        }

        info!("Route removal completed");
        Ok(())
    }
}
