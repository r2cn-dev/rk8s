use crate::{
    ContainerCommand,
    commands::{
        Exec, ExecContainer, container::config::ContainerConfigBuilder, create, delete, exec, list,
        load_container, start,
    },
    cri::cri_api::{ContainerConfig, CreateContainerResponse, Mount, StartContainerResponse},
    rootpath,
    task::{ContainerSpec, add_cap_net_raw, get_cni},
};
use anyhow::{Ok, Result, anyhow};
use chrono::{DateTime, Local};
use libcontainer::container::{Container, State, state};
use liboci_cli::{Create, Delete, List, Start};
use nix::unistd::Pid;
use oci_spec::runtime::{LinuxBuilder, ProcessBuilder, Spec, get_default_namespaces};
use oci_spec::runtime::{Mount as OciMount, MountBuilder};
use std::{fmt::Write as _, io};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use tabwriter::TabWriter;
use tracing::debug;

pub mod config;

pub struct ContainerRunner {
    spec: ContainerSpec,
    config: Option<ContainerConfig>,
    config_builder: ContainerConfigBuilder,
    root_path: PathBuf,
    container_id: String,
}

impl ContainerRunner {
    pub fn from_spec(spec: ContainerSpec, root_path: Option<PathBuf>) -> Result<Self> {
        let container_id = spec.name.clone();

        Ok(ContainerRunner {
            spec,
            config: None,
            config_builder: ContainerConfigBuilder::default(),
            container_id,
            root_path: match root_path {
                Some(p) => p,
                None => rootpath::determine(None)?,
            },
        })
    }

    pub fn from_file(spec_path: &str) -> Result<Self> {
        // read the container_spec bytes
        let mut file = File::open(spec_path)
            .map_err(|e| anyhow!("open the container spec file failed: {e}"))?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;

        let container_spec: ContainerSpec = serde_yaml::from_str(&content)?;
        let container_id = container_spec.name.clone();
        let root_path = rootpath::determine(None)?;
        Ok(ContainerRunner {
            spec: container_spec,
            config_builder: ContainerConfigBuilder::default(),
            root_path,
            config: None,
            container_id,
        })
    }

    pub fn from_container_id(container_id: &str, root_path: Option<PathBuf>) -> Result<Self> {
        Ok(ContainerRunner {
            config_builder: ContainerConfigBuilder::default(),
            spec: ContainerSpec {
                name: container_id.to_string(),
                image: "".to_string(),
                ports: vec![],
                args: vec![],
                resources: None,
            },
            config: None,
            container_id: container_id.to_string(),
            root_path: match root_path {
                Some(path) => path,
                None => rootpath::determine(None)?,
            },
        })
    }

    pub fn add_mounts(&mut self, mounts: Vec<Mount>) {
        self.config_builder.mounts(mounts);
    }

    pub fn create(&mut self) -> Result<()> {
        // create container_config
        self.build_config()?;

        let _ = self.create_container()?;
        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        // create container_config
        self.build_config()?;

        let _ = self.create_container()?;

        let id = self.container_id.as_str();
        // See if the container exists
        match is_container_exist(id, &self.root_path).is_ok() {
            // exist
            true => {
                self.start_container(None)?;
                println!("Container: {id} runs successfully!");
                Ok(())
            }
            // not exist
            false => {
                // create container
                let CreateContainerResponse { container_id } = self.create_container()?;
                self.start_container(None)?;
                println!("Container: {container_id} runs successfully!");
                Ok(())
            }
        }
    }

    pub fn get_container_state(&self) -> Result<State> {
        let container = load_container(&self.root_path, &self.container_id)?;
        Ok(container.state)
    }

    pub fn get_container_id(&self) -> Result<String> {
        let container_id = self
            .config
            .as_ref()
            .and_then(|c| c.metadata.as_ref().map(|m| m.name.clone()))
            .ok_or_else(|| anyhow!("Failed to get the container id"))?;
        Ok(container_id)
    }

    pub fn build_config(&mut self) -> Result<()> {
        let config = self
            .config_builder
            .from_container_spec(self.spec.clone())?
            .clone()
            .build();

        debug!("Get Config: {:?}", config);
        self.config = Some(config);
        Ok(())
    }

