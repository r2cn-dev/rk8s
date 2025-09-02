#![allow(dead_code)]
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use ipnetwork::{Ipv4Network, Ipv6Network};
use log::{error, info, warn};
use rand::prelude::IndexedRandom;
use rand::rngs::ThreadRng;
use serde::{Deserialize, Serialize};
use std::time::Duration as StdDuration;
use std::{
    net::{Ipv4Addr, Ipv6Addr},
    sync::Arc,
};
use tokio::{
    sync::{
        Mutex, Notify,
        mpsc::{self, Sender},
    },
    time::{Duration as TokioDuration, sleep},
};
use tonic::Code;

use crate::network::{
    config::{self, Config},
    ip::{next_ipv4_network, next_ipv6_network},
    lease::{EventType, Lease, LeaseAttrs, LeaseWatchResult},
    registry::{Registry, XlineRegistryError},
    subnet,
};

const RACE_RETRIES: usize = 10;
const SUBNET_TTL: Duration = Duration::seconds(86400);
const LEASE_RETRY_SLEEP_SECS: u64 = 5;

#[derive(Clone)]
pub struct LocalManager {
    registry: Arc<dyn Registry + Send + Sync>,
    previous_subnet: Option<Ipv4Network>,
    previous_subnet_ipv6: Option<Ipv6Network>,
    renew_margin_secs: i64,
}

impl LocalManager {
    pub fn new(
        registry: Arc<dyn Registry + Send + Sync>,
        previous_subnet: Option<Ipv4Network>,
        previous_subnet_ipv6: Option<Ipv6Network>,
        renew_margin_secs: i64,
    ) -> Self {
        Self {
            registry,
            previous_subnet,
            previous_subnet_ipv6,
            renew_margin_secs,
        }
    }

    pub async fn get_network_config(&self) -> Result<Config> {
        let raw = self.registry.get_network_config().await?;
        let mut config = config::parse_config(&raw)?;
        config::check_network_config(&mut config)?;
        Ok(config)
    }

