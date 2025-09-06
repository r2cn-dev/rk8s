#![allow(dead_code)]
use anyhow::Result;
use common::Node;
use etcd_client::{Client, GetOptions, PutOptions};
use serde_yaml;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::protocol::config::NetworkConfig;
/// like etcd, k:/registry/pods/pod_name v:yaml file of pod
/// k:/registry/nodes/node_name v:yaml file of node
#[derive(Clone)]
pub struct XlineStore {
    client: Arc<RwLock<Client>>,
}

impl XlineStore {
    pub async fn new(endpoints: &[&str]) -> Result<Self> {
        let client = Client::connect(endpoints, None).await?;
        Ok(Self {
            client: Arc::new(RwLock::new(client)),
        })
    }

    pub async fn insert_pod_yaml(&self, pod_name: &str, pod_yaml: &str) -> Result<()> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        client.put(key, pod_yaml, None).await?;
        Ok(())
    }

    pub async fn get_pod_yaml(&self, pod_name: &str) -> Result<Option<String>> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        let resp = client.get(key, None).await?;
        Ok(resp
            .kvs()
            .first()
            .map(|kv| String::from_utf8_lossy(kv.value()).to_string()))
    }

    pub async fn list_pods(&self) -> Result<Vec<String>> {
        let mut client = self.client.write().await;
        let resp = client
            .get("/registry/pods/", Some(GetOptions::new().with_prefix()))
            .await?;
        Ok(resp
            .kvs()
            .iter()
            .map(|kv| String::from_utf8_lossy(kv.key()).replace("/registry/pods/", ""))
            .collect())
    }

    pub async fn insert_node_yaml(&self, node_name: &str, node_yaml: &str) -> Result<()> {
        let key = format!("/registry/nodes/{node_name}");
        let mut client = self.client.write().await;
        client.put(key, node_yaml, Some(PutOptions::new())).await?;
        Ok(())
    }

    // pub async fn get_node_yaml(&self, node_name: &str) -> Result<Option<String>> {
    //     let key = format!("/registry/nodes/{node_name}");
    //     let mut client = self.client.write().await;
    //     let resp = client.get(key, None).await?;
    //     Ok(resp.kvs().first().map(|kv| String::from_utf8_lossy(kv.value()).to_string()))
    // }

    // pub async fn get_node(&self, node_name: &str) -> Result<Option<Node>> {
    //     if let Some(yaml) = self.get_node_yaml(node_name).await? {
    //         let node: Node = serde_yaml::from_str(&yaml)?;
    //         Ok(Some(node))
    //     } else {
    //         Ok(None)
    //     }
    // }

    pub async fn list_nodes(&self) -> Result<Vec<(String, Node)>> {
        let mut client = self.client.write().await;
        let resp = client
            .get("/registry/nodes/", Some(GetOptions::new().with_prefix()))
            .await?;
        let mut result = Vec::new();
        for kv in resp.kvs() {
            let name = String::from_utf8_lossy(kv.key()).replace("/registry/nodes/", "");
            let node: Node = serde_yaml::from_slice(kv.value())?;
            result.push((name, node));
        }
        Ok(result)
    }

    pub async fn delete_pod(&self, pod_name: &str) -> Result<()> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        client.delete(key, None).await?;
        Ok(())
    }

    pub async fn delete_node(&self, node_name: &str) -> Result<()> {
        let key = format!("/registry/nodes/{node_name}");
        let mut client = self.client.write().await;
        client.delete(key, None).await?;
        Ok(())
    }

    pub async fn insert_network_config(&self, prefix: &str, config: &NetworkConfig) -> Result<()> {
        let key = format!("{}/config", prefix.trim_end_matches('/'));

        let value = serde_json::to_string(config)?;

        let mut client = self.client.write().await;
        client.put(key, value, Some(PutOptions::new())).await?;
        Ok(())
    }

    pub async fn get_network_config(&self, prefix: &str) -> Result<Option<NetworkConfig>> {
        let key = format!("{}/config", prefix.trim_end_matches('/'));
        let mut client = self.client.write().await;
        let resp = client.get(key, None).await?;
        if let Some(kv) = resp.kvs().first() {
            let cfg: NetworkConfig = serde_json::from_slice(kv.value())?;
            Ok(Some(cfg))
        } else {
            Ok(None)
        }
    }
}
