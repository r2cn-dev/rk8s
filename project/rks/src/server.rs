use crate::api::xlinestore::XlineStore;
use crate::commands::{create, delete};
use crate::network::{self, backend::route, lease::LeaseWatchResult, manager::LocalManager};
use anyhow::{Context, Result};
use common::{
    NodeNetworkConfig, PodTask, RksMessage,
    lease::{Lease, LeaseAttrs},
};
use ipnetwork::{Ipv4Network, Ipv6Network};
use libcni::ip::route::Route;
use quinn::{Connection, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, broadcast, mpsc};

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
#[allow(dead_code)]
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

/// launche the RKS server to listen for incoming QUIC connections.
/// spawn a new task for each accepted connection (either worker or user).
pub async fn serve(
    addr: String,
    xline_store: Arc<XlineStore>,
    local_manager: Arc<LocalManager>,
) -> anyhow::Result<()> {
    // Create QUIC endpoint and server certificate
    let endpoint = make_server_endpoint(addr.parse()?).await?;

    let node_registry = Arc::new(NodeRegistry::default());

    // set up a broadcast channel for distributing pod events
    let (tx, _rx) = broadcast::channel::<RksMessage>(100);

    let (lease_tx, mut lease_rx) = mpsc::channel::<Vec<LeaseWatchResult>>(16);
    let local_manager_clone = local_manager.clone();
    tokio::spawn(async move {
        local_manager_clone.watch_leases(lease_tx).await.unwrap();
    });

    let node_registry_clone = node_registry.clone();
    tokio::spawn(async move {
        while let Some(results) = lease_rx.recv().await {
            let leases = results
                .iter()
                .flat_map(|r| r.snapshot.clone())
                .collect::<Vec<_>>();
            println!("[server] get the all leases :{leases:?}");
            let node_ids: Vec<String> = leases.iter().map(|l| l.attrs.node_id.clone()).collect();
            for node_id in node_ids {
                let routes = calculate_routes_for_node(&node_id, &leases);
                println!("[server] send for {node_id}, the routes:{routes:?}");
                let msg = RksMessage::UpdateRoutes(node_id.clone(), routes);
                if let Some(worker) = node_registry_clone.get(&node_id).await {
                    if let Err(e) = worker.tx.try_send(msg) {
                        eprintln!("Failed to enqueue message for {node_id}: {e:?}");
                    }
                } else {
                    eprintln!("No active worker for {node_id}");
                }
            }
        }
    });

    loop {
        let connecting = endpoint.accept().await;
        let tx = tx.clone();
        let xline_store = xline_store.clone();
        let local_manager = local_manager.clone();
        let node_registry = node_registry.clone();

        match connecting {
            Some(connecting) => {
                match connecting.await {
                    Ok(conn) => {
                        let remote_addr = conn.remote_address().to_string();
                        println!("[server] connection accepted: addr={remote_addr}");

                        // spawn new task to handle this connection
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                conn,
                                xline_store,
                                tx,
                                local_manager,
                                node_registry,
                            )
                            .await
                            {
                                eprintln!("[server] handle_connection error: {e:?}");
                            }
                        });
                    }
                    Err(e) => eprintln!("[server] failed to establish connection: {e:?}"),
                }
            }
            None => break,
        }
    }
    Ok(())
}

/// watches the pod state and pushes create or delete events to the target worker node.
/// The node_id ensures events are filtered per node.
async fn watch_pods(
    xline_store: &Arc<XlineStore>,
    rx: &mut broadcast::Receiver<RksMessage>,
    conn: &Connection,
    node_id: Option<String>,
) -> Result<()> {
    let node_id = match node_id {
        Some(id) => id,
        None => {
            eprintln!("[watch_pods] no node_id provided, skipping message dispatch");
            return Ok(());
        }
    };

    // send all existing pods assigned to this node
    // used for re-connection
    let pods = xline_store.list_pods().await?;
    for pod_name in pods {
        if let Ok(Some(pod_yaml)) = xline_store.get_pod_yaml(&pod_name).await {
            let pod_task: PodTask = serde_yaml::from_str(&pod_yaml)
                .map_err(|e| anyhow::anyhow!("Failed to parse pod_yaml: {}", e))?;
            if pod_task.spec.node_name.as_deref() == Some(&node_id) {
                let msg = RksMessage::CreatePod(Box::new(pod_task.clone()));
                let data = bincode::serialize(&msg)?;
                if let Ok(mut stream) = conn.open_uni().await {
                    stream.write_all(&data).await?;
                    stream.finish()?;
                    println!(
                        "[watch_pods] send CreatePod for pod: {} to node: {}",
                        pod_task.metadata.name, node_id
                    );
                }
            }
        }
    }

    // continue watching for new broadcasted pod messages
    // every worker has a receiver
    // the logic below decides whether rksmessage should be dispatched to worker
    while let Ok(msg) = rx.recv().await {
        match msg {
            RksMessage::CreatePod(pod_task) => {
                create::watch_create(&pod_task, conn, &node_id).await?;
            }
            RksMessage::DeletePod(pod_name) => {
                delete::watch_delete(pod_name, conn, xline_store, &node_id)
                    .await
                    .unwrap();
            }
            _ => {}
        }
    }

    Ok(())
}

