// Copyright (C) 2024 rk8s authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Simple passthrough filesystem example (1:1 mapping to a host directory).
// Used by integration tests (fio & IOR) to validate basic operations.

use clap::Parser;
use libfuse_fs::passthrough::new_passthroughfs_layer;
use rfuse3::{MountOptions, raw::Session};
use tokio::signal;
use std::ffi::OsString;

#[derive(Parser, Debug)]
#[command(author, version, about = "Passthrough FS example for integration tests")] 
struct Args {
    /// Path to mount point
    #[arg(long)]
    mountpoint: String,
    /// Source directory to expose
    #[arg(long)]
    rootdir: String,
    /// Use privileged mount instead of unprivileged (default false)
    #[arg(long, default_value_t = true)]
    not_unprivileged: bool,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();

    let fs = new_passthroughfs_layer(&args.rootdir)
        .await
        .expect("Failed to init passthrough fs");

    let mount_path = OsString::from(&args.mountpoint);
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut mount_options = MountOptions::default();
    // Keep options minimal for CI (avoid allow_other requirement)
    mount_options.force_readdir_plus(true).uid(uid).gid(gid);

    let mut mount_handle = if !args.not_unprivileged {
        println!("Mounting passthrough (unprivileged)");
        Session::new(mount_options)
            .mount_with_unprivileged(fs, mount_path)
            .await
            .expect("Unprivileged mount failed")
    } else {
        println!("Mounting passthrough (privileged)");
        Session::new(mount_options)
            .mount(fs, mount_path)
            .await
            .expect("Privileged mount failed")
    };

    let handle = &mut mount_handle;
    tokio::select! {
        res = handle => res.unwrap(),
        _ = signal::ctrl_c() => {
            mount_handle.unmount().await.unwrap();
        }
    }
}