    pub async fn acquire_lease(&self, attrs: &LeaseAttrs) -> Result<Lease> {
        let config = self.get_network_config().await?;
        for _ in 0..RACE_RETRIES {
            match self
                .try_acquire_lease(&config, attrs.public_ip, attrs)
                .await
            {
                Ok(l) => return Ok(l),
                Err(e) if matches!(e.downcast_ref(), Some(XlineRegistryError::TryAgain)) => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(anyhow!("max retries reached trying to acquire a subnet"))
    }

    pub async fn try_acquire_lease(
        &self,
        config: &Config,
        ext_ip: Ipv4Addr,
        attrs: &LeaseAttrs,
    ) -> Result<Lease> {
        let (leases, _) = { self.registry.get_subnets().await? };

        if let Some(mut l) = find_lease_by_ip(&leases, ext_ip) {
            if is_subnet_config_compat(config, Some(l.subnet))
                && is_ipv6_subnet_config_compat(config, l.ipv6_subnet)
            {
                info!(
                    "Found lease (ip: {} ipv6: {:?}) for current IP ({}), reusing",
                    l.subnet, l.ipv6_subnet, ext_ip
                );

                let ttl = if l.expiration == DateTime::<Utc>::default() {
                    Duration::zero()
                } else {
                    SUBNET_TTL
                };

                let exp = {
                    self.registry
                        .update_subnet(l.subnet, l.ipv6_subnet, attrs, ttl, 0i64)
                        .await?
                };

                l.attrs = attrs.clone();
                l.expiration = exp;
                return Ok(l.clone());
            } else {
                info!(
                    "Found lease ({l:?}) for current IP ({ext_ip}) but not compatible with current config, deleting"
                );
                self.registry.delete_subnet(l.subnet, l.ipv6_subnet).await?;
            }
        }

        let mut sn: Option<Ipv4Network> = None;
        let mut sn6: Option<Ipv6Network> = None;

        if let Some(prev_subnet) = self.previous_subnet
            && find_lease_by_subnet(&leases, prev_subnet).is_none()
        {
            if is_subnet_config_compat(config, Some(prev_subnet))
                && is_ipv6_subnet_config_compat(config, self.previous_subnet_ipv6)
            {
                info!("Found previously leased subnet ({prev_subnet}), reusing");
                sn = Some(prev_subnet);
                sn6 = self.previous_subnet_ipv6;
            } else {
                error!(
                    "Found previously leased subnet ({prev_subnet}) that is not compatible with config, ignoring"
                );
            }
        }

        if sn.is_none() {
            let (alloc_sn, alloc_sn6) = self.allocate_subnet(config, &leases).await?;
            sn = Some(alloc_sn);
            sn6 = alloc_sn6;
        }
        let res = {
            self.registry
                .create_subnet(sn.unwrap(), sn6, attrs, SUBNET_TTL)
                .await
        };
        match res {
            Ok(exp) => {
                info!("Allocated lease (ip: {sn:?} ipv6: {sn6:?}) to current node ({ext_ip})");
                Ok(Lease {
                    enable_ipv4: true,
                    subnet: sn.unwrap(),
                    enable_ipv6: sn6.is_some(),
                    ipv6_subnet: sn6,
                    attrs: attrs.clone(),
                    expiration: exp,
                    asof: None,
                })
            }
            Err(e) if is_err_etcd_node_exist(&e) => Err(anyhow!("try again")),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn allocate_subnet(
        &self,
        config: &Config,
        leases: &[Lease],
    ) -> Result<(Ipv4Network, Option<Ipv6Network>)> {
        info!(
            "Picking subnet in range {:?} ... {:?}",
            config.subnet_min, config.subnet_max
        );
        if config.enable_ipv6 {
            info!(
                "Picking ipv6 subnet in range {:?} ... {:?}",
                config.ipv6_subnet_min, config.ipv6_subnet_max
            );
        }
        let mut available_v4 = Vec::new();
        let mut available_v6 = Vec::new();

        let start_ip = config
            .subnet_min
            .ok_or_else(|| anyhow!("Missing subnet_min"))?;
        let end_ip = config
            .subnet_max
            .ok_or_else(|| anyhow!("Missing subnet_max"))?;
        let prefix_len = config.subnet_len;

        let mut current = Ipv4Network::new(start_ip, prefix_len)
            .map_err(|e| anyhow!("Invalid subnet start: {}", e))?;

        while current.ip() <= end_ip && available_v4.len() < 100 {
            if !leases.iter().any(|l| l.subnet == current) {
                available_v4.push(current);
            }
            current = next_ipv4_network(current)?;
        }

        if config.enable_ipv6
            && let (Some(min_v6), Some(max_v6)) = (config.ipv6_subnet_min, config.ipv6_subnet_max)
        {
            let mut sn6 = Ipv6Network::new(min_v6, config.ipv6_subnet_len)
                .map_err(|e| anyhow!("Invalid IPv6 subnet start: {}", e))?;

            while sn6.ip() <= max_v6 && available_v6.len() < 100 {
                if !leases.iter().any(|l| l.ipv6_subnet == Some(sn6)) {
                    available_v6.push(sn6);
                }
                sn6 = next_ipv6_network(sn6)?;
            }
        }

        if available_v4.is_empty() || (config.enable_ipv6 && available_v6.is_empty()) {
            return Err(anyhow!("out of subnets"));
        }

        let mut rng: ThreadRng = rand::rng();
        let chosen_v4 = *available_v4.choose(&mut rng).unwrap();

        let chosen_v6 = if config.enable_ipv6 {
            Some(*available_v6.choose(&mut rng).unwrap())
        } else {
            None
        };

        Ok((chosen_v4, chosen_v6))
    }

    pub async fn renew_lease(&self, lease: &mut Lease) -> Result<()> {
        let expiration = self
            .registry
            .update_subnet(lease.subnet, lease.ipv6_subnet, &lease.attrs, SUBNET_TTL, 0)
            .await?;
        lease.expiration = expiration;
        Ok(())
    }

    pub async fn lease_watch_reset(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
    ) -> Result<LeaseWatchResult> {
        info!("sn:{sn:?}, sn6:{sn6:?}");
        let (lease_opt, index) = { self.registry.get_subnet(sn, sn6).await? };

        let lease = lease_opt.ok_or_else(|| anyhow::anyhow!("subnet not found"))?;

        Ok(LeaseWatchResult {
            snapshot: vec![lease],
            cursor: Cursor::Cursor(WatchCursor { index }),
            events: vec![],
        })
    }

    pub async fn watch_lease(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        sender: Sender<Vec<LeaseWatchResult>>,
    ) -> Result<()> {
        let wr = self.lease_watch_reset(sn, sn6).await?;
        log::info!("manager.watch_lease: sending reset results...");
        sender.send(vec![wr.clone()]).await?;
        let next_index = get_next_index(&wr.cursor)?;
        {
            self.registry
                .watch_subnet(next_index, sn, sn6, sender)
                .await?;
        }
        Ok(())
    }

    pub async fn watch_leases(&self, sender: Sender<Vec<LeaseWatchResult>>) -> Result<()> {
        info!("localmanager watch leases");
        let registry = self.registry.clone();
        let wr = { registry.leases_watch_reset().await? };
        sender.send(vec![wr.clone()]).await?;
        let next_index = get_next_index(&wr.cursor)?;
        let registry = self.registry.clone();
        tokio::spawn(async move {
            info!("spawn started");
            if let Err(e) = registry.watch_subnets(sender, next_index).await {
                error!("watch_subnets ended with error: {e}");
            }
        });
        Ok(())
    }

    pub async fn complete_lease(
        &self,
        my_lease: Arc<Mutex<Lease>>,
        cancel_notify: Arc<Notify>,
    ) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel(10);

        let lease_clone = my_lease.clone();
        let manager = self.clone();

        tokio::spawn(async move {
            loop {
                {
                    let lease = lease_clone.lock().await;
                    let subnet = lease.subnet;
                    let ipv6_subnet = lease.ipv6_subnet;

                    match manager.watch_lease(subnet, ipv6_subnet, tx.clone()).await {
                        Ok(_) => {
                            warn!("watch_lease returned normally, will retry...");
                        }
                        Err(e) => {
                            error!("watch_lease error: {e:?}, retrying...");
                        }
                    }
                }
                sleep(StdDuration::from_secs(LEASE_RETRY_SLEEP_SECS)).await;
            }
        });

        let renew_margin = Duration::minutes(self.renew_margin_secs);

        loop {
            let now = Utc::now();
            let lease_expiration = {
                let lease = my_lease.lock().await;
                lease.expiration
            };

            let mut dur = lease_expiration - now - renew_margin;
            if dur < Duration::zero() {
                dur = Duration::zero();
            }

            tokio::select! {
                _ = sleep(dur.to_std().unwrap_or(TokioDuration::from_secs(0))) => {
                    let mut lease = my_lease.lock().await;
                    if let Err(e) = self.renew_lease(&mut lease).await {
                        log::error!("Error renewing lease (retrying in 1 min): {e:?}");
                        drop(lease);
                        sleep(TokioDuration::from_secs(60)).await;
                        continue;
                    }
                    log::info!("Lease renewed, new expiration: {:?}", lease.expiration);
                }

                maybe_evt = rx.recv() => {
                    match maybe_evt {
                        Some(results) => {
                            for result in results {
                                for evt in result.events {
                                    match evt.event_type {
                                        EventType::Added => {
                                            if let Some(l) = evt.lease {
                                                let mut lease = my_lease.lock().await;
                                                lease.expiration = l.expiration;
                                                let dur = lease.expiration - Utc::now() - renew_margin;
                                                log::info!("Waiting for {dur:?} to renew lease");
                                            }
                                        }
                                        EventType::Removed => {
                                            log::error!("Lease has been revoked. Shutting down daemon.");
                                            return Err(anyhow::anyhow!("Lease revoked"));
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            log::info!("Stopped monitoring lease");
                            return Err(anyhow::anyhow!("Watch canceled"));
                        }
                    }
                }
                _ = cancel_notify.notified() => {
                    log::info!("CompleteLease received cancel signal, shutting down.");
                    return Err(anyhow::anyhow!("Canceled"));
                }
            }
        }
    }

    pub fn name(&self) -> String {
        let previous_subnet = match self.previous_subnet {
            Some(ref sn) => sn.to_string(),
            None => "None".to_string(),
        };
        format!("Etcd Local Manager with Previous Subnet: {previous_subnet}")
    }

    pub fn handle_subnet_file(
        &self,
        path: &str,
        config: &Config,
        ip_masq: bool,
        sn: Ipv4Network,
        ipv6sn: Option<Ipv6Network>,
        mtu: u32,
    ) -> anyhow::Result<()> {
        subnet::write_subnet_file(path, config, ip_masq, Some(sn), ipv6sn, mtu)
    }
}

#[derive(Serialize, Debug, Deserialize, Clone, Default)]
pub struct WatchCursor {
    pub index: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Cursor {
    Cursor(WatchCursor),
    Str(String),
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor::Str("0".to_string())
    }
}

pub fn get_next_index(cursor: &Cursor) -> Result<i64> {
    match cursor {
        Cursor::Cursor(wc) => Ok(wc.index + 1),
        Cursor::Str(s) => {
            let parsed = s
                .parse::<i64>()
                .with_context(|| format!("failed to parse cursor string: {s}"))?;
            Ok(parsed + 1)
        }
    }
}

pub fn is_index_too_small(err: &XlineRegistryError) -> bool {
    if let XlineRegistryError::Xline(boxed_err) = err
        && let etcd_client::Error::GRpcStatus(status) = &**boxed_err
    {
        return status.code() == Code::OutOfRange
            && status
                .message()
                .contains("required revision has been compacted");
    }
    false
}

pub fn find_lease_by_ip(leases: &[Lease], pub_ip: Ipv4Addr) -> Option<Lease> {
    leases.iter().find(|l| l.attrs.public_ip == pub_ip).cloned()
}

pub fn find_lease_by_subnet(leases: &[Lease], subnet: Ipv4Network) -> Option<Lease> {
    leases.iter().find(|l| l.subnet == subnet).cloned()
}

pub fn is_subnet_config_compat(config: &Config, sn: Option<Ipv4Network>) -> bool {
    let sn = match sn {
        Some(sn) => sn,
        None => return false,
    };

    let ip = sn.ip();

    match (&config.subnet_min, &config.subnet_max) {
        (Some(min), Some(max)) => {
            if ip < *min || ip > *max {
                return false;
            }
        }
        _ => return false,
    }

    sn.prefix() == config.subnet_len
}

pub fn is_ipv6_subnet_config_compat(config: &Config, sn6: Option<Ipv6Network>) -> bool {
    if !config.enable_ipv6 {
        return match sn6 {
            None => true,
            Some(sn6) => sn6.network() == Ipv6Addr::UNSPECIFIED && sn6.prefix() == 0,
        };
    }

    let sn6 = match sn6 {
        Some(sn6) => sn6,
        None => return false,
    };

    let ip = sn6.ip();

    match (&config.ipv6_subnet_min, &config.ipv6_subnet_max) {
        (Some(min), Some(max)) => {
            if ip.is_unspecified() || ip < *min || ip > *max {
                return false;
            }
        }
        _ => return false,
    }

    sn6.prefix() == config.ipv6_subnet_len
}

pub fn is_err_etcd_node_exist(err: &XlineRegistryError) -> bool {
    if let XlineRegistryError::Xline(boxed_err) = err
        && let etcd_client::Error::GRpcStatus(status) = &**boxed_err
    {
        return status.code() == Code::AlreadyExists;
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::{network::registry::XlineSubnetRegistry, protocol::config::XlineConfig};
    use crate::api::xlinestore::XlineStore;
    use super::*;
    #[tokio::test]
    async fn test_local_manager_with_xline_registry() {
        let store = XlineStore::new(&["http://127.0.0.1:2379"])
        .await
        .expect("failed to connect etcd");
        store.init_flannel_config()
        .await
        .expect("failed to init flannel config");


        let cfg = XlineConfig {
            endpoints: vec!["http://127.0.0.1:2379".to_string()],
            prefix: "/coreos.com/network".to_string(),
            username: None,
            password: None,
            subnet_lease_renew_margin: None,
        };

        let xline_registry = XlineSubnetRegistry::new(cfg, None)
            .await
            .expect("failed to create XlineSubnetRegistry");

        let registry: Arc<dyn Registry + Send + Sync> = Arc::new(xline_registry);

        let manager = LocalManager::new(registry.clone(), None, None, 5);

        let lease_attrs = LeaseAttrs {
            public_ip: "1.3.3.4".parse().unwrap(),
            backend_type: "vxlan".to_string(),
            backend_data: Some(serde_json::json!({ "VNI": 1 })),
            ..Default::default()
        };

        let config = manager
            .get_network_config()
            .await
            .expect("get config failed");
        println!("Parsed config: {:?}", config);

        let lease = manager
            .acquire_lease(&lease_attrs)
            .await
            .expect("acquire lease failed");
        println!("Lease acquired: {:?}", lease);

        let mut lease2 = lease.clone();
        manager
            .renew_lease(&mut lease2)
            .await
            .expect("renew failed");
        println!("Lease renewed to: {:?}", lease2.expiration);

        assert!(lease2.expiration > lease.expiration);
    }
}
