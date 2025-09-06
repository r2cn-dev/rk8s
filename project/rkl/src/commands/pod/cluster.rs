use anyhow::Ok;
use anyhow::Result;
use anyhow::anyhow;
use common::PodTask;
use common::RksMessage;
use quinn::ClientConfig as QuinnClientConfig;
use quinn::Connection;
use quinn::Endpoint;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::crypto::CryptoProvider;
use std::fs::File;
use std::io;
use std::io::Write;
use std::{sync::Arc, time::Duration};
use tabwriter::TabWriter;
use tokio::time;
use tracing::debug;

/// Skip certificate verification
use crate::daemon::client::SkipServerVerification;
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};

#[allow(dead_code)]
const DEFAULT_RKS_ADDR: &str = "127.0.0.1:50051";

pub struct UserQUICClient {
    conn: Connection,
}

impl UserQUICClient {
    pub async fn from<S: AsRef<str>>(server_addr: S) -> Result<Self> {
        // Skip certificate verification
        let server_addr = server_addr.as_ref();

        CryptoProvider::install_default(rustls::crypto::ring::default_provider())
            .expect("failed to install default CryptoProvider");

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
        let conn = loop {
            match endpoint.connect(server_addr.parse()?, "localhost") {
                core::result::Result::Ok(connecting) => match connecting.await {
                    core::result::Result::Ok(conn) => break conn,
                    Err(e) => {
                        eprintln!("[user] connect failed: {e}, retrying 2s");
                        time::sleep(Duration::from_secs(2)).await;
                    }
                },
                Err(e) => {
                    eprintln!("[user] endpoint connect error: {e}, retrying 2s");
                    time::sleep(Duration::from_secs(2)).await;
                }
            }
        };
        let cli = UserQUICClient { conn };
        cli.send_uni(&RksMessage::UserRequest("Hello".to_string()))
            .await?;
        println!("RKL connected to RKS at {server_addr}");
        Ok(cli)
    }

    pub async fn wait_response(&self) -> Result<RksMessage> {
        match self.conn.accept_uni().await {
            core::result::Result::Ok(mut recv_stream) => {
                let mut buf = vec![0u8; 4096];
                match recv_stream.read(&mut buf).await {
                    core::result::Result::Ok(Some(n)) => {
                        if let core::result::Result::Ok(msg) =
                            bincode::deserialize::<RksMessage>(&buf[..n])
                        {
                            debug!("Get From Server: {:?}", msg);
                            return Ok(msg);
                        }
                        Err(anyhow!("Empty response"))
                    }

                    core::result::Result::Ok(None) => Err(anyhow!("Empty response")),
                    Err(e) => Err(anyhow!("read response error: {}", e)),
                }
            }
            Err(e) => Err(anyhow!("connection error: {e}")),
        }
    }

    pub async fn send_uni(&self, msg: &RksMessage) -> Result<()> {
        let mut uni = self.conn.open_uni().await?;
        let data = bincode::serialize(msg)?;
        uni.write_all(&data).await?;
        uni.finish()?;
        Ok(())
    }
}

pub async fn delete_pod(pod_name: &str, addr: &str) -> Result<()> {
    let cli = UserQUICClient::from(addr).await?;
    cli.send_uni(&RksMessage::DeletePod(pod_name.to_string()))
        .await?;
    let _ = cli.wait_response().await?;
    println!("pod {pod_name} deleted");
    Ok(())
}

pub async fn create_pod(pod_yaml: &str, addr: &str) -> Result<()> {
    let cli = UserQUICClient::from(addr).await.unwrap();
    let task = pod_task_from_path(pod_yaml).map_err(|e| anyhow!("invalid pod yaml: {}", e))?;
    let pod_name = task.metadata.name.clone();
    cli.send_uni(&RksMessage::CreatePod(task)).await?;
    let _ = cli.wait_response().await?;
    println!("pod {pod_name} created");
    Ok(())
}

pub async fn list_pod(addr: &str) -> Result<()> {
    let cli = UserQUICClient::from(addr).await?;
    cli.send_uni(&RksMessage::ListPod).await?;

    match cli.wait_response().await? {
        RksMessage::ListPodRes(res) => list_print(res),
        msg => Err(anyhow!("unexpected response {:?} ", msg)),
    }
}

pub fn pod_task_from_path(pod_yaml: &str) -> Result<Box<PodTask>> {
    let pod_file = File::open(pod_yaml)?;
    let task: PodTask = serde_yaml::from_reader(pod_file)?;
    Ok(Box::new(task))
}

fn list_print(pod_list: Vec<String>) -> Result<()> {
    let mut tab_writer = TabWriter::new(io::stdout());
    writeln!(&mut tab_writer, "NAME\tREADY\tSTATUS\tRESTARTS\tAGE")?;
    for pod in pod_list {
        let _ = writeln!(&mut tab_writer, "{pod}");
    }
    tab_writer.flush()?;
    Ok(())
}
