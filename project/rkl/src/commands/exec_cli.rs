use std::error::Error;
use std::path::PathBuf;

use clap::Parser;

use crate::commands::exec::Exec;

/// Execute a process within an existing container
/// Reference: https://github.com/opencontainers/runc/blob/main/man/runc-exec.8.md
#[derive(Parser, Debug, Clone)]
pub struct ExecBase {
    // /// Pod name
    // #[arg(value_name = "POD_NAME")]
    // #[clap(required = true)]
    // pub pod_name: String,
    /// Unix socket (file) path , which will receive file descriptor of the writing end of the pseudoterminal
    #[clap(long)]
    pub console_socket: Option<PathBuf>,
    #[clap(long)]
    /// Current working directory of the container
    pub cwd: Option<PathBuf>,
    /// Environment variables that should be set in the container
    #[clap(short, long, value_parser = parse_env::<String, String>, number_of_values = 1)]
    pub env: Vec<(String, String)>,
    #[clap(short, long)]
    pub tty: bool,
    /// Run the command as a user
    #[clap(short, long, value_parser = parse_user::<u32, u32>)]
    pub user: Option<(u32, Option<u32>)>,
    /// Add additional group IDs. Can be specified multiple times
    #[clap(long, short = 'g', number_of_values = 1)]
    pub additional_gids: Vec<u32>,
    /// Path to process.json
    #[clap(short, long)]
    pub process: Option<PathBuf>,
    /// Detach from the container process
    #[clap(short, long)]
    pub detach: bool,
    #[clap(long)]
    /// The file to which the pid of the container process should be written to
    pub pid_file: Option<PathBuf>,
    /// Set the asm process label for the process commonly used with selinux
    #[clap(long)]
    pub process_label: Option<String>,
    /// Set the apparmor profile for the process
    #[clap(long)]
    pub apparmor: Option<String>,
    /// Prevent the process from gaining additional privileges
    #[clap(long)]
    pub no_new_privs: bool,
    /// Add a capability to the bounding set for the process
    #[clap(long, number_of_values = 1)]
    pub cap: Vec<String>,
    /// Pass N additional file descriptors to the container
    #[clap(long, default_value = "0")]
    pub preserve_fds: i32,
    /// Allow exec in a paused container
    #[clap(long)]
    pub ignore_paused: bool,
    /// Execute a process in a sub-cgroup
    #[clap(long)]
    pub cgroup: Option<String>,
    // /// Identifier of the container
    // #[clap(value_parser = clap::builder::NonEmptyStringValueParser::new(), required = true)]
    // pub container_id: String,

    // /// Command that should be executed in the container
    // #[clap(required = false)]
    // pub command: Vec<String>,
}

#[derive(Parser, Debug, Clone)]
pub struct ExecContainer {
    #[arg(value_name = "CONTAINER_ID")]
    #[clap(value_parser = clap::builder::NonEmptyStringValueParser::new(), required = true)]
    pub container_id: String,

    /// Command that should be executed in the container
    #[clap(required = false)]
    pub command: Vec<String>,

    #[clap(flatten)]
    pub base: ExecBase,
}

#[derive(Parser, Debug, Clone)]
pub struct ExecPod {
    /// Pod name
    #[arg(value_name = "POD_NAME")]
    #[clap(required = true)]
    pub pod_name: String,

    #[arg(value_name = "CONTAINER_ID")]
    #[clap(value_parser = clap::builder::NonEmptyStringValueParser::new(), required = true)]
    pub container_id: String,

    /// Command that should be executed in the container
    #[clap(required = false)]
    pub command: Vec<String>,

    #[clap(flatten)]
    pub base: ExecBase,
}

// Support both pod and container
impl From<ExecPod> for Exec {
    fn from(cli: ExecPod) -> Self {
        Exec {
            pod_name: Some(cli.pod_name),
            container_id: cli.container_id,
            command: cli.command,
            console_socket: cli.base.console_socket,
            cwd: cli.base.cwd,
            env: cli.base.env,
            tty: cli.base.tty,
            user: cli.base.user,
            additional_gids: cli.base.additional_gids,
            process: cli.base.process,
            detach: cli.base.detach,
            pid_file: cli.base.pid_file,
            process_label: cli.base.process_label,
            apparmor: cli.base.apparmor,
            no_new_privs: cli.base.no_new_privs,
            cap: cli.base.cap,
            preserve_fds: cli.base.preserve_fds,
            ignore_paused: cli.base.ignore_paused,
            cgroup: cli.base.cgroup,
        }
    }
}

impl From<ExecContainer> for Exec {
    fn from(cli: ExecContainer) -> Self {
        Exec {
            pod_name: None,
            container_id: cli.container_id,
            command: cli.command,
            console_socket: cli.base.console_socket,
            cwd: cli.base.cwd,
            env: cli.base.env,
            tty: cli.base.tty,
            user: cli.base.user,
            additional_gids: cli.base.additional_gids,
            process: cli.base.process,
            detach: cli.base.detach,
            pid_file: cli.base.pid_file,
            process_label: cli.base.process_label,
            apparmor: cli.base.apparmor,
            no_new_privs: cli.base.no_new_privs,
            cap: cli.base.cap,
            preserve_fds: cli.base.preserve_fds,
            ignore_paused: cli.base.ignore_paused,
            cgroup: cli.base.cgroup,
        }
    }
}

fn parse_env<T, U>(s: &str) -> Result<(T, U), Box<dyn Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid VAR=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

fn parse_user<T, U>(s: &str) -> Result<(T, Option<U>), Box<dyn Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: Error + Send + Sync + 'static,
{
    if let Some(pos) = s.find(':') {
        Ok((s[..pos].parse()?, Some(s[pos + 1..].parse()?)))
    } else {
        Ok((s.parse()?, None))
    }
}
