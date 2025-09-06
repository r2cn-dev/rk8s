use anyhow::Result;
use bincode;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig as QuinnClientConfig, Endpoint};
use std::{env, fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::time;

use crate::commands::pod;
use crate::network::ip::{IPStack, PublicIPOpts, lookup_ext_iface};
use crate::network::{
    config::{NetworkConfig, validate_network_config},
    receiver::{NetworkConfigMessage, NetworkReceiver},
};
use crate::task::TaskRunner;
use chrono::Utc;
use common::*;
use gethostname::gethostname;
use ipnetwork::Ipv4Network;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore, SignatureScheme};
use std::collections::HashMap;

use sysinfo::System;

fn get_subnet_file_path() -> String {
    if let Ok(path) = env::var("SUBNET_FILE_PATH") {
        println!("Using custom subnet file path: {path}");
        return path;
    }

    let cni_path = "/etc/cni/net.d/subnet.env";
    if Path::new("/etc/cni/net.d").exists()
        && fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(cni_path)
            .is_ok()
    {
        println!("Using CNI standard path: {cni_path}");
        return cni_path.to_string();
    }

    if let Ok(home) = env::var("HOME") {
        let user_dir = format!("{home}/.rkl");
        let user_path = format!("{user_dir}/subnet.env");

        if fs::create_dir_all(&user_dir).is_ok() {
            println!("Using user directory: {user_path}");
            return user_path;
        }
    }

    let default_path = "/tmp/subnet.env";
    println!("Using default temporary path: {default_path}");
    default_path.to_string()
}

/// Skip certificate verification
#[derive(Debug)]
pub struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}

/// Run worker loop based on environment variables.
/// This function will keep reconnecting if errors occur.
pub async fn run_forever() -> Result<()> {
    //We should give the ipaddr of rks here
    let server_addr: String =
        env::var("RKS_ADDRESS").unwrap_or_else(|_| "192.168.73.128:50051".to_string());
    let server_addr: SocketAddr = server_addr.parse()?;

    let ext_iface = lookup_ext_iface(
        None,
        None,
        None,
        IPStack::Ipv4,
        PublicIPOpts {
            public_ip: None,
            public_ipv6: None,
        },
    )
    .await?;

    let node: Node = if let Ok(node_yaml) = env::var("NODE_YAML") {
        load_node_from_yaml(&node_yaml)?
    } else {
        generate_node(&ext_iface).await?
    };

    loop {
        if let Err(e) = run_once(server_addr, node.clone()).await {
            eprintln!("[rkl_worker] error: {e:?}, retrying in 3s");
            time::sleep(Duration::from_secs(3)).await;
        } else {
            time::sleep(Duration::from_secs(1)).await;
        }
    }
}

fn load_node_from_yaml(yaml_path: &str) -> Result<Node> {
    use std::fs;
    let yaml_content = fs::read_to_string(yaml_path)?;
    let node: Node = serde_yaml::from_str(&yaml_content)?;
    Ok(node)
}

