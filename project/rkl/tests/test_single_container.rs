use std::{env, fs::File, io::Write, path::Path};
mod common;
use anyhow::anyhow;
use common::*;
use rkl::{
    commands::container::{create_container, delete_container, run_container, start_container},
    task::{ContainerRes, ContainerSpec, Port, Resource},
};
use serde_json::Value;
use serial_test::serial;

pub fn get_container_config<T, S>(args: Vec<S>, name: T) -> ContainerSpec
where
    T: Into<String>,
    S: Into<String>,
{
    let name = name.into();
    let args = args.into_iter().map(Into::into).collect();
    ContainerSpec {
        name,
        image: bundles_path("busybox"),
        args,
        ports: vec![Port {
            container_port: 80,
            protocol: String::new(),
            host_ip: String::new(),
            host_port: 0,
        }],
        resources: None,
    }
}

#[test]
#[serial]
fn test_container_create_start_and_delete() {
    let config = get_container_config(
        vec!["sleep".to_string(), "100".to_string()],
        "test-container",
    );
    try_create_container(config, false);

    let container_state = std::fs::read_to_string("/run/youki/test-container/state.json").unwrap();
    let container_state: Value = serde_json::from_str(&container_state).unwrap();
    assert_eq!(container_state["status"], "created");

    let root = env::current_dir().unwrap();
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();
    start_container("test-container").unwrap();

    let container_state = std::fs::read_to_string("/run/youki/test-container/state.json").unwrap();
    let container_state: Value = serde_json::from_str(&container_state).unwrap();
    assert_eq!(container_state["status"], "running");

    delete_container("test-container").unwrap();

    assert!(!Path::new("/run/youki/test-container").exists());
}

#[test]
#[serial]
fn test_container_run() {
    let config = get_container_config(
        vec!["sleep".to_string(), "100".to_string()],
        "test-container-run",
    );
    try_create_container(config, true);

    let container_state =
        std::fs::read_to_string("/run/youki/test-container-run/state.json").unwrap();
    let container_state: Value = serde_json::from_str(&container_state).unwrap();
    assert_eq!(container_state["status"], "running");

    delete_container_helper("test-container-run");
}

#[test]
#[serial]
fn test_container_cpu_and_memory_limit() {
    let mut config = get_container_config(
        vec!["sleep".to_string(), "100".to_string()],
        "test-container-resources",
    );
    config.resources = Some(ContainerRes {
        limits: Some(Resource {
            cpu: Some("500m".to_string()),
            memory: Some("233Mi".to_string()),
        }),
    });
    try_create_container(config, true);

    let cpu_max =
        std::fs::read_to_string("/sys/fs/cgroup/:youki:test-container-resources/cpu.max").unwrap();
    assert_eq!(cpu_max.trim(), "500000 1000000");

    let memory_max =
        std::fs::read_to_string("/sys/fs/cgroup/:youki:test-container-resources/memory.max")
            .unwrap();
    assert_eq!(memory_max.trim(), format!("{}", 233 * 1024 * 1024));

    delete_container_helper("test-container-resources");
}

#[test]
#[serial]
fn test_container_invalid_name() {
    let config = get_container_config(vec!["echo".to_string(), "test".to_string()], "");
    let res = create_container_helper(config, false);
    assert!(res.is_err());
}

#[test]
#[serial]
fn test_container_nonexistent_image() {
    let mut config = get_container_config(
        vec!["echo".to_string(), "test".to_string()],
        "test-nonexistent",
    );
    config.image = "/nonexistent/path".to_string();
    let res = create_container_helper(config, false);
    assert!(res.is_err());
}

fn create_container_helper(config: ContainerSpec, run: bool) -> Result<(), anyhow::Error> {
    let file_content = serde_yaml::to_string(&config)?;
    let root = env::current_dir()?;
    let config_path = root.parent().unwrap().join("test/test-container.yaml");
    if Path::new(&config_path).exists() {
        std::fs::remove_file(&config_path)?;
    }
    let mut file = File::create(&config_path).map_err(|e| anyhow!("create failed: {e}"))?;
    file.write_all(file_content.as_bytes())?;

    // Clean up existing container
    let path = format!("/run/youki/{}", config.name);
    let container_dir = Path::new(&path);
    if container_dir.exists() {
        std::fs::remove_dir_all(container_dir)?;
    }

    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();

    if !run {
        create_container(config_path.to_str().unwrap())?;
    } else {
        run_container(config_path.to_str().unwrap())?;
    }
    std::fs::remove_file(config_path)?;
    Ok(())
}

fn try_create_container(config: ContainerSpec, run: bool) {
    let res = create_container_helper(config, run);
    if res.is_err() {
        println!(
            "  
            Failed to create container. This may be not a test failed, but caused by wrong config.
            tips:
                1. Please build libipam and libbridge before running test.
                2. You need run tests under root.
        "
        );
        panic!("{}", res.unwrap_err());
    }
}

fn delete_container_helper(container_name: &str) {
    let root = env::current_dir().unwrap();
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();
    delete_container(container_name).unwrap();
}
