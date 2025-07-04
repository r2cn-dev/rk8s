use std::path::PathBuf;

use anyhow::Result;
use libcontainer::container::builder::ContainerBuilder;
use libcontainer::syscall::syscall::SyscallType;
use nix::sys::wait::{WaitStatus, waitpid};

#[derive(Debug, Clone)]
pub struct Exec {
    pub pod_name: Option<String>,
    pub container_id: String,
    pub command: Vec<String>,
    pub console_socket: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub tty: bool,
    pub user: Option<(u32, Option<u32>)>,
    pub additional_gids: Vec<u32>,
    pub process: Option<PathBuf>,
    pub detach: bool,
    pub pid_file: Option<PathBuf>,
    pub process_label: Option<String>,
    pub apparmor: Option<String>,
    pub no_new_privs: bool,
    pub cap: Vec<String>,
    pub preserve_fds: i32,
    pub ignore_paused: bool,
    pub cgroup: Option<String>,
}

pub fn exec(args: Exec, root_path: PathBuf) -> Result<i32> {
    let pid = ContainerBuilder::new(args.container_id.clone(), SyscallType::default())
        .with_executor(libcontainer::workload::default::DefaultExecutor {})
        .with_root_path(root_path)?
        .with_console_socket(args.console_socket.as_ref())
        .with_pid_file(args.pid_file.as_ref())?
        .validate_id()?
        .as_tenant()
        .with_detach(args.detach)
        .with_cwd(args.cwd.as_ref())
        .with_env(args.env.clone().into_iter().collect())
        .with_process(args.process.as_ref())
        .with_no_new_privs(args.no_new_privs)
        .with_container_args(args.command.clone())
        .build()?;

    // See https://github.com/containers/youki/pull/1252 for a detailed explanation
    // basically, if there is any error in starting exec, the build above will return error
    // however, if the process does start, and detach is given, we do not wait for it
    // if not detached, then we wait for it using waitpid below
    if args.detach {
        return Ok(0);
    }

    match waitpid(pid, None)? {
        WaitStatus::Exited(_, status) => Ok(status),
        WaitStatus::Signaled(_, sig, _) => Ok(sig as i32),
        _ => Ok(0),
    }
}
