use std::{
    env::{self},
    fs::{self, File, read_dir, remove_dir_all, remove_file},
    path::{Path, PathBuf},
};

use anyhow::{Ok, Result, anyhow};
use libcontainer::container::State;
use liboci_cli::{Delete, List};
use serde_json::json;
use tracing::debug;

use crate::{
    ComposeCommand, DownArgs, PsArgs, UpArgs,
    commands::{
        compose::{
            config::ConfigManager, network::NetworkManager, spec::ComposeSpec,
            volume::VolumeManager,
        },
        container::{ContainerRunner, remove_container},
        delete, list,
    },
    rootpath::{self},
    task::{ContainerSpec, Port},
};

type ComposeAction = Box<dyn FnOnce(&mut ComposeManager) -> Result<()>>;

// pub mod config;
pub mod config;
pub mod network;
pub mod service;
pub mod spec;
pub mod volume;

pub struct ComposeManager {
    /// the path to store the basic info of compose application
    root_path: PathBuf,
    project_name: String,
    containers: Vec<State>,
    network_manager: NetworkManager,
    volume_manager: VolumeManager,
    config_manager: ConfigManager,
}

impl ComposeManager {
    fn new(project_name: String) -> Result<Self> {
        let root_path = rootpath::determine(None)?;

        // /root_path/compose/compose_id to store the state of current compose application
        let root_path = Path::new(&root_path).join("compose").join(&project_name);

        Ok(Self {
            root_path,
            network_manager: NetworkManager::new(project_name.clone()),
            config_manager: ConfigManager::new(),
            volume_manager: VolumeManager::new(),
            project_name,
            containers: vec![],
        })
    }

    fn down(&self, _: DownArgs) -> Result<()> {
        // delete all the containers in the compose application
        if !self.root_path.exists() {
            return Err(anyhow!("The project {} does not exist", self.project_name));
        }

        self.clean_up()
    }

    fn clean_up(&self) -> Result<()> {
        // delete container
        for container in &self.containers {
            remove_container(&self.root_path, container)?;
        }

        fs::remove_dir_all(&self.root_path)
            .map_err(|e| anyhow!("failed to delete the whole project: {}", e))
    }

    fn get_root_path_by_name(&self, project_name: String) -> Result<PathBuf> {
        let root_path = rootpath::determine(None)?;
        let new_path = Path::new(&root_path).join("compose").join(project_name);
        Ok(new_path)
    }

    fn up(&mut self, args: UpArgs) -> Result<()> {
        let compose_yaml = args.compose_yaml;
        // check the project_id exists?
        if self.root_path.exists() {
            return Err(anyhow!("The project {} already exists", self.project_name));
        }

        let target_path = get_yml_path(compose_yaml)?;

        // read the yaml
        let spec = parse_spec(target_path)?;

        // top-field manager handle those field
        let _ = &mut self.network_manager.handle(&spec)?;

        let _ = &mut self.volume_manager.handle(&spec)?;

        let _ = &mut self.config_manager.handle(&spec);

        // start the whole containers
        if let Err(err) = self.run(&spec) {
            self.clean_up().ok();
            return Err(anyhow!("failed to up: {}", err));
        }

        // store the spec info into a .json file
        self.persist_compose_state()?;

        println!("Project {} starts successfully", self.project_name);
        Ok(())
    }

    // persist the compose application's status to a json file
    ///{
    /// "project_name": "",
    /// "containers": [ {} {},],
    /// ""
    ///}
    fn persist_compose_state(&self) -> Result<()> {
        let obj = json!({
            "project_name": self.project_name,
            "containers": &self.containers
        });
        let json_str = serde_json::to_string_pretty(&obj)?;

        let file_path = self.root_path.join("state.json");
        fs::create_dir_all(&self.root_path)?;
        fs::write(file_path, json_str)?;
        Ok(())
    }

