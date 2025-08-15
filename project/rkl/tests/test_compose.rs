use std::{env, fs::File, io::Write, path::Path};  
use anyhow::anyhow;  
use common::*;  
use rkl::commands::compose::{compose_execute};  
use rkl::ComposeCommand;
use rkl::PsArgs;
use rkl::UpArgs;
use rkl::DownArgs;
use serial_test::serial;  

mod common;  

pub fn get_compose_config<T>(project_name: T) -> String
where
    T: Into<String>,
{
    let project_name = project_name.into();
    format!(
        r#"name: {}  
services:  
  backend:  
    container_name: backend  
    image: {}  
    command: ["sleep", "300"]  
    ports:  
      - "8080:8080"  
    networks:  
      - default-net  
  frontend:  
    container_name: frontend  
    image: {}  
    command: ["sleep", "300"]  
    ports:  
      - "80:80"  
    networks:  
      - default-net  
networks:  
  default-net:  
    driver: bridge  
"#,
        project_name,
        bundles_path("busybox"),
        bundles_path("busybox")
    )
}

  
#[test]  
#[serial]  
fn test_compose_up_and_down() {  
    let compose_config = get_compose_config("test-compose-app");  
    try_create_compose(compose_config, "test-compose-app");  
  
    // Verify containers are running  
    let backend_state = std::fs::read_to_string("/run/youki/compose/test-compose-app/backend/state.json");  
    let frontend_state = std::fs::read_to_string("/run/youki/compose/test-compose-app/frontend/state.json");  
      
    assert!(backend_state.is_ok());  
    assert!(frontend_state.is_ok());  
  
    // Test compose down  
    let root = env::current_dir().unwrap();  
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();
      
    compose_execute(ComposeCommand::Down(DownArgs {  
        project_name: Some("test-compose-app".to_string()),  
        compose_yaml: None,
    })).unwrap();  
      
    // Verify cleanup  
    assert!(!Path::new("/run/youki/compose/test-compose-app").exists());  
}  
  
#[test]  
#[serial]  
fn test_compose_ps() {  
    let compose_config = get_compose_config("test-compose-ps");  
    try_create_compose(compose_config, "test-compose-ps");  

    let root = env::current_dir().unwrap();  
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();
  
    // Test compose ps  
    let result = compose_execute(ComposeCommand::Ps(PsArgs {  
        project_name: Some("test-compose-ps".to_string()),  
        compose_yaml: None,
    }));  
    
    assert!(result.is_ok());  
  
    // Cleanup  
    compose_execute(ComposeCommand::Down(DownArgs {  
        project_name: Some("test-compose-ps".to_string()),  
        compose_yaml: None
    })).unwrap();  
      
    println!("{:?}", &root);
}  
  
#[test]  
#[serial]  
fn test_compose_invalid_yaml() {  
    let invalid_config = "invalid: yaml: content";  
    let res = create_compose_helper(invalid_config, "test-invalid");  
    assert!(res.is_err());  

}  
  
#[test]  
#[serial]  
fn test_compose_duplicate_project() {  
    let compose_config = get_compose_config("test-duplicate");  
    try_create_compose(compose_config.clone(), "test-duplicate");  
  
    // Try to create the same project again  
    let res = create_compose_helper(&compose_config, "test-duplicate");  
    assert!(!res.is_err());  
  
    // Cleanup  
    let root = env::current_dir().unwrap();  
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();

    compose_execute(ComposeCommand::Down(DownArgs {  
        project_name: Some("test-duplicate".to_string()),  
        compose_yaml: None
    })).unwrap();  
}  
  
fn create_compose_helper(compose_config: &str, project_name: &str) -> Result<(), anyhow::Error> {  
    let root = env::current_dir()?;  
    let config_path = root.parent().unwrap().join("test/test-compose.yaml");  
    if Path::new(&config_path).exists() {  
        std::fs::remove_file(&config_path)?;  
    }  
    let mut file = File::create(&config_path).map_err(|e| anyhow!("create failed: {e}"))?;  
    file.write_all(compose_config.as_bytes())?;  
  
    // Clean up existing compose project  
    let path_str = format!("/run/youki/compose/{}", project_name);
    let compose_dir = Path::new(&path_str);  
    if compose_dir.exists() {  
        println!("project {} already exists, deleting it to create a new one", project_name);
        std::fs::remove_dir_all(compose_dir)?;  
    }  
  
    let root = env::current_dir().unwrap();  
    let _dir_guard = DirGuard::change_to(root.parent().unwrap().join("target/debug")).unwrap();
      
    compose_execute(ComposeCommand::Up(UpArgs {  
        compose_yaml: Some(config_path.to_str().unwrap().to_string()),  
        project_name: Some(project_name.to_string()),  
    }))?;  
      
    std::fs::remove_file(config_path)?;  
    Ok(())  
}  
  
fn try_create_compose(compose_config: String, project_name: &str) {  
    let res = create_compose_helper(&compose_config, project_name);  
    if res.is_err() {  
        println!(  
            "  
            Failed to create compose application. This may be not a test failed, but caused by wrong config.
            tips:  
                1. Please build libipam and libbridge before running test.
                2. You need run tests under root.
        "  
        );  
        panic!("{}", res.unwrap_err());  
    }  
}