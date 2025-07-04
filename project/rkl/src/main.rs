use clap::{Parser, Subcommand};
use rkl::{
    ComposeCommand, ContainerCommand, PodCommand,
    cli_commands::{compose_execute, container_execute, pod_execute},
};

#[derive(Parser)]
#[command(name = "rkl")]
#[command(
    about = "A simple container runtime", 
    long_about = None,
    override_usage = "rkl <workload> <command> [OPTIONS]",
)]
struct Cli {
    #[command(subcommand)]
    workload: Workload,
}

impl Cli {
    pub fn run(self) -> Result<(), anyhow::Error> {
        match self.workload {
            Workload::Pod(cmd) => pod_execute(cmd),
            Workload::Container(cmd) => container_execute(cmd),
            Workload::Compose(cmd) => compose_execute(cmd),
        }
    }
}

/// define the 3 state for the run command "container" "pod" "compose"
#[derive(Subcommand)]
enum Workload {
    #[command(subcommand, about = "Operations related to pods", alias = "p")]
    Pod(PodCommand),

    #[command(subcommand, about = "Manage standalone containers", alias = "c")]
    Container(ContainerCommand),

    #[command(
        subcommand,
        about = "Manage multi-container apps using Compose",
        alias = "C"
    )]
    Compose(ComposeCommand),
}

fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    cli.run()
}
