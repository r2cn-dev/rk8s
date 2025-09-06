#![allow(dead_code)]
use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use common::lease::{Event as LeaseEvent, EventType, Lease, LeaseAttrs};
use etcd_client::{
    Client, Compare, CompareOp, ConnectOptions, Event as WatchEvent, GetOptions, KeyValue,
    KvClient, PutOptions, Txn, TxnOp, WatchOptions,
};
use futures::StreamExt;
use ipnetwork::{Ipv4Network, Ipv6Network};
use log::{error, info, warn};
use regex::Regex;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tokio::time::{Duration as TokioDuration, sleep};

use crate::network::lease::LeaseWatchResult;
use crate::network::manager::{Cursor, WatchCursor, is_index_too_small};
use crate::network::subnet::{self, parse_subnet_key};
use crate::protocol::config::XlineConfig;

#[derive(Debug, thiserror::Error)]
pub enum XlineRegistryError {
    #[error("try again")]
    TryAgain,
    #[error(
        "flannel config not found in xline store. Did you create your config using etcdv3 API?"
    )]
    ConfigNotFound,
    #[error("no watch channel")]
    NoWatchChannel,
    #[error("subnet already exists")]
    SubnetAlreadyExists,
    #[error(transparent)]
    Xline(#[from] Box<etcd_client::Error>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tls(#[from] native_tls::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<etcd_client::Error> for XlineRegistryError {
    fn from(e: etcd_client::Error) -> Self {
        XlineRegistryError::Xline(Box::new(e))
    }
}

#[async_trait]
pub trait Registry: Send + Sync {
    async fn get_network_config(&self) -> Result<String, XlineRegistryError>;

    async fn get_subnets(&self) -> Result<(Vec<Lease>, i64), XlineRegistryError>;

    async fn get_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
    ) -> Result<(Option<Lease>, i64), XlineRegistryError>;

    async fn create_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        attrs: &LeaseAttrs,
        ttl: Duration,
    ) -> Result<DateTime<Utc>, XlineRegistryError>;

    async fn update_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        attrs: &LeaseAttrs,
        ttl: Duration,
        asof: i64,
    ) -> Result<DateTime<Utc>, XlineRegistryError>;

    async fn delete_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
    ) -> Result<(), XlineRegistryError>;

    async fn watch_subnets(
        &self,
        lease_watch_chan: Sender<Vec<LeaseWatchResult>>,
        since: i64,
    ) -> Result<(), XlineRegistryError>;

    async fn watch_subnet(
        &self,
        since: i64,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        lease_watch_chan: Sender<Vec<LeaseWatchResult>>,
    ) -> Result<(), XlineRegistryError>;

    async fn leases_watch_reset(&self) -> Result<LeaseWatchResult, XlineRegistryError>;
}