    pub fn read_spec(&self, path: PathBuf) -> Result<ComposeSpec> {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow!("compose.yml file is None"))?;
        let reader = File::open(path)?;
        let spec: ComposeSpec = serde_yaml::from_reader(reader).map_err(|_| {
            anyhow!("Read the compose specification failed, make sure the file is valid")
        })?;
        Ok(spec)
    }

    fn run(&mut self, _: &ComposeSpec) -> Result<()> {
        let network_mapping = self.network_manager.network_service_mapping();

        for (network_name, services) in network_mapping {
            println!("Creating network: {network_name}");

            for (srv_name, srv) in services.into_iter() {
                let container_ports = map_port_style(srv.ports.clone())?;
                let container_spec = ContainerSpec {
                    name: srv
                        .container_name
                        .clone()
                        .unwrap_or(self.generate_container_name(&srv_name)),
                    image: srv.image.clone(),
                    ports: container_ports,
                    // TODO: Here just pass the command directly not support ENTRYPOINT yet
                    args: srv.command.clone(),
                    resources: None,
                };

                // generate the volumes Mount
                let volumes = VolumeManager::map_to_mount(srv.volumes.clone())?;

                debug!("get mount: {:#?}", volumes);

                //  setup the network_conf file
                self.network_manager
                    .setup_network_conf(&network_name)
                    .map_err(|e| {
                        anyhow!(
                            "Service [{}] create network Config file failed: {}",
                            srv_name,
                            e
                        )
                    })?;
                // get config
                let configs_mounts = self.config_manager.get_mounts_by_service(&srv_name);

                let mut runner =
                    ContainerRunner::from_spec(container_spec, Some(self.root_path.clone()))?;

                runner.add_mounts(volumes);
                runner.add_mounts(configs_mounts);

                match runner.run() {
                    std::result::Result::Ok(_) => {
                        self.containers.push(runner.get_container_state()?);
                    }
                    Err(err) => {
                        // create one container failed delete others
                        println!(
                            "container {} created failed: {}",
                            runner.get_container_id()?,
                            err
                        );
                        for state in &self.containers {
                            if let Err(err) = delete(
                                Delete {
                                    container_id: state.id.clone(),
                                    force: true,
                                },
                                self.root_path.clone(),
                            ) {
                                println!("container {} deleted failed: {}", state.id, err)
                            } else {
                                println!("container {} deleted during the rollback", state.id)
                            }
                        }
                        return Err(err);
                    }
                };
            }
        }
        // return the compose application's state
        Ok(())
    }

    fn ps(&self, ps_args: PsArgs) -> Result<()> {
        let PsArgs {
            compose_yaml,
            project_name,
        } = ps_args;
        let list_arg = List {
            format: "".to_string(),
            quiet: false,
        };

        let target_path = if !self.root_path.exists() {
            let yml_file = get_yml_path(compose_yaml)?;
            let spec = self.read_spec(yml_file)?;
            match spec.name {
                Some(name) => self.get_root_path_by_name(name)?,
                None => return Err(anyhow!("Invalid Compose Spec (no project name is set)")),
            }
        } else if let Some(name) = project_name {
            self.get_root_path_by_name(name)?
        } else {
            self.root_path.clone()
        };

        list(list_arg, target_path).map_err(|e| {
            if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
                if io_err.kind() == std::io::ErrorKind::NotFound {
                    return anyhow!("There is no running compose application");
                }
            }
            // Fallback for other errors, ensuring all list errors are handled consistently
            anyhow!("Failed to list compose containers: {}", e)
        })
    }

    /// if the `container_name` field is not supplied then create a random container_name
    /// for the service container
    pub fn generate_container_name(&self, srv_name: &String) -> String {
        let root = self
            .root_path
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or("unknown");
        let timestamp = chrono::Utc::now().timestamp() % 1000; // persist 4 bits
        format!("{root}_{srv_name}_{timestamp}")
    }
}

pub fn parse_spec(path: PathBuf) -> Result<ComposeSpec> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("compose.yml file is None"))?;
    let reader = File::open(path)?;
    let spec: ComposeSpec = serde_yaml::from_reader(reader).map_err(|e| {
        anyhow!(
            "Read the compose specification failed, make sure the file is valid: {}",
            e
        )
    })?;
    Ok(spec)
}

