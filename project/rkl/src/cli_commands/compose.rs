use std::{
    collections, env,
    fs::{self, File, read_dir},
    path::{Path, PathBuf},
};

use anyhow::{Ok, Result, anyhow};
use libcontainer::container::State;
use liboci_cli::List;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ComposeCommand, DownArgs, PsArgs, UpArgs,
    cli_commands::ContainerRunner,
    commands::list::list,
    rootpath::{self},
    task::{ContainerSpec, Port},
};

type ComposeAction = Box<dyn FnOnce(ComposeManager) -> Result<()>>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeSpec {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub services: collections::HashMap<String, ServiceSpec>,

    #[serde(default)]
    pub volumes: Vec<VolumeSpec>,

    #[serde(default)]
    pub configs: Vec<ConfigSpec>,

    #[serde(default)]
    pub networks: Vec<NetworkSpec>,

    #[serde(default)]
    pub secrets: Vec<SecretSpec>,

    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub ports: Vec<String>,

    #[serde(default)]
    pub networks: Vec<String>,

    #[serde(default)]
    pub command: Vec<String>,

    #[serde(default)]
    pub configs: Option<Vec<String>>,

    #[serde(default)]
    pub secrets: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpec {}

pub struct ComposeManager {
    /// the path to store the basic info of compose application
    root_path: PathBuf,
    project_name: String,
}

impl ComposeManager {
    fn new(project_name: String) -> Result<Self> {
        let root_path = rootpath::determine(None)?;

        // /root_path/compose/compose_id to store the state of current compose application
        let root_path = Path::new(&root_path).join("compose").join(&project_name);
        Ok(Self {
            root_path,
            project_name,
        })
    }

    fn down(&self, _: DownArgs) -> Result<()> {
        // delete all the containers in the compose application
        if !self.root_path.exists() {
            return Err(anyhow!("The project {} does not exist", self.project_name));
        }

        // iterate rootpath find the container in the compose application and delete one by one
        for entry in read_dir(&self.root_path)? {
            let path = entry?.path();
            // delete the container directory
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }

    fn get_root_path_by_name(&self, project_name: String) -> Result<PathBuf> {
        let root_path = rootpath::determine(None)?;
        let new_path = Path::new(&root_path).join("compose").join(project_name);
        Ok(new_path)
    }

    fn up(&self, args: UpArgs) -> Result<()> {
        let compose_yaml = args.compose_yaml;
        // check the project_id exists?
        if self.root_path.exists() {
            return Err(anyhow!("The project {} already exists", self.project_name));
        }

        let target_path = get_yml_path(compose_yaml)?;

        // read the yaml
        let spec = self.read_spec(target_path)?;

        // start the whole containers
        let states = self.run(&spec)?;

        // store the spec info into a .json file
        self.persist_compose_state(states)?;

        println!("Project {} starts successfully", self.project_name);
        Ok(())
    }

    // persist the compose application's status to a json file
    ///{
    /// "project_name": "",
    /// "containers": [ {} {},],
    /// ""
    ///}
    fn persist_compose_state(&self, states: Vec<State>) -> Result<()> {
        let obj = json!({
            "project_name": self.project_name,
            "containers": states
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

    fn run(&self, spec: &ComposeSpec) -> Result<Vec<State>> {
        let mut states: Vec<State> = vec![];
        for (srv_name, srv) in &spec.services {
            let container_ports = map_port_style(srv.ports.clone())?;

            let container_spec = ContainerSpec {
                name: srv_name.clone(),
                image: srv.image.clone(),
                ports: container_ports,
                // TODO: Here just pass the command directly not support ENTRYPOINT yet
                args: srv.command.clone(),
                resources: None,
            };

            let mut runner =
                ContainerRunner::from_spec(container_spec, Some(self.root_path.clone()))?;
            runner.run()?;
            states.push(runner.get_container_state()?);
        }
        // return the compose application's state
        Ok(states)
    }

    fn ps(&self, ps_args: PsArgs) -> Result<()> {
        let PsArgs { compose_yaml } = ps_args;
        let list_arg = List {
            format: "".to_string(),
            quiet: false,
        };

        // now the self.project_name is the current_env
        if !self.root_path.exists() {
            let yml_file = get_yml_path(compose_yaml)?;
            let spec = self.read_spec(yml_file)?;
            match spec.name {
                Some(name) => {
                    let new_path = self.get_root_path_by_name(name)?;
                    list(list_arg, new_path)?;
                    return Ok(());
                }
                None => return Err(anyhow!("Invalid Compose Spec (no project name is set)")),
            }
        } else {
            // use the cur_dir first
            // list all the containers
            list(list_arg, self.root_path.clone())
        }
    }
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

pub fn get_manager_from_name(project_name: Option<String>) -> Result<ComposeManager> {
    match project_name {
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
    }
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
        ComposeCommand::Ps(ps_args) => (None, Box::new(move |manager| manager.ps(ps_args))),
    };

    let manager = get_manager_from_name(project_name)?;
    action(manager)
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use super::*;
    use anyhow::Result;

    use crate::cli_commands::ComposeManager;

    #[test]
    pub fn test_parse_compose_spec() -> Result<()> {
        let manager = ComposeManager::new("test_project".to_string()).unwrap();

        let path = PathBuf::from_str("/home/erasernoob/srv.yaml").unwrap();
        let spec = manager.read_spec(path).unwrap();
        let yaml_str = serde_yaml::to_string(&spec)?;
        println!("{}", yaml_str);
        Ok(())
    }
}