pub type XlineNewFunc = fn(
    Arc<XlineConfig>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(Client, KvClient), XlineRegistryError>> + Send>,
>;

// This struct is part of the subnet registry abstraction.
// While some fields or methods may not yet be used in the current implementation,
// it is kept to ensure forward compatibility and to support upcoming features such as
// distributed lease tracking, dynamic subnet reallocation, and multi-cluster integration.
#[allow(dead_code)]
pub struct XlineSubnetRegistry {
    cli_new_func: XlineNewFunc,
    kv_api: Arc<Mutex<KvClient>>,
    cli: Arc<Mutex<Client>>,
    xline_cfg: XlineConfig,
    network_regex: Regex,
}

#[async_trait]
impl Registry for XlineSubnetRegistry {
    async fn get_network_config(&self) -> Result<String, XlineRegistryError> {
        let key = format!("{}/config", self.xline_cfg.prefix);
        let resp = self.kv().await.get(key, None).await?;
        if resp.kvs().is_empty() {
            return Err(XlineRegistryError::ConfigNotFound);
        }

        let value = resp.kvs()[0].value();
        let s = String::from_utf8(value.to_vec()).map_err(|e| {
            warn!("Failed to parse network config as UTF8: {e}");
            XlineRegistryError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        Ok(s)
    }

    async fn get_subnets(&self) -> Result<(Vec<Lease>, i64), XlineRegistryError> {
        let key = format!("{}/subnets", self.xline_cfg.prefix);

        let opts = GetOptions::new().with_prefix();
        let resp = match self.kv().await.get(key, Some(opts)).await {
            Ok(r) => r,
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("Key not found") || err_msg.contains("NOT_FOUND") {
                    return Ok((Vec::new(), 0));
                }
                return Err(e.into());
            }
        };

        let mut leases = Vec::new();
        for kv in resp.kvs() {
            let lease_id = kv.lease();
            let ttl_resp = self.cli().await.lease_time_to_live(lease_id, None).await;
            let ttl = match ttl_resp {
                Ok(r) => r.ttl(),
                Err(e) => {
                    warn!("Could not read ttl: {e:?}");
                    continue;
                }
            };
            match kv_to_ip_lease(kv, ttl) {
                Ok(lease) => leases.push(lease),
                Err(e) => {
                    warn!("Ignoring bad subnet node: {e:?}");
                    continue;
                }
            }
        }
        let revision = resp.header().map_or(0, |h| h.revision());
        Ok((leases, revision))
    }

    async fn get_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
    ) -> Result<(Option<Lease>, i64), XlineRegistryError> {
        info!("sn:{sn:?}, sn6:{sn6:?}");
        let key = format!(
            "{}/subnets/{}",
            self.xline_cfg.prefix,
            subnet::make_subnet_key(&sn, sn6.as_ref())
        );
        info!("key: {key}");
        let resp = self.kv().await.get(key, None).await?;
        if resp.kvs().is_empty() {
            info!("resp's kv is empty");
            return Ok((None, 0));
        }
        let kv = &resp.kvs()[0];
        let lease_id = kv.lease();
        let ttl_resp = self.cli().await.lease_time_to_live(lease_id, None).await?;
        let ttl = ttl_resp.ttl();
        let lease = kv_to_ip_lease(kv, ttl)?;
        let revision = resp.header().map_or(0, |h| h.revision());
        Ok((Some(lease), revision))
    }

    async fn create_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        attrs: &LeaseAttrs,
        ttl: Duration,
    ) -> Result<DateTime<Utc>, XlineRegistryError> {
        let key = format!(
            "{}/subnets/{}",
            self.xline_cfg.prefix,
            subnet::make_subnet_key(&sn, sn6.as_ref())
        );

        let value = serde_json::to_vec(attrs)?;

        let lease_resp = self
            .cli()
            .await
            .lease_client()
            .grant(ttl.num_seconds(), None)
            .await?;

        let lease_id = lease_resp.id();
        let put_op = TxnOp::put(
            key.clone(),
            value,
            Some(PutOptions::new().with_lease(lease_id)),
        );
        let cmp = Compare::version(key.clone(), CompareOp::Equal, 0);

        let txn = Txn::new().when([cmp]).and_then([put_op]);

        let txn_resp = match self.cli().await.txn(txn).await {
            Ok(resp) => resp,
            Err(e) => {
                let _ = self.cli().await.lease_revoke(lease_id).await;
                return Err(e.into());
            }
        };

        if !txn_resp.succeeded() {
            let _ = self.cli().await.lease_revoke(lease_id).await;
            return Err(XlineRegistryError::SubnetAlreadyExists);
        }

        let exp = Utc::now() + chrono::Duration::seconds(lease_resp.ttl());
        Ok(exp)
    }

    async fn update_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        attrs: &LeaseAttrs,
        ttl: Duration,
        _asof: i64,
    ) -> Result<DateTime<Utc>, XlineRegistryError> {
        let key = format!(
            "{}/subnets/{}",
            self.xline_cfg.prefix,
            subnet::make_subnet_key(&sn, sn6.as_ref())
        );

        let value = serde_json::to_vec(attrs)?;

        let lease_resp = self
            .cli()
            .await
            .lease_client()
            .grant(ttl.num_seconds(), None)
            .await?;
        let lease_id = lease_resp.id();

        let res = self
            .kv()
            .await
            .put(key, value, Some(PutOptions::new().with_lease(lease_id)))
            .await;

        if let Err(e) = res {
            let _ = self.cli().await.lease_revoke(lease_id).await;
            return Err(e.into());
        }

        let exp = Utc::now() + chrono::Duration::seconds(lease_resp.ttl());
        Ok(exp)
    }

    async fn delete_subnet(
        &self,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
    ) -> Result<(), XlineRegistryError> {
        let key = format!(
            "{}/subnets/{}",
            self.xline_cfg.prefix,
            subnet::make_subnet_key(&sn, sn6.as_ref())
        );
        self.kv().await.delete(key, None).await?;
        Ok(())
    }

    // leasesWatchReset is called when incremental lease watch failed and we need to grab a snapshot
    async fn leases_watch_reset(&self) -> Result<LeaseWatchResult, XlineRegistryError> {
        let (leases, index) = self
            .get_subnets()
            .await
            .context("failed to retrieve subnet leases")?;

        Ok(LeaseWatchResult {
            events: Vec::new(),
            snapshot: leases,
            cursor: Cursor::Cursor(WatchCursor { index }),
        })
    }

    async fn watch_subnets(
        &self,
        lease_watch_chan: Sender<Vec<LeaseWatchResult>>,
        mut since: i64,
    ) -> Result<(), XlineRegistryError> {
        info!("registry watch subnets");
        let key_prefix = format!("{}/subnets", self.xline_cfg.prefix);

        let mut backoff = TokioDuration::from_millis(100);
        let max_backoff = TokioDuration::from_secs(5);

        loop {
            info!("registry: watching subnets starting from rev {since}");

            let watch_opts = WatchOptions::new().with_prefix().with_start_revision(since);

            let (mut _watcher, mut stream) = match self
                .cli()
                .await
                .watch(key_prefix.clone(), Some(watch_opts))
                .await
            {
                Ok(watch_pair) => watch_pair,
                Err(e) => {
                    error!("Failed to establish etcd watch channel: {e}");
                    sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                    continue;
                }
            };

            backoff = TokioDuration::from_millis(100); // reset backoff on success

            while let Some(resp_result) = stream.next().await {
                let resp = match resp_result {
                    Ok(resp) => resp,
                    Err(e) => {
                        error!("etcd watch stream error: {e}");
                        break;
                    }
                };

                if resp.canceled() {
                    warn!("etcd watch channel canceled, reconnecting...");
                    break;
                }

                since = resp.header().map(|h| h.revision()).unwrap_or(since);

                let mut results = Vec::new();

                for etcd_event in resp.events() {
                    let subnet_result = {
                        let mut cli = self.cli().await;
                        parse_subnet_watch_response(&mut cli, etcd_event).await
                    };
                    match subnet_result {
                        Ok(subnet_event) => {
                            info!("watchSubnets: got valid subnet event with revision {since}");
                            let mut lease = subnet_event.lease.unwrap_or_default();
                            lease.enable_ipv4 = true;
                            let leases = self.get_subnets().await.unwrap_or_default().0;
                            let wr = LeaseWatchResult {
                                events: vec![LeaseEvent {
                                    event_type: subnet_event.event_type,
                                    lease: Some(lease),
                                }],
                                snapshot: leases,
                                cursor: Cursor::Cursor(WatchCursor { index: since }),
                            };
                            results.push(wr);
                        }
                        Err(e) if is_index_too_small(&e) => {
                            warn!("Watch failed due to etcd index outside history window");
                            match self.leases_watch_reset().await {
                                Ok(wr) => results.push(wr),
                                Err(e) => error!("error resetting etcd watch: {e}"),
                            }
                        }
                        Err(e) => {
                            warn!("Watch of subnet failed with error: {e}");
                            results.push(LeaseWatchResult::default());
                        }
                    }
                }

                if !results.is_empty() {
                    lease_watch_chan
                        .send(results)
                        .await
                        .map_err(|_| XlineRegistryError::NoWatchChannel)?;
                }
            }

            // reconnect with exponential backoff
            sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, max_backoff);
        }
    }

    async fn watch_subnet(
        &self,
        since: i64,
        sn: Ipv4Network,
        sn6: Option<Ipv6Network>,
        lease_watch_chan: Sender<Vec<LeaseWatchResult>>,
    ) -> Result<(), XlineRegistryError> {
        let subnet_key = subnet::make_subnet_key(&sn, sn6.as_ref());
        let key = format!("{}/subnets/{}", self.xline_cfg.prefix, subnet_key);

        let mut backoff = TokioDuration::from_millis(100);
        let max_backoff = TokioDuration::from_secs(5);

        let mut rev = since;

        loop {
            let watch_opts = WatchOptions::new().with_prefix().with_start_revision(rev);

            let (mut _watcher, mut stream) =
                match self.cli().await.watch(key.clone(), Some(watch_opts)).await {
                    Ok(watch_pair) => watch_pair,
                    Err(e) => {
                        error!("Failed to establish etcd watch channel: {e}");
                        sleep(backoff).await;
                        backoff = std::cmp::min(backoff * 2, max_backoff);
                        continue;
                    }
                };

            backoff = TokioDuration::from_millis(100);

            while let Some(resp_result) = stream.next().await {
                let resp = match resp_result {
                    Ok(resp) => resp,
                    Err(e) => {
                        error!("etcd watch stream error: {e}");
                        break;
                    }
                };
                if resp.canceled() {
                    warn!("etcd watch channel canceled, reconnecting...");
                    break;
                }

                rev = resp.header().map(|h| h.revision()).unwrap_or(rev);

                let mut batch = Vec::new();

                for etcd_event in resp.events() {
                    let subnet_result = {
                        let mut cli = self.cli().await;
                        parse_subnet_watch_response(&mut cli, etcd_event).await
                    };
                    match subnet_result {
                        Ok(subnet_event) => {
                            let wr = LeaseWatchResult {
                                events: vec![LeaseEvent {
                                    event_type: subnet_event.event_type,
                                    lease: subnet_event.lease.clone(),
                                }],
                                snapshot: vec![],
                                cursor: Cursor::Cursor(WatchCursor { index: since }),
                            };
                            batch.push(wr);
                        }
                        Err(e) if is_index_too_small(&e) => {
                            warn!("Watch failed due to etcd index outside history window");
                            match self.leases_watch_reset().await {
                                Ok(wr) => batch.push(wr),
                                Err(e) => error!("error resetting etcd watch: {e}"),
                            }
                        }
                        Err(e) => {
                            error!("couldn't read etcd event: {e}");
                        }
                    }
                }

                if !batch.is_empty() {
                    lease_watch_chan
                        .send(batch)
                        .await
                        .map_err(|_| XlineRegistryError::NoWatchChannel)?;
                }
            }

            sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, max_backoff);
        }
    }
}