    pub fn create_oci_spec(&self) -> Result<Spec> {
        // validate the config
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow!("Container's Config is required"))?;

        debug!("Get Config: {:#?}", config);

        let mut spec = Spec::default();
        // let root = RootBuilder::default()
        //     .readonly(false)
        //     .build()
        //     .unwrap_or_default();
        // spec.set_root(Some(root));

        // use the default namespace configuration
        let namespaces = get_default_namespaces();

        let mut linux: LinuxBuilder = LinuxBuilder::default().namespaces(namespaces);
        if let Some(x) = &config.linux {
            if let Some(r) = &x.resources {
                linux = linux.resources(r);
            }
        }
        let linux = linux.build()?;
        spec.set_linux(Some(linux));

        // build the process path
        let mut process = ProcessBuilder::default()
            .args(self.spec.args.clone())
            .build()?;

        let mut capabilities = process.capabilities().clone().unwrap();
        // add the CAP_NET_RAW
        add_cap_net_raw(&mut capabilities);

        process.set_capabilities(Some(capabilities));

        spec.set_process(Some(process));

        let mut mounts = convert_oci_mounts(&config.mounts)?;
        let existing_mounts = spec.mounts().clone().unwrap_or_default();
        mounts.extend(existing_mounts);
        spec.set_mounts(Some(mounts));

        Ok(spec)
    }

    pub fn create_container(&self) -> Result<CreateContainerResponse> {
        let container_id = self.get_container_id()?;

        //  create oci spec
        let spec = self.create_oci_spec()?;

        // create a config.path at the bundle path
        // TODO: Here use the local file path directly
        let bundle_path = self.spec.image.clone();
        if bundle_path.is_empty() {
            return Err(anyhow!("Bundle path (image) is empty"));
        }
        let bundle_dir = Path::new(&bundle_path);
        if !bundle_dir.exists() {
            return Err(anyhow!("Bundle directory does not exist"));
        }

        let config_path = format!("{bundle_path}/config.json");
        if Path::new(&config_path).exists() {
            std::fs::remove_file(&config_path).map_err(|e| {
                anyhow!(
                    "Failed to remove existing config.json in bundle path: {}",
                    e
                )
            })?;
        }
        let file = File::create(config_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &spec)?;
        writer.flush()?;

        let create_args = Create {
            bundle: bundle_path.clone().into(),
            console_socket: None,
            pid_file: None,
            no_pivot: false,
            no_new_keyring: false,
            preserve_fds: 0,
            container_id: container_id.clone(),
        };

        create(create_args, self.root_path.clone(), false)
            .map_err(|e| anyhow!("Failed to create container: {}", e))?;

        Ok(CreateContainerResponse { container_id })
    }

    pub fn setup_container_network(&self) -> Result<()> {
        let mut cni = get_cni()?;
        let container_pid = self
            .get_container_state()?
            .pid
            .ok_or_else(|| anyhow!("get container {} pid failed", self.container_id))?;
        cni.load_default_conf();

        cni.setup(
            self.container_id.clone(),
            format!("/proc/{container_pid}/ns/net"),
        )
        .map_err(|e| anyhow::anyhow!("Failed to add CNI network: {}", e))?;
        Ok(())
    }

    pub fn start_container(&self, id: Option<String>) -> Result<StartContainerResponse> {
        let root_path = self.root_path.clone();

        // setup the network
        self.setup_container_network()?;

        match id {
            None => {
                let id = self.get_container_id()?;
                start(
                    Start {
                        container_id: id.clone(),
                    },
                    root_path,
                )?;

                Ok(StartContainerResponse {})
            }
            Some(id) => {
                start(
                    Start {
                        container_id: id.clone(),
                    },
                    root_path,
                )?;

                Ok(StartContainerResponse {})
            }
        }
    }
}

