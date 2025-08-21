use anyhow::Result;
use bincode;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig as QuinnClientConfig, Endpoint};
use std::{env, fs, net::SocketAddr, sync::Arc, time::Duration};
use tokio::time;

use crate::commands::pod;
use crate::task::{Node, PodTask, RksMessage, TaskRunner};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore, SignatureScheme};
use serde_yaml;

///Skip certificate verification
#[derive(Debug)]
struct SkipServerVerification;

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

fn load_node_from_yaml(path: &str) -> Result<Node> {
    let s = fs::read_to_string(path)?;
    let node: Node = serde_yaml::from_str(&s)?;
    Ok(node)
}

/// Run worker loop based on environment variables.
/// This function will keep reconnecting if errors occur.
pub async fn run_forever() -> Result<()> {
    //We should give the ipaddr of rks here
    let server_addr: String =
        env::var("RKS_ADDR").unwrap_or_else(|_| "192.168.73.128:50051".to_string());
    //now we just use a yaml to record the node information
    //in fact , this should be generate by rkl
    let node_yaml: String = env::var("NODE_YAML")
        .unwrap_or_else(|_| "/home/ich/rk8s/project/rkl/src/daemon/test.yaml".to_string());

    let server_addr: SocketAddr = server_addr.parse()?;
    let node: Node = load_node_from_yaml(&node_yaml)?;

    loop {
        if let Err(e) = run_once(server_addr, node.clone()).await {
            eprintln!("[rkl_worker] error: {e:?}, retrying in 3s");
            time::sleep(Duration::from_secs(3)).await;
        } else {
            time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Single connection lifecycle:
/// 1. Establish QUIC connection
/// 2. Register node
/// 3. Start heartbeat loop
/// 4. Handle CreatePod/DeletePod messages
pub async fn run_once(server_addr: SocketAddr, node: Node) -> Result<()> {
    // Skip certificate verification
    let mut tls = RustlsClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    tls.dangerous()
        .set_certificate_verifier(Arc::new(SkipServerVerification));

    let quic_crypto = QuicClientConfig::try_from(tls)?;
    let client_cfg = QuinnClientConfig::new(Arc::new(quic_crypto));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_cfg);

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
    if let Ok(Ok(mut recv)) = time::timeout(Duration::from_secs(3), connection.accept_uni()).await {
        let mut buf = vec![0u8; 4096];
        if let Ok(Some(n)) = recv.read(&mut buf).await {
            if let Ok(resp) = bincode::deserialize::<RksMessage>(&buf[..n]) {
                match resp {
                    RksMessage::Ack => println!("[worker] got register Ack"),
                    RksMessage::Error(e) => eprintln!("[worker] register error: {e}"),
                    other => println!("[worker] unexpected register response: {other:?}"),
                }
            } else {
                eprintln!("[worker] failed to parse register response");
            }
        }
    }

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

    //Main receive loop: handle CreatePod/DeletePod...
    loop {
        match connection.accept_uni().await {
            Ok(mut recv) => {
                let mut buf = vec![0u8; 4096];
                match recv.read(&mut buf).await {
                    Ok(Some(n)) => match bincode::deserialize::<RksMessage>(&buf[..n]) {
                        Ok(RksMessage::CreatePod(pod_box)) => {
                            let pod: PodTask = (*pod_box).clone();

                            // validate target node
                            let target_opt = pod.spec.nodename.as_deref();
                            if let Some(target) = target_opt {
                                if target != node.metadata.name {
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
                            match pod::delete_pod(&name) {
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

/// send a message over a unidirectional stream
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