impl XlineSubnetRegistry {
    pub async fn new_xline_client(
        config: Arc<XlineConfig>,
    ) -> Result<(Client, KvClient), XlineRegistryError> {
        let opts = if let (Some(user), Some(pass)) = (&config.username, &config.password) {
            ConnectOptions::default().with_user(user.clone(), pass.clone())
        } else {
            ConnectOptions::default()
        };

        let cli = Client::connect(config.endpoints.clone(), Some(opts)).await?;
        let kv = cli.kv_client();
        Ok((cli, kv))
    }

    pub async fn new(
        config: XlineConfig,
        cli_new_func: Option<XlineNewFunc>,
    ) -> Result<Self, XlineRegistryError> {
        let config_arc = Arc::new(config.clone());
        let func: XlineNewFunc =
            cli_new_func.unwrap_or(|cfg| Box::pin(Self::new_xline_client(cfg)));

        let (cli, kv_api) = func(config_arc).await?;

        let pattern = format!("{}/([^/]*)(/|/config)?$", config.prefix);
        let network_regex = Regex::new(&pattern).unwrap();

        Ok(Self {
            cli_new_func: func,
            cli: Arc::new(Mutex::new(cli)),
            kv_api: Arc::new(Mutex::new(kv_api)),
            xline_cfg: config,
            network_regex,
        })
    }