/// Single connection lifecycle:
/// 1. Establish QUIC connection
/// 2. Register node
/// 3. Start heartbeat loop
/// 4. Handle CreatePod/DeletePod messages
/// 5. Handle Network Configuration
pub async fn run_once(server_addr: SocketAddr, node: Node) -> Result<()> {
    // Skip certificate verification
    let mut tls = RustlsClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    tls.dangerous()
        .set_certificate_verifier(Arc::new(SkipServerVerification));

    let quic_crypto = QuicClientConfig::try_from(tls)?;
    let client_cfg: QuinnClientConfig = QuinnClientConfig::new(Arc::new(quic_crypto));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_cfg);

    let subnet_file_path = get_subnet_file_path();
    let link_index = env::var("LINK_INDEX")
        .unwrap_or_else(|_| "1".to_string())
        .parse::<u32>()
        .unwrap_or(1);
    let backend_type = env::var("BACKEND_TYPE").unwrap_or_else(|_| "hostgw".to_string());

    let network_receiver = NetworkReceiver::new(
        subnet_file_path,
        link_index,
        backend_type,
        node.metadata.name.clone(),
    );

    println!("Network receiver created for node: {}", node.metadata.name);

    // establish connection with retry
    let connection = loop {
        match endpoint.connect(server_addr, "localhost") {
            Ok(connecting) => match connecting.await {
                Ok(conn) => break conn,
                Err(e) => {
                    eprintln!("[worker] connect failed: {e}, retrying 2s");
                    time::sleep(Duration::from_secs(2)).await;
                }
            },
            Err(e) => {
                eprintln!("[worker] endpoint connect error: {e}, retrying 2s");
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    println!("[worker] connected to RKS at {server_addr}");

    // register to rks by sending RegisterNode(Box<Node>)
    let register_msg = RksMessage::RegisterNode(Box::new(node.clone()));
    send_uni(&connection, &register_msg).await?;
    println!("[worker] sent RegisterNode({})", node.metadata.name);

    // read ack
    // if let Ok(Ok(mut recv)) = time::timeout(Duration::from_secs(3), connection.accept_uni()).await {
    //     let mut buf = vec![0u8; 4096];
    //     if let Ok(Some(n)) = recv.read(&mut buf).await {
    //         if let Ok(resp) = bincode::deserialize::<RksMessage>(&buf[..n]) {
    //             match resp {
    //                 RksMessage::Ack => println!("[worker] got register Ack"),
    //                 RksMessage::Error(e) => eprintln!("[worker] register error: {e}"),
    //                 other => println!("[worker] unexpected register response: {other:?}"),
    //             }
    //         } else {
    //             eprintln!("[worker] failed to parse register response");
    //         }
    //     }
    // }

    // heartbeat
    let hb_conn = connection.clone();
    let node_name = node.metadata.name.clone();
    tokio::spawn(async move {
        loop {
            time::sleep(Duration::from_secs(5)).await;
            let hb = RksMessage::Heartbeat(node_name.clone());
            if let Err(e) = send_uni(&hb_conn, &hb).await {
                eprintln!("[worker heartbeat] send failed: {e}");
            } else {
                println!("[worker] heartbeat sent");
            }
        }
    });

    //Main receive loop: handle CreatePod/DeletePod/Network...
    loop {
        match connection.accept_uni().await {
            Ok(mut recv) => {
                let mut buf = vec![0u8; 4096];
                match recv.read(&mut buf).await {
                    Ok(Some(n)) => match bincode::deserialize::<RksMessage>(&buf[..n]) {
                        Ok(RksMessage::Ack) => {
                            println!("[worker] got register Ack");
                        }
                        Ok(RksMessage::Error(e)) => {
                            eprintln!("[worker] register error: {e}");
                        }
                        Ok(RksMessage::SetNetwork(cfg)) => {
                            println!("[worker] received network config: {cfg:?}");

                            if let Err(e) = handle_network_config(&network_receiver, &cfg).await {
                                eprintln!("[worker] failed to apply network config: {e}");
                                let _ = send_uni(
                                    &connection,
                                    &RksMessage::Error(format!("network config failed: {e}")),
                                )
                                .await;
                            } else {
                                println!("[worker] network config applied successfully");
                                let _ = send_uni(&connection, &RksMessage::Ack).await;
                            }
                        }
                        Ok(RksMessage::UpdateRoutes(_id, routes)) => {
                            println!("[worker] received routes update: {routes:?}");
                            let route_msg = NetworkConfigMessage::Route { routes };
                            if let Err(e) = network_receiver.handle_network_config(route_msg).await
                            {
                                eprintln!("[worker] failed to apply routes: {e}");
                                let _ = send_uni(
                                    &connection,
                                    &RksMessage::Error(format!("routes update failed: {e}")),
                                )
                                .await;
                            } else {
                                println!("[worker] routes applied successfully");
                                let _ = send_uni(&connection, &RksMessage::Ack).await;
                            }
                        }
                        Ok(RksMessage::CreatePod(pod_box)) => {
                            let pod: PodTask = (*pod_box).clone();

                            // validate target node
                            let target_opt = pod.spec.node_name.as_deref();
                            if let Some(target) = target_opt
                                && target != node.metadata.name
                            {
                                eprintln!(
                                    "[worker] CreatePod skipped: target={} self={}",
                                    target, node.metadata.name
                                );
                                let _ = send_uni(
                                    &connection,
                                    &RksMessage::Error(format!(
                                        "pod {} target node mismatch: target={}, self={}",
                                        pod.metadata.name, target, node.metadata.name
                                    )),
                                )
                                .await;
                                continue;
                            }

                            println!(
                                "[worker] CreatePod name={} assigned_to={}",
                                pod.metadata.name,
                                target_opt.unwrap_or("<unspecified>")
                            );

                            // Create and run task
                            let runner = match TaskRunner::from_task(pod.clone()) {
                                Ok(r) => r,
                                Err(e) => {
                                    eprintln!("[worker] TaskRunner::from_task failed: {e:?}");
                                    let _ = send_uni(
                                        &connection,
                                        &RksMessage::Error(format!(
                                            "create {} failed: {e}",
                                            pod.metadata.name
                                        )),
                                    )
                                    .await;
                                    continue;
                                }
                            };

                            match pod::run_pod_from_taskrunner(runner) {
                                Ok(_) => {
                                    let _ = send_uni(&connection, &RksMessage::Ack).await;
                                }
                                Err(e) => {
                                    eprintln!("[worker] run_pod_from_taskrunner failed: {e:?}");
                                    let _ = send_uni(
                                        &connection,
                                        &RksMessage::Error(format!(
                                            "create {} failed: {e}",
                                            pod.metadata.name
                                        )),
                                    )
                                    .await;
                                }
                            }
                        }
                        Ok(RksMessage::DeletePod(name)) => {
                            println!("[worker] DeletePod {name}");
                            match pod::standalone::delete_pod(&name) {
                                Ok(_) => {
                                    let _ = send_uni(&connection, &RksMessage::Ack).await;
                                }
                                Err(e) => {
                                    eprintln!("[worker] delete_pod failed: {e:?}");
                                    let _ = send_uni(
                                        &connection,
                                        &RksMessage::Error(format!("delete {name} failed: {e}")),
                                    )
                                    .await;
                                }
                            }
                        }
                        Ok(other) => {
                            println!("[worker] unexpected message: {other:?}");
                        }
                        Err(err) => {
                            eprintln!("[worker] deserialize failed: {err}");
                            eprintln!("[worker] raw: {:?}", &buf[..n]);
                        }
                    },
                    Ok(None) => {
                        eprintln!("[worker] uni stream closed early");
                    }
                    Err(e) => {
                        eprintln!("[worker] read error: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[worker] accept_uni error: {e}, breaking to reconnect");
                break Ok(());
            }
        }
    }
}

async fn handle_network_config(
    network_receiver: &NetworkReceiver,
    node_cfg: &NodeNetworkConfig,
) -> Result<()> {
    println!(
        "[worker] Processing network configuration for node: {}",
        node_cfg.node_id
    );

    let mut network = None;
    let mut subnet = None;
    let mut ip_masq = true;
    let mut mtu = 1500;

    for line in node_cfg.subnet_env.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "RKL_NETWORK" => network = Some(value.parse::<Ipv4Network>()?),
                "RKL_SUBNET" => subnet = Some(value.parse::<Ipv4Network>()?),
                "RKL_MTU" => mtu = value.parse().unwrap_or(1500),
                "RKL_IPMASQ" => ip_masq = value.parse().unwrap_or(true),
                _ => {}
            }
        }
    }

    let mut cfg = NetworkConfig {
        enable_ipv4: true,
        enable_ipv6: false,
        enable_nftables: false,
        network,
        ipv6_network: None,
        subnet_min: None,
        subnet_max: None,
        ipv6_subnet_min: None,
        ipv6_subnet_max: None,
        subnet_len: 24,
        ipv6_subnet_len: 64,
        backend_type: env::var("BACKEND_TYPE").unwrap_or_else(|_| "hostgw".to_string()),
        backend: None,
    };

    validate_network_config(&mut cfg)?;

    let config_msg = NetworkConfigMessage::SubnetConfig {
        network_config: cfg,
        ip_masq,
        ipv4_subnet: subnet,
        ipv6_subnet: None,
        mtu,
    };

    network_receiver.handle_network_config(config_msg).await?;

    println!("[worker] Network configuration processed successfully");
    Ok(())
}

/// Send a message over a unidirectional stream
async fn send_uni(conn: &quinn::Connection, msg: &RksMessage) -> Result<()> {
    let mut uni = conn.open_uni().await?;
    let data = bincode::serialize(msg)?;
    uni.write_all(&data).await?;
    uni.finish()?;
    Ok(())
}

pub fn init_crypto() {
    CryptoProvider::install_default(rustls::crypto::ring::default_provider())
        .expect("failed to install default CryptoProvider");
}

pub async fn generate_node(ext_iface: &ExternalInterface) -> Result<Node> {
    let mut sys = System::new_all();
    sys.refresh_all();

    // hostname
    let hostname = gethostname().to_string_lossy().into_owned();

    // IP addr
    let mut addresses = vec![];

    if let Some(ip) = ext_iface.iface_addr {
        addresses.push(NodeAddress {
            address_type: "InternalIP".to_string(),
            address: ip.to_string(),
        });
    }
    addresses.push(NodeAddress {
        address_type: "Hostname".to_string(),
        address: hostname.clone(),
    });

    // CPU / memory
    let total_cpu = sys.cpus().len().to_string();
    let total_mem = format!("{}Mi", sys.total_memory() / 1024);

    let mut capacity = HashMap::new();
    capacity.insert("cpu".to_string(), total_cpu.clone());
    capacity.insert("memory".to_string(), total_mem.clone());
    capacity.insert("pods".to_string(), "110".to_string());

    let mut allocatable = capacity.clone();
    allocatable.insert("cpu".to_string(), (sys.cpus().len() - 1).to_string());

    // conditions
    let now = Utc::now().to_rfc3339();
    let conditions = vec![
        NodeCondition {
            condition_type: "Ready".to_string(),
            status: "True".to_string(),
            last_heartbeat_time: Some(now.clone()),
        },
        NodeCondition {
            condition_type: "MemoryPressure".to_string(),
            status: "False".to_string(),
            last_heartbeat_time: Some(now),
        },
    ];

    Ok(Node {
        api_version: "v1".to_string(),
        kind: "Node".to_string(),
        metadata: ObjectMeta {
            name: hostname,
            namespace: "default".to_string(),
            labels: HashMap::new(),
            annotations: HashMap::new(),
        },
        spec: NodeSpec {
            pod_cidr: "10.244.1.0/24".to_string(),
        },
        status: NodeStatus {
            capacity,
            allocatable,
            addresses,
            conditions,
        },
    })
}
