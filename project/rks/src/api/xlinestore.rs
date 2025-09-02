use anyhow::Result;
use etcd_client::{Client, GetOptions, PutOptions, WatchOptions, WatchStream, Watcher};
use std::sync::Arc;
use tokio::sync::RwLock;

/// XlineStore provides an etcd-like API for managing pods and nodes.
/// Keys are stored under `/registry/pods/` and `/registry/nodes/`.
/// Values are YAML serialized definitions.
#[derive(Clone)]
pub struct XlineStore {
    client: Arc<RwLock<Client>>,
}

impl XlineStore {
    /// Create a new XlineStore instance by connecting to the given endpoints.
    pub async fn new(endpoints: &[&str]) -> Result<Self> {
        let client = Client::connect(endpoints, None).await?;
        Ok(Self {
            client: Arc::new(RwLock::new(client)),
        })
    }

    /// Get a read-only reference to the internal etcd client.
    /// This is typically used for watch operations.
    pub async fn client(&self) -> tokio::sync::RwLockReadGuard<'_, Client> {
        self.client.read().await
    }

    /// List all pod names (keys only, values are ignored).
    pub async fn list_pods(&self) -> Result<Vec<String>> {
        let key = "/registry/pods/".to_string();
        let mut client = self.client.write().await;
        let resp = client
            .get(
                key.clone(),
                Some(GetOptions::new().with_prefix().with_keys_only()),
            )
            .await?;
        Ok(resp
            .kvs()
            .iter()
            .map(|kv| String::from_utf8_lossy(kv.key()).replace("/registry/pods/", ""))
            .collect())
    }

    /// List all node names (keys only, values are ignored).
    pub async fn list_nodes(&self) -> Result<Vec<String>> {
        let key = "/registry/nodes/".to_string();
        let mut client = self.client.write().await;
        let resp = client
            .get(
                key.clone(),
                Some(GetOptions::new().with_prefix().with_keys_only()),
            )
            .await?;
        Ok(resp
            .kvs()
            .iter()
            .map(|kv| String::from_utf8_lossy(kv.key()).replace("/registry/nodes/", ""))
            .collect())
    }

    /// Insert a node YAML definition into xline.
    pub async fn insert_node_yaml(&self, node_name: &str, node_yaml: &str) -> Result<()> {
        let key = format!("/registry/nodes/{node_name}");
        let mut client = self.client.write().await;
        client.put(key, node_yaml, Some(PutOptions::new())).await?;
        Ok(())
    }

    // Example (currently unused):
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

    /// Insert a pod YAML definition into xline.
    pub async fn insert_pod_yaml(&self, pod_name: &str, pod_yaml: &str) -> Result<()> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        client.put(key, pod_yaml, Some(PutOptions::new())).await?;
        Ok(())
    }

    /// Get a pod YAML definition from xline.
    pub async fn get_pod_yaml(&self, pod_name: &str) -> Result<Option<String>> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        let resp = client.get(key, None).await?;
        if let Some(kv) = resp.kvs().first() {
            Ok(Some(String::from_utf8_lossy(kv.value()).to_string()))
        } else {
            Ok(None)
        }
    }

    /// Delete a pod from xline.
    pub async fn delete_pod(&self, pod_name: &str) -> Result<()> {
        let key = format!("/registry/pods/{pod_name}");
        let mut client = self.client.write().await;
        client.delete(key, None).await?;
        Ok(())
    }

    /// Take a snapshot of all pods and return them with the current revision.
    pub async fn pods_snapshot_with_rev(&self) -> Result<(Vec<(String, String)>, i64)> {
        let key_prefix = "/registry/pods/".to_string();
        let mut client = self.client.write().await;
        let resp = client
            .get(key_prefix.clone(), Some(GetOptions::new().with_prefix()))
            .await?;
        let rev = resp.header().map(|h| h.revision()).unwrap_or(0);
        let items: Vec<(String, String)> = resp
            .kvs()
            .iter()
            .map(|kv| {
                (
                    String::from_utf8_lossy(kv.key()).replace("/registry/pods/", ""),
                    String::from_utf8_lossy(kv.value()).to_string(),
                )
            })
            .collect();
        Ok((items, rev))
    }

    /// Create a watch on all pods with prefix `/registry/pods/`, starting from a given revision.
    pub async fn watch_pods(&self, start_rev: i64) -> Result<(Watcher, WatchStream)> {
        let key_prefix = "/registry/pods/".to_string();
        let opts = WatchOptions::new()
            .with_prefix()
            .with_prev_key()
            .with_start_revision(start_rev);
        let mut client = self.client.write().await;
        let (watcher, stream) = client.watch(key_prefix, Some(opts)).await?;
        Ok((watcher, stream))
    }

    /// Initialize Flannel CNI network configuration.
    pub async fn init_flannel_config(&self) -> Result<()> {
        let config_json = r#"{
            "Network": "10.244.0.0/16",
            "SubnetLen": 24,
            "Backend": {
                "Type": "vxlan",
                "VNI": 1
            }
        }"#;

        let key = "/coreos.com/network/config";
        let mut client = self.client.write().await;
        client.put(key, config_json, Some(PutOptions::new())).await?;
        Ok(())
    }

    // Example (currently unused):
    // pub async fn delete_node(&self, node_name: &str) -> Result<()> {
    //     let key = format!("/registry/nodes/{}", node_name);
    //     let mut client = self.client.write().await;
    //     client.delete(key, None).await?;
    //     Ok(())
    // }
}