use clap::{Parser, Subcommand};
use rkl::{
    ComposeCommand, ContainerCommand, PodCommand,
    commands::{compose::compose_execute, container::container_execute, pod::pod_execute},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

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
        about = "Manage multi-container apps using compose",
        alias = "C"
    )]
    Compose(ComposeCommand),
}

fn main() -> Result<(), anyhow::Error> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let cli = Cli::parse();
    cli.run()
        .inspect_err(|err| eprintln!("Failed to run: {err}"))
}
