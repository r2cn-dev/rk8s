use libcni::ip::route::{Interface, Route};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
};

pub mod lease;
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TypeMeta {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "kind")]
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub annotations: HashMap<String, String>,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PodSpec {
    //if pod is distributed to a node ,then this field should be filled with node-id
    #[serde(default)]
    pub node_name: Option<String>,
    #[serde(default)]
    pub containers: Vec<ContainerSpec>,
    #[serde(default)]
    pub init_containers: Vec<ContainerSpec>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContainerRes {
    pub limits: Option<Resource>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Resource {
    pub cpu: Option<String>,
    pub memory: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub args: Vec<String>,
    pub resources: Option<ContainerRes>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Port {
    #[serde(rename = "containerPort")]
    pub container_port: i32,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(rename = "hostPort", default)]
    pub host_port: i32,
    #[serde(rename = "hostIP", default)]
    pub host_ip: String,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PodTask {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "kind")]
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: PodSpec,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RksMessage {
    //request
    CreatePod(Box<PodTask>),
    DeletePod(String),
    ListPod,

    GetNodeCount,
    RegisterNode(Box<Node>),
    UserRequest(String),
    Heartbeat(String),
    SetNetwork(Box<NodeNetworkConfig>),
    UpdateRoutes(String, Vec<Route>),

    //response
    Ack,
    Error(String),
    NodeCount(usize),
    ListPodRes(Vec<String>),
}

/// Node spec
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeSpec {
    #[serde(rename = "podCIDR")]
    pub pod_cidr: String, // Pod network CIDR assigned to this node
}

/// Node status
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeStatus {
    pub capacity: HashMap<String, String>, // Total resource capacity
    pub allocatable: HashMap<String, String>, // Available for scheduling
    #[serde(default)]
    pub addresses: Vec<NodeAddress>, // Node IPs, hostnames, etc.
    #[serde(default)]
    pub conditions: Vec<NodeCondition>, // Health and status flags
}

/// Node address entry
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeAddress {
    #[serde(rename = "type")]
    pub address_type: String, // e.g., "InternalIP", "Hostname"
    pub address: String, // IP or hostname value
}

/// Node condition entry
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeCondition {
    #[serde(rename = "type")]
    pub condition_type: String, // e.g., "Ready", "MemoryPressure"
    pub status: String, // "True" | "False" | "Unknown"
    #[serde(rename = "lastHeartbeatTime", default)]
    pub last_heartbeat_time: Option<String>, // Last heartbeat timestamp
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Node {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "kind")]
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: NodeSpec,
    pub status: NodeStatus,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeNetworkConfig {
    pub node_id: String,
    pub subnet_env: String,
}

#[derive(Debug, Clone)]
pub struct ExternalInterface {
    pub iface: Interface,
    pub iface_addr: Option<Ipv4Addr>,
    pub iface_v6_addr: Option<Ipv6Addr>,
    pub ext_addr: Option<Ipv4Addr>,
    pub ext_v6_addr: Option<Ipv6Addr>,
}
