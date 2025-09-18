#![allow(unused)]
use crate::api::xlinestore::XlineStore;
use anyhow::Result;
use clap::builder::Str;
use common::{PodTask, RksMessage};
use log::info;
use quinn::Connection;
use std::sync::Arc;
pub async fn watch_delete(
    pod_name: String,
    pod_yaml: String,
    conn: &Connection,
    node_id: &str,
) -> Result<()> {
    if let Ok(pod_task) = serde_yaml::from_str::<PodTask>(&pod_yaml)
        && pod_task.spec.node_name.as_deref() == Some(node_id)
    {
        info!(
            "[watch_pods] DELETE pod_name={} for node={}",
            pod_name, node_id
        );

        let msg = RksMessage::DeletePod(pod_name.clone());
        let data = bincode::serialize(&msg)?;
        if let Ok(mut stream) = conn.open_uni().await {
            stream.write_all(&data).await?;
            stream.finish()?;
            info!("[watch_pods] sent delete pod to worker {}", node_id);
        }
    }
    Ok(())
}

pub async fn user_delete(
    pod_name: String,
    xline_store: &Arc<XlineStore>,
    conn: &Connection,
) -> Result<()> {
    xline_store.delete_pod(&pod_name).await?;
    println!("[user_delete] deleted pod {} (written to xline)", pod_name);

    let response = RksMessage::Ack;
    let data = bincode::serialize(&response)?;
    if let Ok(mut stream) = conn.open_uni().await {
        stream.write_all(&data).await?;
        stream.finish()?;
    }
    Ok(())
}
