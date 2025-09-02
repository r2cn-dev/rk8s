use crate::api::xlinestore::XlineStore;
use crate::commands::{create, delete};
use crate::network::{
    self,
    backend::route,
    lease::{Lease, LeaseAttrs, LeaseWatchResult},
    manager::LocalManager,
};
use anyhow::{Context, Result};
use common::{NodeNetworkConfig, PodTask, RksMessage};
use ipnetwork::{Ipv4Network, Ipv6Network};
use libcni::ip::route::Route;
use quinn::{Connection, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, mpsc};
use log::{info, warn, error};
use rcgen;
use serde_yaml;
use etcd_client;
use futures_util::StreamExt;

#[derive(Clone)]
pub struct WorkerSession {
    pub tx: mpsc::Sender<RksMessage>,
    pub cancel_notify: Arc<Notify>,
    pub lease: Arc<Mutex<Lease>>,
}

#[derive(Default)]
pub struct NodeRegistry {
    inner: Mutex<HashMap<String, Arc<WorkerSession>>>,
}

impl NodeRegistry {
    pub async fn register(&self, node_id: String, session: Arc<WorkerSession>) {
        let mut inner = self.inner.lock().await;
        inner.insert(node_id, session);
    }

    pub async fn unregister(&self, node_id: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.remove(node_id) {
            session.cancel_notify.notify_one();
        }
    }

    pub async fn get(&self, node_id: &str) -> Option<Arc<WorkerSession>> {
        let inner = self.inner.lock().await;
        inner.get(node_id).cloned()
    }
}

/// Launch the RKS server to listen for incoming QUIC connections.
/// Each connection will be handled in a dedicated task.
pub async fn serve(
    addr: String,
    xline_store: Arc<XlineStore>,
    local_manager: Arc<LocalManager>,
) -> anyhow::Result<()> {
    info!("Starting server with address: {}", addr);

    // Create QUIC endpoint and server certificate
    let endpoint = make_server_endpoint(addr.parse()?).await?;
    info!("QUIC server listening on {}", addr);

    let node_registry = Arc::new(NodeRegistry::default());

    // Channel for receiving lease watch results
    let (lease_tx, mut lease_rx) = mpsc::channel::<Vec<LeaseWatchResult>>(16);
    let local_manager_clone = local_manager.clone();
    tokio::spawn(async move {
        local_manager_clone.watch_leases(lease_tx).await.unwrap();
    });

    // Spawn task to propagate lease updates to workers
    let node_registry_clone = node_registry.clone();
    tokio::spawn(async move {
        while let Some(results) = lease_rx.recv().await {
            let leases = results
                .iter()
                .flat_map(|r| r.snapshot.clone())
                .collect::<Vec<_>>();
            info!("[server] received all leases: {leases:?}");
            let node_ids: Vec<String> = leases.iter().map(|l| l.attrs.node_id.clone()).collect();
            for node_id in node_ids {
                let routes = calculate_routes_for_node(&node_id, &leases);
                info!("[server] sending routes to {node_id}: {routes:?}");
                let msg = RksMessage::UpdateRoutes(node_id.clone(), routes);
                if let Some(worker) = node_registry_clone.get(&node_id).await {
                    if let Err(e) = worker.tx.try_send(msg) {
                        error!("Failed to enqueue message for {node_id}: {e:?}");
                    }
                } else {
                    error!("No active worker for {node_id}");
                }
            }
        }
    });

    // Accept loop
    loop {
        let connecting = endpoint.accept().await;
        let xline_store = xline_store.clone();
        let local_manager = local_manager.clone();
        let node_registry = node_registry.clone();

        match connecting {
            Some(connecting) => {
                match connecting.await {
                    Ok(conn) => {
                        let remote_addr = conn.remote_address().to_string();
                        info!("[server] connection accepted: addr={remote_addr}");

                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                conn,
                                xline_store,
                                local_manager,
                                node_registry,
                            ).await {
                                error!("[server] handle_connection error: {e:?}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Connection failed: {}", e);
                    }
                }
            }
            None => break,
        }
    }
    Ok(())
}

