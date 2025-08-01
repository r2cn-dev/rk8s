use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeSpec {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub services: HashMap<String, ServiceSpec>,

    #[serde(default)]
    pub volumes: Option<VolumesSpec>,

    #[serde(default)]
    pub configs: Option<ConfigSpec>,

    #[serde(default)]
    // pub networks: Option<HashMap<String, NetworkSpec>>,
    pub networks: Option<NetworksSpec>,
    #[serde(default)]
    pub secrets: Option<SecretSpec>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    #[serde(default)]
    pub container_name: Option<String>,

    #[serde(default)]
    pub image: String,

    #[serde(default)]
    pub ports: Vec<String>,

    #[serde(default)]
    pub networks: Vec<String>,

    #[serde(default)]
    pub volumes: Vec<String>,

    #[serde(default)]
    pub command: Vec<String>,

    #[serde(default)]
    pub configs: Option<Vec<String>>,

    #[serde(default)]
    pub secrets: Option<Vec<String>>,

    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumesSpec(pub HashMap<String, VolumeSpec>);

#[derive(Debug, Serialize, Deserialize)]
pub struct VolumeSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpec {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
// #[serde(transparent)]
pub struct NetworksSpec(pub HashMap<String, NetworkSpec>);

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    external: Option<bool>,
    driver: Option<NetworkDirver>,
}

/// network driver: default: Bridge
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum NetworkDirver {
    Bridge,
    Overlay,
    Host,
    None,
}
