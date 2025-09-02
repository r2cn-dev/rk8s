use crate::api::xlinestore::XlineStore;
use anyhow::Result;
use common::{PodTask, RksMessage};
use quinn::Connection;
use std::sync::Arc;

/// Send a pod creation message to a specific worker node
pub async fn watch_create(pod_task: &PodTask, conn: &Connection, node_id: &str) -> Result<()> {
    if pod_task.spec.nodename.as_deref() == Some(node_id) {
        let msg = RksMessage::CreatePod(Box::new(pod_task.clone()));
        let data = bincode::serialize(&msg)?;
        if let Ok(mut stream) = conn.open_uni().await {
            stream.write_all(&data).await?;
            stream.finish()?;
            println!(
                "[watch_create] sent CreatePod for pod: {} to node: {}",
                pod_task.metadata.name, node_id
            );
        }
    }
    Ok(())
}

/// Handle user-requested pod creation, store pod in Xline
pub async fn user_create(
    mut pod_task: Box<PodTask>,
    xline_store: &Arc<XlineStore>,
    conn: &Connection,
) -> Result<()> {
    // Ensure pod has a node assigned
    let node_name = match &pod_task.spec.nodename {
        Some(name) => name.clone(),
        None => {
            let response = RksMessage::Error("Pod does not have a node assigned".to_string());
            let data = bincode::serialize(&response)?;
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&data).await?;
                stream.finish()?;
            }
            return Ok(());
        }
    };

    // Serialize pod to YAML
    let pod_yaml = match serde_yaml::to_string(&pod_task) {
        Ok(yaml) => yaml,
        Err(e) => {
            eprintln!("[user_create] Failed to serialize pod task: {e}");
            let response = RksMessage::Error(format!("Serialization error: {e}"));
            let data = bincode::serialize(&response).unwrap_or_else(|_| vec![]);
            if let Ok(mut stream) = conn.open_uni().await {
                stream.write_all(&data).await?;
                stream.finish()?;
            }
            return Ok(());
        }
    };

    // Insert into Xline
    xline_store
        .insert_pod_yaml(&pod_task.metadata.name, &pod_yaml)
        .await?;

    println!(
        "[user_create] created pod {} on node {} (written to Xline)",
        pod_task.metadata.name, node_name
    );

    // Send ACK to user
    let response = RksMessage::Ack;
    let data = bincode::serialize(&response)?;
    if let Ok(mut stream) = conn.open_uni().await {
        stream.write_all(&data).await?;
        stream.finish()?;
    }

    Ok(())
}
