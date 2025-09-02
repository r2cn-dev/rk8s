use rks::api::xlinestore::XlineStore;
use rks::protocol::config::load_config;
use std::sync::Arc;


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
async fn test_xline_rw() {
let store = load_store();


// Generate unique pod name
let pod_name = format!("pod-test-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap());
let pod_yaml = format!("apiVersion: v1\nkind: Pod\nmetadata:\n name: {}\n", pod_name);


// Insert Pod YAML
store.insert_pod_yaml(&pod_name, &pod_yaml).await.expect("Insert pod yaml failed");


// Fetch and verify
let fetched = store.get_pod_yaml(&pod_name).await.expect("Get pod yaml failed");
assert_eq!(fetched.as_deref(), Some(pod_yaml.as_str()));


// List pods and check presence
let pods = store.list_pods().await.expect("List pods failed");
assert!(pods.contains(&pod_name));


// Clean up
store.delete_pod(&pod_name).await.expect("Delete pod failed");
}