/// delete all the file and dir in the target_dir
pub fn clear_dir<P: AsRef<Path>>(dir: P) -> Result<()> {
    for entry in read_dir(&dir)? {
        let path = entry?.path();
        if path.is_dir() {
            remove_dir_all(&path).map_err(|e| {
                anyhow!(
                    "failed to delete the {}: {}",
                    path.file_name()
                        .and_then(|os_str| os_str.to_str())
                        .unwrap_or("unknown"),
                    e
                )
            })?;
        } else {
            remove_file(&path).map_err(|e| {
                anyhow!(
                    "failed to remove the file {}: {}",
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown"),
                    e
                )
            })?;
        }
    }
    Ok(())
}

// map the compose-style port to k8s-container-style ports
// compose-style: "(host-ip) 80: (container-ip) 3000"
// k8s-container-style:
// - containerPort: 80
//   protocol: ""
//   hostPort: 0
//   hostIP: "" default is ""
fn map_port_style(ports: Vec<String>) -> Result<Vec<Port>> {
    ports
        .into_iter()
        .map(|port| {
            let parts: Vec<&str> = port.split(":").collect();
            let (host_ip, host_port, container_port) = match parts.len() {
                2 => ("", parts[0], parts[1]),
                3 => (parts[0], parts[1], parts[2]),
                _ => return Err(anyhow!("Invalid port mapping syntax in compose file")),
            };

            let host_port = host_port
                .parse::<i32>()
                .map_err(|_| anyhow!("Invalid port mapping syntax in compose file"))?;

            let container_port = container_port
                .parse::<i32>()
                .map_err(|_| anyhow!("Invalid port mapping syntax in compose file"))?;

            let host_ip = host_ip.to_string();

            Ok(Port {
                container_port,
                protocol: "".to_string(),
                host_port,
                host_ip,
            })
        })
        .collect()
}

pub fn get_yml_path(compose_yaml: Option<String>) -> Result<PathBuf> {
    let target_path = if let Some(path) = compose_yaml {
        PathBuf::from(path)
    } else {
        let cwd = env::current_dir()?;
        let yml = cwd.join("compose.yml");
        let yaml = cwd.join("compose.yaml");
        if yml.exists() {
            yml
        } else if yaml.exists() {
            yaml
        } else {
            return Err(anyhow!(
                "No compose.yml or compose.yaml file in current directory: {}",
                cwd.display()
            ));
        }
    };
    Ok(target_path)
}

pub fn get_manager_from_name(project_name: Option<String>) -> Result<Box<ComposeManager>> {
    let manager = match project_name {
        Some(name) => ComposeManager::new(name),
        None => {
            let cwd = env::current_dir()?;
            let project_name = cwd
                .file_name()
                .and_then(|os_str| os_str.to_str())
                .ok_or_else(|| anyhow!("Failed to get current directory'name"))?
                .to_string();
            ComposeManager::new(project_name)
        }
    }?;
    Ok(Box::new(manager))
}

pub fn compose_execute(command: ComposeCommand) -> Result<()> {
    let (project_name, action): (Option<String>, ComposeAction) = match command {
        ComposeCommand::Up(up_args) => {
            let name = up_args.project_name.clone();
            (name, Box::new(move |manager| manager.up(up_args)))
        }
        ComposeCommand::Down(down_args) => {
            let name = down_args.project_name.clone();
            (name, Box::new(move |manager| manager.down(down_args)))
        }
        ComposeCommand::Ps(ps_args) => {
            let name = ps_args.project_name.clone();
            (name, Box::new(move |manager| manager.ps(ps_args)))
        }
    };

    let mut manager = get_manager_from_name(project_name)?;
    action(&mut manager)
}

#[cfg(test)]
mod test {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::tempdir;

    fn get_test_yml() -> String {
        r#"
name: test_proj
services:
  web:
    image: test/bundles/busybox/
    ports: ["8080:80"]
    volumes: 
      - ./tmp/mount/dir:/app/data
      - ./data:/app/data2
volumes:
  
"#
        .to_string()
    }

    fn get_test_multiple_service() -> String {
        r#"
services:
  backend:
    container_name: back
    image: test/bundles/busybox
    command: ["sleep", "300"]
    ports:
      - "8080:8080"
    networks:
      - libra-net
    volumes:
      - /tmp/mount/dir:/app/data
  frontend:
    container_name: front
    image: test/bundles/busybox
    command: ["sleep", "300"]
    ports:
      - "80:80"
networks: 
  libra-net: 
    driver: bridge 
"#
        .to_string()
    }