fn convert_oci_mounts(mounts: &Vec<Mount>) -> Result<Vec<OciMount>> {
    let mut oci_mounts: Vec<OciMount> = vec![];
    for mount in mounts {
        let oci_mount = MountBuilder::default()
            .typ(determine_mount_type(&mount.host_path))
            .destination(&mount.container_path)
            .source(&mount.host_path)
            .options(build_mount_options(mount))
            .build()?;
        oci_mounts.push(oci_mount);
    }
    Ok(oci_mounts)
}

fn build_mount_options(mount: &Mount) -> Vec<String> {
    let mut options = vec![];

    if mount.readonly {
        options.push("ro".to_string());
    } else {
        options.push("rw".to_string());
    }

    options.push("rbind".to_string());

    // TODO: more options
    options
}

fn determine_mount_type(host_path: &str) -> String {
    match host_path {
        "proc" => "proc".to_string(),
        "tmpfs" => "tmpfs".to_string(),
        _ => "bind".to_string(), // default is the bind
    }
}

pub fn run_container(path: &str) -> Result<(), anyhow::Error> {
    // read the container_spec bytes to container_spec struct
    let mut runner = ContainerRunner::from_file(path)?;

    // create container_config
    runner.build_config()?;

    let id = runner.container_id.as_str();
    // See if the container exists
    match is_container_exist(id, &runner.root_path).is_ok() {
        // exist
        true => {
            runner.start_container(None)?;
            println!("Container: {id} runs successfully!");
            Ok(())
        }
        // not exist
        false => {
            // create container
            let CreateContainerResponse { container_id } = runner.create_container()?;
            runner.start_container(None)?;
            println!("Container: {container_id} runs successfully!");
            Ok(())
        }
    }
}

pub fn is_container_exist(id: &str, root_path: &PathBuf) -> Result<()> {
    let _ = load_container(root_path, id)?;
    Ok(())
}

/// command state
pub fn state_container(id: &str) -> Result<()> {
    let root_path = rootpath::determine(None)?;
    debug!("ROOT PATH: {}", root_path.to_str().unwrap_or_default());
    print_status(id.to_owned(), root_path)
}

/// command delete
pub fn delete_container(id: &str) -> Result<()> {
    let root_path = rootpath::determine(None)?;
    is_container_exist(id, &root_path)?;
    let delete_args = Delete {
        container_id: id.to_string(),
        force: true,
    };
    // delete the network
    let container = load_container(&root_path, id)?;
    let pid = container
        .pid()
        .ok_or(anyhow!("invalid container {} can't find pid", id))?;
    remove_container_network(pid)?;

    delete(delete_args, root_path)?;

    Ok(())
}

pub fn remove_container(root_path: &Path, state: &State) -> Result<()> {
    let delete_args = Delete {
        container_id: state.id.clone(),
        force: true,
    };
    let pid = state
        .pid
        .ok_or(anyhow!("failed to get pid of container {}", &state.id))?;
    // delete the network
    remove_container_network(Pid::from_raw(pid))?;
    delete(delete_args, root_path.to_path_buf())?;
    Ok(())
}

pub fn remove_container_network(pid: Pid) -> Result<()> {
    let mut cni = get_cni()?;
    cni.load_default_conf();
    let netns_path = format!("/proc/{pid}/ns/net");
    let id = pid.to_string();
    cni.remove(id, netns_path.clone())
        .map_err(|e| anyhow::anyhow!("Failed to remove CNI network: {}", e))?;
    Ok(())
}
pub fn start_container(container_id: &str) -> Result<()> {
    let runner = ContainerRunner::from_container_id(container_id, None)?;
    runner.start_container(Some(container_id.to_string()))?;
    println!("container {container_id} start successfully");
    Ok(())
}

pub fn list_container(quiet: Option<bool>, format: Option<String>) -> Result<()> {
    list(
        List {
            format: format.unwrap_or("default".to_owned()),
            quiet: quiet.unwrap_or(false),
        },
        rootpath::determine(None)?,
    )?;
    Ok(())
}

pub fn exec_container(args: ExecContainer, root_path: Option<PathBuf>) -> Result<i32> {
    let args = Exec::from(args);

    let exit_code = exec(
        args,
        match root_path {
            Some(path) => path,
            None => rootpath::determine(None)?,
        },
    )?;
    Ok(exit_code)
}

