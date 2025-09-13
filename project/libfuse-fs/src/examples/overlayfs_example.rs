// Copyright (C) 2024 rk8s authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Example binary to mount overlay filesystem implemented by libfuse-fs.
// This is used by integration tests to exercise overlayfs with fio & IOR.

use clap::Parser;
use libfuse_fs::overlayfs::mount_fs;
use tokio::signal;

#[derive(Parser, Debug)]
#[command(author, version, about = "OverlayFS example for integration tests")] 
struct Args {
    /// Mount point path
    #[arg(long)]
    mountpoint: String,
    /// Upper writable layer directory
    #[arg(long)]
    upperdir: String,
    /// Lower read-only layer directories (can appear multiple times)
    #[arg(long)]
    lowerdir: Vec<String>,
    /// Use privileged mount instead of unprivileged (default false)
    #[arg(long, default_value_t = true)]
    not_unprivileged: bool,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();

    // Start mount (returns a future that resolves when unmounted)
    let mut mount_handle = mount_fs(
        args.mountpoint,
        args.upperdir,
        args.lowerdir,
        args.not_unprivileged,
    )
    .await;

    let handle = &mut mount_handle;

    // Wait for either filesystem termination or CTRL+C
    tokio::select! {
        res = handle => res.unwrap(),
        _ = signal::ctrl_c() => {
            mount_handle.unmount().await.unwrap();
        }
    }
}