    #[test]
    fn test_new_compose_manager() {
        let mgr = ComposeManager::new("demo_proj".to_string());
        assert!(mgr.is_ok());
        let mgr = mgr.unwrap();
        assert!(mgr.root_path.ends_with("compose/demo_proj"));
        assert_eq!(mgr.project_name, "demo_proj");
    }

    #[test]
    fn test_get_root_path_by_name() {
        let mgr = ComposeManager::new("abc".to_string()).unwrap();
        let path = mgr.get_root_path_by_name("xyz".to_string()).unwrap();
        assert!(path.ends_with("compose/xyz"));
    }

    #[test]
    fn test_persist_and_read_spec() {
        let dir = tempdir().unwrap();
        let test_path = dir.path().join("compose.yml");
        let yaml = get_test_yml();

        fs::write(&test_path, yaml).unwrap();
        let mgr = ComposeManager::new("test_proj".to_string()).unwrap();
        let spec = mgr.read_spec(test_path.clone()).unwrap();
        assert_eq!(spec.name, Some("test_proj".to_string()));
        assert!(spec.services.contains_key("web"));
        assert_eq!(spec.services["web"].image, "test/bundles/busybox/");
        assert_eq!(spec.services["web"].volumes[0], "./tmp/mount/dir:/app/data");
        assert_eq!(spec.services["web"].volumes[1], "./data:/app/data2");
    }

    #[tokio::test]
    #[serial]
    async fn test_map_volume_style() {
        let volumes = vec![
            "./tmp/mount/dir:/app/data:ro".to_string(),
            "/home/erasernoob/data:/app/data2".to_string(),
        ];
        let mapped = VolumeManager::string_to_pattern(volumes).unwrap();
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].host_path, "./tmp/mount/dir");
        assert_eq!(mapped[0].container_path, "/app/data");
        assert!(mapped[0].read_only);
        assert_eq!(mapped[1].host_path, "/home/erasernoob/data");
        assert_eq!(mapped[1].container_path, "/app/data2");
        assert!(!mapped[1].read_only);
    }

    #[test]
    fn test_map_port_style() {
        let ports = vec!["127.0.0.1:8080:80".to_string(), "8081:81".to_string()];
        let mapped = map_port_style(ports).unwrap();
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].host_ip, "127.0.0.1");
        assert_eq!(mapped[0].host_port, 8080);
        assert_eq!(mapped[0].container_port, 80);
        assert_eq!(mapped[1].host_ip, "");
        assert_eq!(mapped[1].host_port, 8081);
        assert_eq!(mapped[1].container_port, 81);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_yml_path_with_none() {
        let dir = tempdir().unwrap();
        let yml = dir.path().join("compose.yml");
        fs::write(&yml, "name: demo\nservices: {}\n").unwrap();
        let _cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let path = get_yml_path(None).unwrap();
        assert!(path.ends_with("compose.yml"));
        std::env::set_current_dir(_cwd).unwrap();
    }

    #[test]
    fn test_get_manager_from_name_some() {
        let mgr = get_manager_from_name(Some("abc_proj".to_string())).unwrap();
        assert_eq!(mgr.project_name, "abc_proj");
    }

    #[tokio::test]
    #[serial]
    async fn test_up() {
        let root_dir = tempdir().unwrap();
        let root_path = root_dir.path();
        let project_name = root_dir
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        fs::write(
            root_dir.path().join("compose.yml"),
            get_test_multiple_service(),
        )
        .unwrap();
        // cd to the current_dir's parent
        let root = env::current_dir().expect("Failed to get current dev");
        env::set_current_dir(root.parent().unwrap()).unwrap();

        let mut manager = ComposeManager::new(project_name.clone()).unwrap();
        manager
            .up(UpArgs {
                compose_yaml: Some(root_path.join("compose.yml").to_str().unwrap().to_owned()),
                project_name: Some(project_name),
            })
            .unwrap();

        env::set_current_dir(root).unwrap();
    }
}
