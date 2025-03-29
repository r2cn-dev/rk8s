mod task; 
mod cri;
mod rootpath;
mod commands;
mod cli_commands;
use task::task::TaskRunner; 
use std::error::Error;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rkl")]
#[command(about = "A simple container runtime", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        #[arg(value_name = "POD_YAML")]
        pod_yaml: String,
    },
    Create {
        #[arg(value_name = "POD_YAML")]
        pod_yaml: String,
    },
    Start {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },
    Delete {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },
    State {
        #[arg(value_name = "POD_NAME")]
        pod_name: String,
    },
}

fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    match cli.command {
        //./rkl run xxx.yaml
        //./rkl create xxx.yaml
        //./rkl start podname
        //./rkl delete podname
        //./rkl state podname

        Commands::Run { pod_yaml } => cli_commands::run_pod(&pod_yaml),
        Commands::Create { pod_yaml } => cli_commands::create_pod(&pod_yaml),
        Commands::Start { pod_name } => cli_commands::start_pod(&pod_name),
        Commands::Delete { pod_name } => cli_commands::delete_pod(&pod_name),
        Commands::State { pod_name } => cli_commands::state_pod(&pod_name),
    }
}