/// handle an individual connection (worker or user) by:
/// reading initial message to identify the client type,
/// spawning `watch_pods` if it's a worker,
/// dispatching further requests based on stream messages.
async fn handle_connection(
    conn: Connection,
    xline_store: Arc<XlineStore>,
    tx: broadcast::Sender<RksMessage>,
    local_manager: Arc<LocalManager>,
    node_registry: Arc<NodeRegistry>,
) -> Result<()> {
    let mut buf = vec![0u8; 4096];
    let mut is_worker = false;
    let mut node_id = None;
    let node_registry_clone = node_registry.clone();
    // initial handshake to classify connection (RegisterNode or UserRequest)
    if let Ok(mut recv) = conn.accept_uni().await {
        match recv.read(&mut buf).await {
            Ok(Some(n)) => {
                println!("[server] received raw data: {:?}", &buf[..n]);
                if let Ok(msg) = bincode::deserialize::<RksMessage>(&buf[..n]) {
                    match msg {
                        RksMessage::RegisterNode(node) => {
                            let id = node.metadata.name.clone();
                            if id.is_empty() {
                                eprintln!("[server] invalid node: metadata.name is empty");
                                return Ok(());
                            }
                            is_worker = true;
                            node_id = Some(id.clone());
                            let ip = conn.remote_address().ip().to_string();
                            let node_yaml = serde_yaml::to_string(&*node)?;
                            xline_store.insert_node_yaml(&id, &node_yaml).await?;
                            println!("[server] registered worker node: {id}, ip: {ip}");

                            let config = local_manager.get_network_config().await?;
                            println!("[server] get the network config : {config:?}");

                            let (public_ip, public_ipv6) = match conn.remote_address().ip() {
                                IpAddr::V4(v4) => (v4, None),
                                IpAddr::V6(v6) => (Ipv4Addr::new(0, 0, 0, 0), Some(v6)),
                            };
                            let lease_attrs = LeaseAttrs {
                                public_ip,
                                public_ipv6,
                                backend_type: "hostgw".to_string(),
                                node_id: id.clone(),
                                ..Default::default()
                            };
                            let lease = local_manager.acquire_lease(&lease_attrs).await?;
                            println!("[server] racquire worker node lease : {lease:?}");
                            let subnet = lease.subnet;
                            let ipv6_subnet = lease.ipv6_subnet;

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

                            let response = RksMessage::Ack;
                            let data = bincode::serialize(&response)?;
                            if let Ok(mut stream) = conn.open_uni().await {
                                stream.write_all(&data).await?;
                                stream.finish()?;
                            }

                            let node_net_config = build_node_network_config(
                                id.clone(),
                                &config,
                                false,
                                Some(subnet),
                                ipv6_subnet,
                            )?;
                            let msg = RksMessage::SetNetwork(Box::new(node_net_config));
                            if let Some(worker) = node_registry_clone.get(&id).await {
                                if let Err(e) = worker.tx.try_send(msg) {
                                    eprintln!("Failed to enqueue message for {id}: {e:?}");
                                }
                            } else {
                                eprintln!("No active worker for {id}");
                            }
                        }
                        RksMessage::UserRequest(_) => {
                            is_worker = false;
                            println!("[server] user connection established");
                        }
                        _ => {
                            eprintln!("[server] invalid initial message, closing connection");
                            return Ok(());
                        }
                    }
                } else {
                    eprintln!("[server] deserialize failed: {:?}", &buf[..n]);
                }
            }
            Ok(None) => eprintln!("[server] stream closed"),
            Err(e) => eprintln!("[server] read error: {e:?}"),
        }
    }

    // start watching pods if this is a registered worker node
    if is_worker && node_id.is_some() {
        let xline_store_clone = xline_store.clone();
        let conn_clone = conn.clone();
        let node_id_clone = node_id.clone().unwrap();
        let node_registry_clone = node_registry.clone();
        let local_manager_clone = local_manager.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let _ = watch_pods(&xline_store_clone, &mut rx, &conn_clone, node_id).await;
        });

        tokio::spawn(async move {
            if let Some(worker_session) = node_registry_clone.get(&node_id_clone).await {
                let my_lease_clone = worker_session.lease.clone();
                let cancel_notify_clone = worker_session.cancel_notify.clone();
                if let Err(e) = local_manager_clone
                    .complete_lease(my_lease_clone, cancel_notify_clone)
                    .await
                {
                    eprintln!("[server] complete_lease error for node={node_id_clone}: {e:?}");
                }
            } else {
                eprintln!("[server] no active worker session for node={node_id_clone}");
            }
        });
    }

    // Main loop: accept uni-directional streams for ongoing communication
    loop {
        match conn.accept_uni().await {
            Ok(mut recv_stream) => {
                println!("[server] stream accepted: {}", recv_stream.id());
                let xline_store = xline_store.clone();
                let tx = tx.clone();

                let mut buf = vec![0u8; 4096];
                match recv_stream.read(&mut buf).await {
                    Ok(Some(n)) => {
                        if let Ok(msg) = bincode::deserialize::<RksMessage>(&buf[..n]) {
                            if is_worker {
                                let _ = dispatch_worker(msg.clone(), &conn).await;
                            } else {
                                let _ = dispatch_user(msg.clone(), &xline_store, &conn, &tx).await;
                            }
                        }
                    }
                    Ok(None) => println!("[server] stream closed"),
                    Err(e) => println!("[server] read error: {e}"),
                }
            }
            Err(e) => {
                println!("[server] connection error: {e}");
                break;
            }
        }
    }

    Ok(())
}