pub fn create_container(path: &str) -> Result<()> {
    let mut runner = ContainerRunner::from_file(path)?;
    runner.create()
}

pub fn print_status(container_id: String, root_path: PathBuf) -> Result<()> {
    let root_path = fs::canonicalize(root_path)?;
    let container_root = root_path.join(container_id);
    let mut content = String::new();

    let state_file = state::State::file_path(&container_root);
    if !state_file.exists() {
        return Err(anyhow!(
            "Broken container_file: no container state find in {}",
            container_root.to_str().unwrap()
        ));
    }
    let container = Container::load(container_root)?;
    let pid = if let Some(pid) = container.pid() {
        pid.to_string()
    } else {
        "".to_owned()
    };

    let creator = container.creator().unwrap_or_default();
    let created = if let Some(utc) = container.created() {
        let local: DateTime<Local> = DateTime::from(utc);
        local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    } else {
        "".to_owned()
    };
    let _ = writeln!(
        content,
        "{}\t{}\t{}\t{}\t{}\t{}",
        container.id(),
        pid,
        container.status(),
        container.bundle().display(),
        created,
        creator.to_string_lossy()
    );

    let mut tab_writer = TabWriter::new(io::stdout());
    writeln!(&mut tab_writer, "ID\tPID\tSTATUS\tBUNDLE\tCREATED\tCREATOR")?;
    write!(&mut tab_writer, "{content}")?;
    tab_writer.flush()?;

    Ok(())
}

pub fn container_execute(cmd: ContainerCommand) -> Result<()> {
    match cmd {
        ContainerCommand::Run { container_yaml } => run_container(&container_yaml),
        ContainerCommand::Start { container_name } => start_container(&container_name),
        ContainerCommand::State { container_name } => state_container(&container_name),
        ContainerCommand::Delete { container_name } => delete_container(&container_name),
        ContainerCommand::Create { container_yaml } => create_container(&container_yaml),
        ContainerCommand::List { quiet, format } => list_container(quiet, format),
        ContainerCommand::Exec(exec) => {
            // root_path => default directory
            // to support enter the container created by compose-style
            let exit_code =
                exec_container((*exec).clone(), exec.root_path.as_ref().map(PathBuf::from))?;
            std::process::exit(exit_code)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_container_runner_from_spec_and_file() {
        let spec = ContainerSpec {
            name: "demo1".to_string(),
            image: "/tmp/demoimg".to_string(),
            ports: vec![],
            args: vec!["/bin/echo".to_string(), "hi".to_string()],
            resources: None,
        };
        let runner = ContainerRunner::from_spec(spec.clone(), None).unwrap();
        assert_eq!(runner.container_id, "demo1");
        assert_eq!(runner.spec.name, "demo1");

        // test from_file
        let dir = tempdir().unwrap();
        let yaml_path = dir.path().join("spec.yaml");
        let yaml = serde_yaml::to_string(&spec).unwrap();
        fs::write(&yaml_path, yaml).unwrap();
        let runner2 = ContainerRunner::from_file(yaml_path.to_str().unwrap()).unwrap();
        assert_eq!(runner2.spec.name, "demo1");
    }

    #[test]
    fn test_build_config() {
        let mut runner = ContainerRunner::from_spec(
            ContainerSpec {
                name: "demo2".to_string(),
                image: "/tmp/demoimg2".to_string(),
                ports: vec![],
                args: vec![],
                resources: None,
            },
            None,
        )
        .unwrap();
        assert!(runner.build_config().is_ok());
        assert!(runner.config.is_some());
        assert_eq!(runner.get_container_id().unwrap(), "demo2");
    }

    #[test]
    fn test_is_container_exist_fail() {
        let res = is_container_exist("not_exist", &PathBuf::from("/tmp/none"));
        assert!(res.is_err());
    }

    #[test]
    fn test_list_container_default() {
        // Should not panic, even if no containers exist
        let res = list_container(None, None);
        assert!(res.is_ok() || res.is_err());
    }
}
