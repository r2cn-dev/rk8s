#![allow(dead_code)]
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::str;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use common::lease::{Event, EventType, Lease, LeaseAttrs};
use ipnetwork::{Ipv4Network, Ipv6Network};
use lazy_static::lazy_static;
use log::{error, info};
use regex::Regex;
use tokio::sync::mpsc::{self, Sender};

use crate::network::config::Config;
use crate::network::lease::{LeaseWatchResult, LeaseWatcher};

lazy_static! {
    static ref SUBNET_REGEX: Regex =
        Regex::new(r"(\d+\.\d+\.\d+\.\d+)-(\d+)(?:&([a-f\d:]+)-(\d+))?").unwrap();
}

pub fn parse_subnet_key(s: &str) -> Option<(Ipv4Network, Option<Ipv6Network>)> {
    if let Some(caps) = SUBNET_REGEX.captures(s) {
        let ipv4: Ipv4Addr = caps[1].parse().ok()?;
        let ipv4_prefix: u8 = caps[2].parse().ok()?;
        let ipv4_net = Ipv4Network::new(ipv4, ipv4_prefix).ok()?;

        let ipv6_net = if let (Some(ipv6_str), Some(prefix_str)) = (caps.get(3), caps.get(4)) {
            let ipv6: Ipv6Addr = ipv6_str.as_str().parse().ok()?;
            let prefix: u8 = prefix_str.as_str().parse().ok()?;
            Some(Ipv6Network::new(ipv6, prefix).ok()?)
        } else {
            None
        };

        Some((ipv4_net, ipv6_net))
    } else {
        None
    }
}

pub fn make_subnet_key(sn4: &Ipv4Network, sn6: Option<&Ipv6Network>) -> String {
    match sn6 {
        Some(v6) => format!(
            "{}&{}",
            sn4.to_string().replace("/", "-"),
            v6.to_string().replace("/", "-")
        ),
        None => sn4.to_string().replace("/", "-"),
    }
}

pub fn write_subnet_file<P: AsRef<Path>>(
    path: P,
    config: &Config,
    ip_masq: bool,
    mut sn4: Option<Ipv4Network>,
    mut sn6: Option<Ipv6Network>,
    mtu: u32,
) -> Result<()> {
    let path = path.as_ref();
    let (dir, name) = (
        path.parent().context("Missing parent directory")?,
        path.file_name().context("Missing file name")?,
    );
    fs::create_dir_all(dir)?;

    let temp_file = dir.join(format!(".{}", name.to_string_lossy()));
    let mut contents = String::new();

    if config.enable_ipv4
        && let Some(ref mut net) = sn4
    {
        contents += &format!("RKL_NETWORK={}\n", config.network.unwrap());
        contents += &format!("RKL_SUBNET={}/{}\n", net.ip(), net.prefix());
    }

    if config.enable_ipv6
        && let Some(ref mut net) = sn6
    {
        contents += &format!("RKL_IPV6_NETWORK={}\n", config.ipv6_network.unwrap());
        contents += &format!("RKL_IPV6_SUBNET={}/{}\n", net.ip(), net.prefix());
    }

    contents += &format!("RKL_MTU={mtu}\n");
    contents += &format!("RKL_IPMASQ={ip_masq}\n");

    fs::write(&temp_file, contents)?;
    fs::rename(&temp_file, path)?;

    Ok(())
}

#[async_trait]
pub trait Manager: Send + Sync {
    async fn get_network_config(&self) -> Result<Config>;

    async fn handle_subnet_file(
        &self,
        path: &str,
        config: &Config,
        ip_masq: bool,
        sn: Ipv4Network,
        sn6: Ipv6Network,
        mtu: i32,
    ) -> Result<()>;

    async fn acquire_lease(&self, attrs: &LeaseAttrs) -> Result<Lease>;

    async fn renew_lease(&self, lease: &Lease) -> Result<()>;

    async fn watch_lease(
        &self,
        sn: Ipv4Network,
        sn6: Ipv6Network,
        sender: mpsc::Sender<Vec<LeaseWatchResult>>,
    ) -> Result<()>;

    async fn watch_leases(&self, sender: mpsc::Sender<Vec<LeaseWatchResult>>) -> Result<()>;

    async fn complete_lease(&self, lease: &Lease) -> Result<()>;

    async fn get_stored_mac_addresses(&self) -> (String, String);

    async fn get_stored_public_ip(&self) -> (String, String);

    fn name(&self) -> String;
}

pub async fn watch_leases(
    sm: Arc<dyn Manager>,
    own_lease: Lease,
    receiver: Sender<Vec<Event>>,
) -> Result<()> {
    let mut lw = LeaseWatcher {
        own_lease,
        leases: vec![],
    };
    let (tx_watch, mut rx_watch) = mpsc::channel(100);

    // Spawn watcher task
    let sm_clone = sm.clone();
    tokio::spawn(async move {
        if let Err(e) = sm_clone.watch_leases(tx_watch).await {
            error!("could not watch leases: {e}");
        }
    });

    while let Some(watch_results) = rx_watch.recv().await {
        for wr in watch_results {
            let batch = if !wr.events.is_empty() {
                lw.update(wr.events)
            } else {
                lw.reset(wr.snapshot)
            };

            for (i, evt) in batch.iter().enumerate() {
                info!("Batch elem [{i}] is {evt:?}");
            }

            if !batch.is_empty() {
                let _ = receiver.send(batch).await;
            }
        }
    }
    Ok(())
}

pub async fn watch_lease(
    sm: Arc<dyn Manager>,
    sn: Ipv4Network,
    sn6: Ipv6Network,
    receiver: Sender<Event>,
) -> Result<()> {
    let (tx_watch, mut rx_watch) = mpsc::channel::<Vec<LeaseWatchResult>>(100);

    let sm_clone = sm.clone();

    // Spawn watcher task
    tokio::spawn(async move {
        match sm_clone.watch_lease(sn, sn6, tx_watch).await {
            Err(e) if e.to_string().contains("cancelled") => {
                info!("Context cancelled, closing receiver channel");
            }
            Err(e) => {
                error!("Subnet watch failed: {e}");
            }
            Ok(_) => {}
        }
    });

    while let Some(watch_results) = rx_watch.recv().await {
        for wr in watch_results {
            if let Some(lease) = wr.snapshot.first() {
                let event_added = Event {
                    event_type: EventType::Added,
                    lease: Some(lease.clone()),
                };
                let _ = receiver.send(event_added).await;
            } else if let Some(event) = wr.events.first() {
                let _ = receiver.send(event.clone()).await;
            } else {
                info!("WatchLease: empty event received");
            }
        }
    }

    info!("leaseWatchChan channel closed");
    Ok(())
}
