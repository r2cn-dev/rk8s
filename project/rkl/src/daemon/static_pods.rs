use anyhow::anyhow;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::{collections::HashSet, path::Path, sync::Arc, time::Duration};
use tokio::fs::{File, read_dir};
use tokio::io::AsyncReadExt;
use tracing::{error, warn};

use futures::FutureExt;
use tokio::time::sleep;

use crate::task::TaskRunner;
use crate::{cli_commands, task::PodTask};

use crate::daemon::sync_loop::{Event, State, WithEvent};

/// Check and ensure that the pod status is consistent with the requirements every five seconds.
pub struct CheckStaticPodsPeriodically;

impl Event<()> for CheckStaticPodsPeriodically {
    /// Generate a check event every 5 seconds.
    fn listen() -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        async {
            sleep(Duration::from_secs(5)).await;
        }
        .boxed()
    }
}

/// Ensure that the pod status is consistent with the requirements every five seconds.
/// - If an error occurs, it will be logged and the operation will be stopped.
pub async fn handler(
    state: Arc<State>,
    _data: Box<()>,
    _event: WithEvent<CheckStaticPodsPeriodically>,
) {
    // now, it only moniter the default directory.
    let static_pods = read_pods_from_dir("/etc/rk8s/manifests").await;
    if let Err(e) = static_pods {
        error!("Failed to check static pods: {e}");
        return;
    }

    let pods = static_pods.unwrap();
    let stop_res = stop_removed_pods(state.clone(), &pods).await;
    if let Err(e) = stop_res {
        error!("Failed to stop removed pods: {e}");
        return;
    }
    run_new_pods(state.clone(), pods).await;
}

/// Caculate hash value by its yaml.
fn calculate_hash(p: &PodTask) -> u64 {
    let t = serde_yaml::to_string(p).unwrap();
    // Since the serialization results may be inconsistent, sort lines.
    let mut tmp: Vec<_> = t.split('\n').collect();
    tmp.sort();
    let mut s = DefaultHasher::new();
    tmp.hash(&mut s);
    s.finish()
}

/// Try to start pods that in static pods config directory but not created.
/// - It will be logged as an error if start failed.
async fn run_new_pods(state: Arc<State>, pods: Vec<PodTask>) {
    let mut current_pods = state.pods_mut().await;
    let mut pods_set = HashSet::new();
    for p in &*current_pods {
        let hs = calculate_hash(p);
        pods_set.insert(hs);
    }
    pods.into_iter()
        .filter(|p| !pods_set.contains(&calculate_hash(p)))
        .for_each(|p| {
            let runner = TaskRunner::from_task(p.clone()).unwrap();
            let name = runner.task.metadata.name.clone();
            let res = cli_commands::run_pod_from_taskrunner(runner);
            if let Err(e) = res {
                error!("Failed to run pod {}: {e}", name);
            } else {
                (*current_pods).push(p);
            }
        });
}

/// Try to remove pods that not in static pods config directory but created.
async fn stop_removed_pods(state: Arc<State>, pods: &Vec<PodTask>) -> Result<(), anyhow::Error> {
    let mut pods_hash = HashSet::new();
    for p in pods {
        let hs = calculate_hash(p);
        pods_hash.insert(hs);
    }
    // must get write lock to avoid other thread operate pods simitaneously.
    let mut current_pods = state.pods_mut().await;

    let del_err_vec: Vec<_> = (*current_pods)
        .iter()
        .filter(|&p| {
            let hs = calculate_hash(p);
            !pods_hash.contains(&hs)
        })
        .map(|p| cli_commands::delete_pod(&p.metadata.name))
        .filter(|r| r.is_err())
        .collect();
    if !del_err_vec.is_empty() {
        return Err(anyhow!("{del_err_vec:?}"));
    }

    (*current_pods) = current_pods
        .drain(..)
        .filter(|p| pods_hash.contains(&calculate_hash(p)))
        .collect();
    Ok(())
}

/// Get pods from static pods config directory.
async fn read_pods_from_dir<P: AsRef<Path>>(path: P) -> Result<Vec<PodTask>, anyhow::Error> {
    let mut entries = read_dir(path)
        .await
        .map_err(|e| anyhow!("Failed to read default static pods dir: {e}"))?;
    let mut res = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| anyhow!("Failed to read default static pods dir entries: {e}"))?
    {
        let file_path = entry.path();
        let mut content = String::new();
        let mut file = File::open(&file_path)
            .await
            .map_err(|e| anyhow!("Failed to open static pods config file {file_path:#?}: {e}"))?;
        match file.read_to_string(&mut content).await {
            Ok(_) => match serde_yaml::from_str(&content) {
                Ok(r) => res.push(r),
                Err(e) => warn!("Failed to parse pod config file {file_path:#?}: {e}. Skipped."),
            },
            Err(e) => warn!("Pod config file {file_path:#?} is not valid utf8: {e}, skipped."),
        }
    }
    Ok(res)
}