/// Watches pod changes from Xline and pushes create/delete events to the worker node.
async fn watch_pods(
    xline_store: &Arc<XlineStore>,
    conn: &Connection,
    node_id: Option<String>,
) -> Result<()> {
    let node_id = match node_id {
        Some(id) => id,
        None => {
            error!("[watch_pods] no node_id provided, skipping message dispatch");
            return Ok(());
        }
    };

    // Get current snapshot and revision
    let (pods, rev) = xline_store.pods_snapshot_with_rev().await?;

    // Send snapshot to the worker
    for (pod_name, pod_yaml) in pods {
        if let Ok(pod_task) = serde_yaml::from_str::<PodTask>(&pod_yaml) {
            if pod_task.spec.nodename.as_deref() == Some(&node_id) {
                let msg = RksMessage::CreatePod(Box::new(pod_task));
                let data = bincode::serialize(&msg)?;
                if let Ok(mut stream) = conn.open_uni().await {
                    stream.write_all(&data).await?;
                    stream.finish()?;
                    info!("[watch_pods] sent existing pod to worker: {}", pod_name);
                }
            }
        } else {
            error!("Failed to parse pod YAML: {}", pod_yaml);
        }
    }

    // Start watching for changes
    let (mut watcher, mut stream) = xline_store.watch_pods(rev + 1).await?;
    while let Some(resp) = stream.next().await {
        match resp {
            Ok(resp) => {
                for event in resp.events() {
                    match event.event_type() {
                        etcd_client::EventType::Put => {
                            if let Some(kv) = event.kv() {
                                let pod_yaml = String::from_utf8_lossy(kv.value()).to_string();
                                if let Ok(pod_task) = serde_yaml::from_str::<PodTask>(&pod_yaml) {
                                    if pod_task.spec.nodename.as_deref() == Some(&node_id) {
                                        let msg = RksMessage::CreatePod(Box::new(pod_task));
                                        let data = bincode::serialize(&msg)?;
                                        if let Ok(mut stream) = conn.open_uni().await {
                                            stream.write_all(&data).await?;
                                            stream.finish()?;
                                            info!("[watch_pods] sent new pod to worker");
                                        }
                                    }
                                }
                            }
                        }
                        etcd_client::EventType::Delete => {
                            if let Some(kv) = event.prev_kv() {
                                let pod_name = String::from_utf8_lossy(kv.key())
                                    .replace("/registry/pods/", "");
                                let msg = RksMessage::DeletePod(pod_name.clone());
                                let data = bincode::serialize(&msg)?;
                                if let Ok(mut stream) = conn.open_uni().await {
                                    stream.write_all(&data).await?;
                                    stream.finish()?;
                                    info!("[watch_pods] sent delete pod to worker: {}", pod_name);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Watch error: {}", e);
                break;
            }
        }
    }

    watcher.cancel().await?;
    Ok(())
}

/// Handle an individual connection (worker or user).
/// Classifies client type and spawns watchers for workers.
async fn handle_connection(
    conn: Connection,
    xline_store: Arc<XlineStore>,
    local_manager: Arc<LocalManager>,
    node_registry: Arc<NodeRegistry>,
) -> Result<()> {
    let mut buf = vec![0u8; 4096];
    let mut is_worker = false;
    let mut node_id = None;
    let node_registry_clone = node_registry.clone();

    // Initial handshake
    if let Ok(mut recv) = conn.accept_uni().await {
        match recv.read(&mut buf).await {
            Ok(Some(n)) => {
                if let Ok(msg) = bincode::deserialize::<RksMessage>(&buf[..n]) {
                    match msg {
                        RksMessage::RegisterNode(node) => {
                            let id = node.metadata.name.clone();
                            if id.is_empty() {
                                error!("[server] invalid node: metadata.name is empty");
                                return Ok(());
                            }
                            is_worker = true;
                            node_id = Some(id.clone());

                            let ip = conn.remote_address().ip().to_string();
                            let node_yaml = serde_yaml::to_string(&*node)?;
                            xline_store.insert_node_yaml(&id, &node_yaml).await?;
                            info!("[server] registered worker node: {id}, ip: {ip}");

                            let config = local_manager.get_network_config().await?;
                            let (public_ip, public_ipv6) = match conn.remote_address().ip() {
                                IpAddr::V4(v4) => (v4, None),
                                IpAddr::V6(v6) => (Ipv4Addr::new(0, 0, 0, 0), Some(v6)),
                            };

                            // Acquire lease for this worker
                            let lease_attrs = LeaseAttrs {
                                public_ip,
                                public_ipv6,
                                backend_type: "hostgw".to_string(),
                                node_id: id.clone(),
                                ..Default::default()
                            };
                            let lease = local_manager.acquire_lease(&lease_attrs).await?;

                            // Create worker session
                            let (msg_tx, mut msg_rx) = mpsc::channel::<RksMessage>(32);
                            let conn_clone = conn.clone();
                            tokio::spawn(async move {
                                while let Some(msg) = msg_rx.recv().await {
                                    if let Ok(mut stream) = conn_clone.open_uni().await
                                        && let Ok(data) = bincode::serialize(&msg)
                                    {
                                        let _ = stream.write_all(&data).await;
                                        let _ = stream.finish();
                                    }
                                }
                            });

                            let cancel_notify = Arc::new(Notify::new());
                            let my_lease = Arc::new(Mutex::new(lease));
                            let session = Arc::new(WorkerSession {
                                tx: msg_tx.clone(),
                                cancel_notify: cancel_notify.clone(),
                                lease: my_lease.clone(),
                            });
                            node_registry.register(id.clone(), session.clone()).await;

                            // Send Ack
                            let response = RksMessage::Ack;
                            let data = bincode::serialize(&response)?;
                            if let Ok(mut stream) = conn.open_uni().await {
                                stream.write_all(&data).await?;
                                stream.finish()?;
                            }

                            // Send network config
                            let node_net_config = build_node_network_config(
                                id.clone(),
                                &config,
                                false,
                                Some(my_lease.lock().await.subnet),
                                my_lease.lock().await.ipv6_subnet,
                            )?;
                            let msg = RksMessage::SetNetwork(Box::new(node_net_config));
                            if let Some(worker) = node_registry_clone.get(&id).await {
                                let _ = worker.tx.try_send(msg);
                            }
                        }
                        RksMessage::UserRequest(_) => {
                            is_worker = false;
                            let response = RksMessage::Ack;
                            let data = bincode::serialize(&response)?;
                            if let Ok(mut stream) = conn.open_uni().await {
                                stream.write_all(&data).await?;
                                stream.finish()?;
                            }
                        }
                        _ => return Ok(()),
                    }
                }
            }
            _ => return Ok(()),
        }
    }

    // Spawn pod watcher for workers
    if is_worker && node_id.is_some() {
        let xline_store_clone = xline_store.clone();
        let conn_clone = conn.clone();
        let node_id_clone = node_id.clone().unwrap();
        let node_registry_clone = node_registry.clone();
        let local_manager_clone = local_manager.clone();

        let node_id_for_watch = node_id_clone.clone();
        tokio::spawn(async move {
            if let Err(e) = watch_pods(&xline_store_clone, &conn_clone, Some(node_id_for_watch)).await {
                error!("Watch pods error: {}", e);
            }
        });

        let node_id_for_lease = node_id_clone.clone();
        tokio::spawn(async move {
            if let Some(worker_session) = node_registry_clone.get(&node_id_for_lease).await {
                let my_lease_clone = worker_session.lease.clone();
                let cancel_notify_clone = worker_session.cancel_notify.clone();
                if let Err(e) = local_manager_clone
                    .complete_lease(my_lease_clone, cancel_notify_clone)
                    .await
                {
                    error!("[server] complete_lease error for node={node_id_clone}: {e:?}");
                }
            }
        });
    }

    // Main read loop
    loop {
        match conn.accept_uni().await {
            Ok(mut recv) => {
                let mut buf = vec![0u8; 4096];
                match recv.read(&mut buf).await {
                    Ok(Some(n)) => {
                        if let Ok(msg) = bincode::deserialize::<RksMessage>(&buf[..n]) {
                            if is_worker {
                                if let Err(e) = dispatch_worker(msg.clone(), &conn).await {
                                    error!("Error dispatching worker message: {}", e);
                                }
                            } else {
                                if let Err(e) = dispatch_user(msg.clone(), &xline_store, &conn).await {
                                    error!("Error dispatching user message: {}", e);
                                }
                            }
                        }
                    }
                    Ok(None) => info!("[server] stream closed"),
                    Err(e) => error!("[server] read error: {e}"),
                }
            }
            Err(e) => {
                error!("[server] connection error: {e}");
                break;
            }
        }
    }
    Ok(())
}

/// Dispatch worker-originated messages
async fn dispatch_worker(msg: RksMessage, conn: &Connection) -> Result<()> {
    match msg {
        RksMessage::Heartbeat(_node_id) => {
            info!("[worker dispatch] heartbeat received");
            let response = RksMessage::Ack;
            let data = bincode::serialize(&response)?;
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&data).await?;
                stream.finish()?;
            }
        }
        RksMessage::Error(err_msg) => error!("[worker dispatch] reported error: {err_msg}"),
        RksMessage::Ack => info!("[worker dispatch] received Ack"),
        _ => warn!("[worker dispatch] unknown or unexpected message from worker"),
    }
    Ok(())
}

/// Handle user-originated messages
pub async fn dispatch_user(
    msg: RksMessage,
    xline_store: &Arc<XlineStore>,
    conn: &Connection,
) -> Result<()> {
    match msg {
        RksMessage::CreatePod(pod_task) => {
            create::user_create(pod_task, xline_store, conn).await?;
        }
        RksMessage::DeletePod(pod_name) => {
            delete::user_delete(pod_name, xline_store, conn).await?;
        }
        RksMessage::GetNodeCount => {
            info!("[user dispatch] GetNodeCount received");
        }
        _ => warn!("[user dispatch] unknown message"),
    }
    Ok(())
}

/// Set up the QUIC server endpoint with TLS certificate.
async fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<Endpoint> {
    let server_config = configure_server()?;
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

/// Generate a self-signed TLS certificate and configure QUIC server.
fn configure_server() -> anyhow::Result<ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = CertificateDer::from(cert.serialize_der()?);
    let key = PrivatePkcs8KeyDer::from(cert.serialize_private_key_der());
    let certs = vec![cert_der];
    let server_config =
        ServerConfig::with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;
    Ok(server_config)
}

/// Calculate routes for a node from all current leases.
fn calculate_routes_for_node(node_id: &str, leases: &[Lease]) -> Vec<Route> {
    let mut routes = Vec::new();
    for lease in leases {
        if lease.attrs.node_id == node_id {
            continue;
        }
        if let Some(route) = route::get_route_from_lease(lease) {
            routes.push(route);
        }
        if let Some(route_v6) = route::get_v6_route_from_lease(lease) {
            routes.push(route_v6);
        }
    }
    routes
}

/// Build node network configuration environment variables.
pub fn build_node_network_config(
    node_id: String,
    config: &network::config::Config,
    ip_masq: bool,
    mut sn4: Option<Ipv4Network>,
    mut sn6: Option<Ipv6Network>,
) -> Result<NodeNetworkConfig> {
    let mut contents = String::new();

    if config.enable_ipv4
        && let Some(ref mut net) = sn4
    {
        contents += &format!(
            "RKL_NETWORK={}\n",
            config.network.context("IPv4 network config missing")?
        );
        contents += &format!("RKL_SUBNET={}/{}\n", net.ip(), net.prefix());
    }

    if config.enable_ipv6
        && let Some(ref mut net) = sn6
    {
        contents += &format!(
            "RKL_IPV6_NETWORK={}\n",
            config.ipv6_network.context("IPv6 network config missing")?
        );
        contents += &format!("RKL_IPV6_SUBNET={}/{}\n", net.ip(), net.prefix());
    }

    contents += &format!("RKL_IPMASQ={ip_masq}\n");

    Ok(NodeNetworkConfig {
        node_id,
        subnet_env: contents,
    })
}

