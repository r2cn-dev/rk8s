use rks::api::xlinestore::XlineStore;
use rks::protocol::config::load_config;
use futures::StreamExt;
use tokio::time::{timeout, Duration, sleep};
use std::sync::Arc;
use etcd_client::EventType;

fn load_store() -> Arc<XlineStore> {
    let config_path = std::env::var("TEST_CONFIG_PATH").unwrap_or_else(|_| {
        format!("{}/tests/config.yaml", std::env::var("CARGO_MANIFEST_DIR").unwrap())
    });
    let config = load_config(&config_path).expect("Failed to load config");
    let endpoints: Vec<&str> = config.xline_config.endpoints.iter().map(|s| s.as_str()).collect();
    Arc::new(tokio::runtime::Handle::current().block_on(async {
        XlineStore::new(&endpoints).await.expect("connect xline failed")
    }))
}

#[tokio::test]
async fn test_watch_pods_create_update_delete() {
    let store = load_store();

    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();
    let (_watcher, mut stream) = store.watch_pods(rev + 1).await.unwrap();

    let pod_name = format!("watch-pod-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let yaml_v1 = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml_v1).await.unwrap();

    let resp = timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap().unwrap();
    let ev = resp.events().first().unwrap();
    assert!(String::from_utf8_lossy(ev.kv().as_ref().unwrap().key()).ends_with(&pod_name));
    assert_eq!(ev.event_type(), EventType::Put);

    let yaml_v2 = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n  labels:\n    v: \"2\"\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml_v2).await.unwrap();

    let resp2 = timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap().unwrap();
    let ev2 = resp2.events().first().unwrap();
    assert_eq!(ev2.event_type(), EventType::Put);
    assert!(ev2.prev_kv().is_some(), "expected prev_kv for update");

    store.delete_pod(&pod_name).await.unwrap();
    let resp3 = timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap().unwrap();
    let ev3 = resp3.events().first().unwrap();
    assert_eq!(ev3.event_type(), EventType::Delete);
}

#[tokio::test]
async fn test_watch_pods_multiple_updates_order() {
    let store = load_store();
    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();
    let (_watcher, mut stream) = store.watch_pods(rev + 1).await.unwrap();

    let pod_name = format!("multi-update-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let mut yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();

    for i in 2..=4 {
        yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n  labels:\n    v: \"{}\"\n", pod_name, i);
        store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();
    }

    let mut revisions = vec![];
    for _ in 0..4 {
        let resp = timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap().unwrap();
        let ev = resp.events().first().unwrap();
        revisions.push(ev.kv().as_ref().unwrap().mod_revision());
    }
    for w in revisions.windows(2) {
        assert!(w[0] < w[1], "expected increasing revisions, got {:?}", revisions);
    }
}

#[tokio::test]
async fn test_watch_from_stale_revision() {
    let store = load_store();
    let result = store.watch_pods(1).await;
    assert!(result.is_err(), "expected error when watching from stale rev");
}

#[tokio::test]
async fn test_watch_cancel() {
    let store = load_store();
    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();
    let (watcher, mut stream) = store.watch_pods(rev + 1).await.unwrap();

    drop(watcher);

    let pod_name = format!("cancel-pod-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();

    let next = timeout(Duration::from_secs(2), stream.next()).await;
    assert!(next.is_err() || next.unwrap().is_none(), "expected stream closed after cancel");
}

#[tokio::test]
#[ignore]
async fn test_watch_reconnect_manual() {
    let store = load_store();
    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();
    let (_watcher, mut stream) = store.watch_pods(rev + 1).await.unwrap();

    let pod_name = format!("reconnect-pod-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();

    let resp = timeout(Duration::from_secs(15), stream.next()).await.expect("timeout waiting after reconnect");
    assert!(resp.is_some(), "expected to receive event after reconnect");
}

#[tokio::test]
async fn test_watch_backpressure() {
    let store = load_store();
    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();
    let (_watcher, mut stream) = store.watch_pods(rev + 1).await.unwrap();

    let pod_name = format!("backpressure-pod-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());

    for i in 0..20 {
        let yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n  labels:\n    v: \"{}\"\n", pod_name, i);
        store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();
    }

    sleep(Duration::from_secs(2)).await;

    let mut count = 0;
    while let Ok(Some(Ok(resp))) = timeout(Duration::from_secs(2), stream.next()).await {
        count += resp.events().len();
        if count >= 20 { break; }
    }

    assert!(count > 0, "expected to receive events even under backpressure");
}

#[tokio::test]
async fn test_watch_multiple_resources() {
    let store = load_store();
    let (_items, rev) = store.pods_snapshot_with_rev().await.unwrap();

    let (_watcher1, mut stream1) = store.watch_pods(rev + 1).await.unwrap();
    let (_watcher2, mut stream2) = store.watch_pods(rev + 1).await.unwrap();

    let pod_name = format!("multi-prefix-pod-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n", pod_name);
    store.insert_pod_yaml(&pod_name, &yaml).await.unwrap();

    let resp1 = timeout(Duration::from_secs(5), stream1.next()).await.unwrap().unwrap().unwrap();
    let resp2 = timeout(Duration::from_secs(5), stream2.next()).await.unwrap().unwrap().unwrap();

    let ev1 = resp1.events().first().unwrap();
    let ev2 = resp2.events().first().unwrap();
    assert_eq!(ev1.event_type(), EventType::Put);
    assert_eq!(ev2.event_type(), EventType::Put);
}


