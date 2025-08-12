use clap::{Args, Subcommand};

pub mod bundle;
pub mod commands;
mod cri;
pub mod daemon;
mod rootpath;
pub mod task;

#[derive(Subcommand)]
pub enum ComposeCommand {
    #[command(about = "Start a compose application from a compose yaml")]
    Up(UpArgs),

    #[command(about = "stop and delete all the containers in the compose application")]
    Down(DownArgs),

    #[command(about = "List all the containers' state in compose application")]
    Ps(PsArgs),
}

#[derive(Args)]
pub struct PsArgs {
    // /// specify the compose application's name, default is the cwd
    #[arg(long = "project-name", short, value_name = "PROJECT_NAME")]
    pub project_name: Option<String>,
    //  specify the target compose_yml path
    #[arg(short = 'f', value_name = "COMPOSE_YAML")]
    pub compose_yaml: Option<String>,
}

#[derive(Args)]
pub struct DownArgs {
    /// specify the compose application's name, default is the cwd
    #[arg(long = "project-name", short, value_name = "PROJECT_NAME")]
    pub project_name: Option<String>,

    /// specify the compose application's name, default is the cwd
    #[arg(short = 'f', value_name = "COMPOSE_YAML")]
    pub compose_yaml: Option<String>,
}

#[derive(Args)]
pub struct UpArgs {
    /// the compose.yaml's path, default under the cwd dir
    #[arg(value_name = "COMPOSE_YAML")]
    pub compose_yaml: Option<String>,

    /// specify the compose application's name, default is the cwd
    #[arg(long = "project-name", value_name = "PROJECT_NAME")]
    pub project_name: Option<String>,
}

#[derive(Subcommand)]
pub enum ContainerCommand {
    #[command(about = "Run a single container from a YAML file using rkl run container.yaml")]
    Run {
        #[arg(value_name = "CONTAINER_YAML")]
        container_yaml: String,
    },
    #[command(about = "Create a Container from a YAML file using rkl create container.yaml")]
    Create {
        #[arg(value_name = "CONTAINER_YAML")]
        container_yaml: String,
    },
    #[command(about = "Start a Container with a Container-name using rkl start container-name")]
    Start {
        #[arg(value_name = "CONTAINER_NAME")]
        container_name: String,
    },

    #[command(about = "Delete a Container with a Container-name using rkl delete container-name")]
    Delete {
        #[arg(value_name = "CONTAINER_NAME")]
        container_name: String,
    },
    #[command(about = "Get the state of a container using rkl state container-name")]
    State {
        #[arg(value_name = "CONTAINER_NAME")]
        container_name: String,
    },

    #[command(about = "List the current running container")]
    List {
        /// Only display container IDs default is false
        #[arg(long, short)]
        quiet: Option<bool>,

        /// Specify the format (default or table)
        #[arg(long, short)]
        format: Option<String>,
    },

    Exec(Box<commands::ExecContainer>),
}

#[derive(Subcommand)]
pub enum PodCommand {
    #[command(about = "Run a pod from a YAML file using rkl run pod.yaml")]
    Run {
        #[arg(value_name = "POD_YAML")]
        pod_yaml: String,
    },
    #[command(about = "Create a pod from a YAML file using rkl create pod.yaml")]
    Create {
        #[arg(value_name = "POD_YAML")]
        pod_yaml: String,
    },
    #[command(about = "Start a pod with a pod-name using rkl start pod-name")]
    Start {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },

    #[command(about = "Delete a pod with a pod-name using rkl delete pod-name")]
    Delete {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },
    #[command(about = "Get the state of a pod using rkl state pod-name")]
    State {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },
    Exec(Box<commands::ExecPod>),
    // Run as a daemon process.
    // For convenient, I won't remove cli part now.
    #[command(
        about = "Set rkl on daemon mod monitoring the pod.yaml in '/etc/rk8s/manifests' directory"
    )]
    Daemon,
}
