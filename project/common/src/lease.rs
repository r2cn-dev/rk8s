use chrono::{DateTime, Utc};
use ipnetwork::{Ipv4Network, Ipv6Network};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    Added = 0,
    Removed = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub lease: Option<Lease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseAttrs {
    #[serde(rename = "PublicIP")]
    pub public_ip: Ipv4Addr,

    #[serde(rename = "PublicIPv6", skip_serializing_if = "Option::is_none")]
    pub public_ipv6: Option<Ipv6Addr>,

    #[serde(rename = "BackendType", skip_serializing_if = "String::is_empty")]
    pub backend_type: String,

    #[serde(rename = "BackendData", skip_serializing_if = "Option::is_none")]
    pub backend_data: Option<JsonValue>,

    #[serde(rename = "BackendV6Data", skip_serializing_if = "Option::is_none")]
    pub backend_v6_data: Option<JsonValue>,

    #[serde(rename = "Node")]
    pub node_id: String,
}

impl Default for LeaseAttrs {
    fn default() -> Self {
        LeaseAttrs {
            public_ip: Ipv4Addr::UNSPECIFIED,
            public_ipv6: None,
            backend_type: String::new(),
            backend_data: None,
            backend_v6_data: None,
            node_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub enable_ipv4: bool,
    pub enable_ipv6: bool,
    pub subnet: Ipv4Network,
    pub ipv6_subnet: Option<Ipv6Network>,
    pub attrs: LeaseAttrs,
    pub expiration: DateTime<Utc>, // from chrono crate
    pub asof: Option<i64>,         // only used in etcd
}

impl Default for Lease {
    fn default() -> Self {
        Lease {
            enable_ipv4: false,
            enable_ipv6: false,
            subnet: "0.0.0.0/32".parse().unwrap(),
            ipv6_subnet: None,
            attrs: LeaseAttrs::default(),
            expiration: Utc::now(),
            asof: None,
        }
    }
}

impl std::fmt::Display for LeaseAttrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BackendType: {}, PublicIP: {}",
            self.backend_type, self.public_ip
        )?;
        write!(
            f,
            ", PublicIPv6: {}",
            self.public_ipv6
                .map_or("(nil)".to_string(), |v| v.to_string())
        )?;
        write!(
            f,
            ", BackendData: {}",
            self.backend_data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "(nil)".to_string())
        )?;
        write!(
            f,
            ", BackendV6Data: {}",
            self.backend_v6_data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "(nil)".to_string())
        )
    }
}