/// acknowledges response from worker node
async fn dispatch_worker(msg: RksMessage, conn: &Connection) -> Result<()> {
    match msg {
        RksMessage::Heartbeat(node_id) => {
            println!("[worker dispatch] received heartbeat from node: {node_id}");
            let response = RksMessage::Ack;
            let data = bincode::serialize(&response)?;
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&data).await?;
                stream.finish()?;
            }
        }
        RksMessage::Error(err_msg) => {
            println!("[worker dispatch] reported error: {err_msg}");
        }
        RksMessage::Ack => {
            println!("[worker dispatch] received Ack");
        }
        _ => {
            println!("[worker dispatch] unknown or unexpected message from worker");
        }
    }
    Ok(())
}

/// handle user-side commands like creating or deleting pods,
/// or querying cluster info like node count.
pub async fn dispatch_user(
    msg: RksMessage,
    xline_store: &Arc<XlineStore>,
    conn: &Connection,
    tx: &broadcast::Sender<RksMessage>,
) -> Result<()> {
    match msg {
        RksMessage::CreatePod(pod_task) => {
            create::user_create(pod_task, xline_store, conn, tx).await?;
        }

        RksMessage::DeletePod(pod_name) => {
            delete::user_delete(pod_name, xline_store, conn, tx).await?;
        }

        RksMessage::ListPod => {
            let pods = xline_store.list_pods().await?;
            println!("[user dispatch] list current pod: {pods:?}");
            let res = bincode::serialize(&RksMessage::ListPodRes(pods))?;
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&res).await?;
                stream.finish()?;
            }
        }

        RksMessage::GetNodeCount => {
            let count = xline_store.list_nodes().await?.len();
            println!("[user dispatch] node count: {count}");
            let response = RksMessage::NodeCount(count);
            let data = bincode::serialize(&response)?;
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&data).await?;
                stream.finish()?;
            }
        }

        _ => {
            println!("[user dispatch] unknown message");
        }
    }

    Ok(())
}

/// set up the QUIC server endpoint with TLS certificate.
async fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<Endpoint> {
    let server_config = configure_server()?;
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

/// generates a self-signed TLS certificate and constructs QUIC server config.
fn configure_server() -> anyhow::Result<ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = CertificateDer::from(cert.serialize_der()?);
    let key = PrivatePkcs8KeyDer::from(cert.serialize_private_key_der());
    let certs = vec![cert_der];
    let server_config =
        ServerConfig::with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;
    Ok(server_config)
}

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