    async fn kv(&self) -> tokio::sync::MutexGuard<'_, KvClient> {
        self.kv_api.lock().await
    }

    async fn cli(&self) -> tokio::sync::MutexGuard<'_, Client> {
        self.cli.lock().await
    }
}

pub fn kv_to_ip_lease(kv: &KeyValue, ttl: i64) -> Result<Lease, XlineRegistryError> {
    let key_str = std::str::from_utf8(kv.key())?;
    let (subnet4, subnet6) =
        crate::network::subnet::parse_subnet_key(key_str).ok_or_else(|| {
            XlineRegistryError::Other(anyhow::anyhow!("invalid subnet key: {}", key_str))
        })?;
    let attrs: LeaseAttrs = serde_json::from_slice(kv.value())?;

    let expiration = Utc::now() + Duration::seconds(ttl);

    Ok(Lease {
        enable_ipv4: true,
        enable_ipv6: subnet6.is_some(),
        subnet: subnet4,
        ipv6_subnet: subnet6,
        attrs,
        expiration,
        asof: Some(kv.mod_revision()),
    })
}

pub async fn parse_subnet_watch_response(
    cli: &mut Client,
    ev: &WatchEvent,
) -> Result<LeaseEvent, XlineRegistryError> {
    let kv = ev.kv().context("no key-value in watch event")?;
    let key = std::str::from_utf8(kv.key())?;
    let (subnet4, subnet6) = parse_subnet_key(key)
        .ok_or_else(|| anyhow!("{:?}: not a subnet, skipping", ev.event_type()))?;

    match ev.event_type() {
        etcd_client::EventType::Delete => Ok(LeaseEvent {
            event_type: EventType::Removed,
            lease: Some(Lease {
                enable_ipv4: true,
                enable_ipv6: subnet6.is_some(),
                subnet: subnet4,
                ipv6_subnet: subnet6,
                attrs: LeaseAttrs::default(),
                expiration: Utc::now(),
                asof: kv.mod_revision().into(),
            }),
        }),

        _ => {
            let attrs: LeaseAttrs = serde_json::from_slice(kv.value())?;

            let lease_id = kv.lease();
            let ttl_resp = cli.lease_time_to_live(lease_id, None).await?;
            let expiration = Utc::now() + Duration::seconds(ttl_resp.ttl());

            Ok(LeaseEvent {
                event_type: EventType::Added,
                lease: Some(Lease {
                    enable_ipv4: true,
                    enable_ipv6: subnet6.is_some(),
                    subnet: subnet4,
                    ipv6_subnet: subnet6,
                    attrs,
                    expiration,
                    asof: kv.mod_revision().into(),
                }),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ipnetwork::{Ipv4Network, Ipv6Network};
    use std::str::FromStr;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_create_and_get_subnet() {
        let cfg = XlineConfig {
            endpoints: vec!["http://127.0.0.1:2379".to_string()],
            prefix: "/coreos.com/network".to_string(),
            username: None,
            password: None,
            subnet_lease_renew_margin: None,
        };

        let registry = XlineSubnetRegistry::new(cfg, None)
            .await
            .expect("failed to create registry");

        let sn4 = Ipv4Network::from_str("10.1.5.0/24").unwrap();
        let sn6 = Ipv6Network::from_str("fd00::/64").unwrap();

        let lease_attrs = LeaseAttrs {
            public_ip: "1.2.3.4".parse().unwrap(),
            backend_type: "vxlan".to_string(),
            backend_data: Some(serde_json::json!({"VNI": 1})),
            ..Default::default()
        };

        let exp = registry
            .create_subnet(sn4, Some(sn6), &lease_attrs, Duration::seconds(60))
            .await
            .expect("failed to create subnet");

        println!("Lease expiration: {exp}");

        let (lease_opt, _) = registry
            .get_subnet(sn4, Some(sn6))
            .await
            .expect("failed to get subnet");

        assert!(lease_opt.is_some());
        assert_eq!(lease_opt.unwrap().attrs.public_ip.to_string(), "1.2.3.4");
    }

    #[tokio::test]
    async fn test_watch_and_create_subnet() {
        let cfg = XlineConfig {
            endpoints: vec!["http://127.0.0.1:2379".to_string()],
            prefix: "/coreos.com/network".to_string(),
            username: None,
            password: None,
            subnet_lease_renew_margin: None,
        };

        let registry = Arc::new(
            XlineSubnetRegistry::new(cfg, None)
                .await
                .expect("failed to create registry"),
        );

        let revision = registry
            .cli()
            .await
            .kv_client()
            .get("/coreos.com/network/subnets", None)
            .await
            .unwrap()
            .header()
            .map(|h| h.revision())
            .unwrap_or(0);

        let (tx, mut rx) = mpsc::channel(10);

        let registry_for_watch = registry.clone();
        let watch_task = tokio::spawn(async move {
            let _ = registry_for_watch.watch_subnets(tx, revision).await;
        });

        let sn4 = Ipv4Network::from_str("10.1.6.0/24").unwrap();
        let sn6 = Ipv6Network::from_str("fd00::/64").unwrap();
        let lease_attrs = LeaseAttrs {
            public_ip: "1.2.3.4".parse().unwrap(),
            backend_type: "vxlan".to_string(),
            backend_data: Some(serde_json::json!({"VNI": 1})),
            ..Default::default()
        };

        registry
            .create_subnet(sn4, Some(sn6), &lease_attrs, Duration::seconds(60))
            .await
            .expect("failed to create subnet");

        tokio::select! {
            Some(watch_result) = rx.recv() => {
                println!("Received watch event: {:?}", watch_result);
                assert!(!watch_result.is_empty());
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                panic!("Timeout waiting for watch event");
            }
        }

        watch_task.abort();
    }
}
