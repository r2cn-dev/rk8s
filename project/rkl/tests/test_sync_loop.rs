use rkl::daemon::{static_pods, sync_loop::SyncLoop};
use serial_test::serial;
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::common::get_pod_config;

mod common;

#[tokio::test]
#[serial]
async fn test_static_pod() {
    let sync_loop = SyncLoop::default().register_event(static_pods::handler);
    let handle = tokio::spawn(sync_loop.run());
    let yes_pod = get_pod_config(vec!["sh", "-c", "yes > /dev/null"], "yes-pod");
    let zero_null_pod = get_pod_config(vec!["dd", "if=/dev/zero", "of=/dev/null"], "zero-null-pod");
    let manifest_path = PathBuf::from("/etc/rk8s/manifests");
    assert!(manifest_path.exists());

    let mut pod_file = File::create(manifest_path.join("yes_pod.yaml")).unwrap();
    pod_file
        .write_all(serde_yaml::to_string(&yes_pod).unwrap().as_bytes())
        .unwrap();
    let mut pod_file = File::create(manifest_path.join("zero-null-pod.yaml")).unwrap();
    pod_file
        .write_all(serde_yaml::to_string(&zero_null_pod).unwrap().as_bytes())
        .unwrap();

    tokio::time::sleep(Duration::from_secs(8)).await;

    assert!(Path::new("/run/youki/yes-pod-main-container1").exists());
    assert!(Path::new("/run/youki/yes-pod").exists());
    assert!(Path::new("/run/youki/pods/yes-pod").exists());
    assert!(Path::new("/run/youki/zero-null-pod-main-container1").exists());
    assert!(Path::new("/run/youki/zero-null-pod").exists());
    assert!(Path::new("/run/youki/pods/zero-null-pod").exists());

    std::fs::remove_file(manifest_path.join("yes_pod.yaml")).unwrap();
    std::fs::remove_file(manifest_path.join("zero-null-pod.yaml")).unwrap();

    tokio::time::sleep(Duration::from_secs(8)).await;

    assert!(!Path::new("/run/youki/yes-pod-main-container1").exists());
    assert!(!Path::new("/run/youki/yes-pod").exists());
    assert!(!Path::new("/run/youki/pods/yes-pod").exists());
    assert!(!Path::new("/run/youki/zero-null-pod-main-container1").exists());
    assert!(!Path::new("/run/youki/zero-null-pod").exists());
    assert!(!Path::new("/run/youki/pods/zero-null-pod").exists());

    assert!(!handle.is_finished());
}

#[tokio::test]
#[serial]
async fn test_skip_invalid_pod() {
    let sync_loop = SyncLoop::default().register_event(static_pods::handler);
    let handle = tokio::spawn(sync_loop.run());
    let yes_pod = get_pod_config(vec!["sh", "-c", "yes > /dev/null"], "yes-pod");
    let manifest_path = PathBuf::from("/etc/rk8s/manifests");
    assert!(manifest_path.exists());

    let mut pod_file = File::create(manifest_path.join("yes_pod.yaml")).unwrap();
    pod_file
        .write_all(serde_yaml::to_string(&yes_pod).unwrap().as_bytes())
        .unwrap();
    let mut pod_file = File::create(manifest_path.join("wrong.yaml")).unwrap();
    pod_file.write_all("Wrong config file!".as_bytes()).unwrap();

    tokio::time::sleep(Duration::from_secs(8)).await;

    assert!(Path::new("/run/youki/yes-pod-main-container1").exists());
    assert!(Path::new("/run/youki/yes-pod").exists());
    assert!(Path::new("/run/youki/pods/yes-pod").exists());

    std::fs::remove_file(manifest_path.join("yes_pod.yaml")).unwrap();
    std::fs::remove_file(manifest_path.join("wrong.yaml")).unwrap();

    tokio::time::sleep(Duration::from_secs(8)).await;

    assert!(!Path::new("/run/youki/yes-pod-main-container1").exists());
    assert!(!Path::new("/run/youki/yes-pod").exists());
    assert!(!Path::new("/run/youki/pods/yes-pod").exists());

    assert!(!handle.is_finished());
}
