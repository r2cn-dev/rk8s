use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::commands::compose::spec::NetworkDriver::Bridge;
use crate::commands::compose::spec::NetworkDriver::Host;
use crate::commands::compose::spec::NetworkDriver::None;
use crate::commands::compose::spec::NetworkDriver::Overlay;

use crate::commands::compose::spec::ComposeSpec;
use crate::commands::compose::spec::NetworkSpec;
use crate::commands::compose::spec::ServiceSpec;
use anyhow::Ok;
use anyhow::Result;
use anyhow::anyhow;
use serde::{Deserialize, Serialize};

const CNI_VERSION: &str = "1.0.0";
const STD_CONF_PATH: &str = "/etc/cni/net.d";

const BRIDGE_PLUGIN_NAME: &str = "libbridge";
const BRIDGE_CONF: &str = "bridge.conf";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliNetworkConfig {
    /// default is 1.0.0
    #[serde(default)]
    pub cni_version: String,
    /// the `type` in JSON
    #[serde(rename = "type")]
    pub plugin: String,
    /// network's name
    #[serde(default)]
    pub name: String,
    /// bridge interface' s name (default cni0）
    #[serde(default)]
    pub bridge: String,
    /// whether this network should be set the container's default gateway
    #[serde(default)]
    pub is_default_gateway: Option<bool>,
    /// whether the bridge should at as a gateway
    #[serde(default)]
    pub is_gateway: Option<bool>,
    /// Maximum Transmission Unit (MTU) to set on the bridge interface
    #[serde(default)]
    pub mtu: Option<u32>,
    /// Enable Mac address spoofing check
    #[serde(default)]
    pub mac_spoof_check: Option<bool>,
    /// IPAM type（like host-local, static, etc.）
    #[serde(default)]
    pub ipam_type: Option<String>,
    /// IPAM configuration's file path（Optional）
    #[serde(default)]
    pub ipam_config: Option<String>,
    /// enable hairpin mod
    #[serde(default)]
    pub hairpin_mode: Option<bool>,
    /// VLAN ID
    #[serde(default)]
    pub vlan: Option<u16>,
    /// VLAN Trunk
    #[serde(default)]
    pub vlan_trunk: Option<Vec<u16>>,
}

impl CliNetworkConfig {
    pub fn from_name_bridge(network_name: &str, bridge: &str) -> Self {
        Self {
            bridge: bridge.to_string(),
            name: network_name.to_string(),
            ..Default::default()
        }
    }
}

impl Default for CliNetworkConfig {
    fn default() -> Self {
        Self {
            cni_version: String::from(CNI_VERSION),
            plugin: String::from(BRIDGE_PLUGIN_NAME),
            name: Default::default(),
            bridge: Default::default(),
            is_default_gateway: Default::default(),
            is_gateway: Some(true),
            mtu: Some(1500),
            mac_spoof_check: Default::default(),
            ipam_type: Default::default(),
            ipam_config: Default::default(),
            hairpin_mode: Default::default(),
            vlan: Default::default(),
            vlan_trunk: Default::default(),
        }
    }
}

pub struct NetworkManager {
    map: HashMap<String, NetworkSpec>,
    /// key: network_name; value: bridge interface
    network_interface: HashMap<String, String>,
    /// key: service_name value: networks
    service_mapping: HashMap<String, Vec<String>>,
    /// key: network_name value: (srv_name, service_spec) k
    network_service: HashMap<String, Vec<(String, ServiceSpec)>>,
    /// if there is no network definition then just create a default network
    is_default: bool,
    project_name: String,
}

impl NetworkManager {
    pub fn new(project_name: String) -> Self {
        Self {
            map: HashMap::new(),
            service_mapping: HashMap::new(),
            is_default: false,
            network_service: HashMap::new(),
            project_name,
            network_interface: HashMap::new(),
        }
    }

    pub fn network_service_mapping(&self) -> HashMap<String, Vec<(String, ServiceSpec)>> {
        self.network_service.clone()
    }

    pub fn setup_network_conf(&self, network_name: &String) -> Result<()> {
        // generate the config file
        let interface = self.network_interface.get(network_name).ok_or_else(|| {
            anyhow!(
                "Failed to find bridge interface for network {}",
                network_name
            )
        })?;
        // check if there is other config file if does delete it
        let conf = CliNetworkConfig::from_name_bridge(network_name, interface);
        let conf_value = serde_json::to_value(conf).expect("Failed to parse network config");

        let mut conf_path = PathBuf::from(STD_CONF_PATH);
        conf_path.push(BRIDGE_CONF);
        if let Some(parent) = conf_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        // write it to
        fs::write(conf_path, serde_json::to_string_pretty(&conf_value)?)?;

        Ok(())
    }

    pub fn handle(&mut self, spec: &ComposeSpec) -> Result<()> {
        // read the networks
        if let Some(networks_spec) = &spec.networks {
            self.map = networks_spec.clone()
        } else {
            // there is no definition of networks
            self.is_default = true
        }

        self.validate(spec)?;

        // allocate the bridge interface
        self.allocate_interface()
    }

    pub fn is_default(&self) -> bool {
        self.is_default
    }

    pub fn write_network_config(&self) -> Result<()> {
        Ok(())
    }

    /// validate the correctness and initialize  the service_mapping
    fn validate(&mut self, spec: &ComposeSpec) -> Result<()> {
        for (srv, srv_spec) in &spec.services {
            // if the srv does not have the network definition then add to the default network
            let network_name = format!("{}_default", self.project_name);
            if srv_spec.networks.is_empty() {
                self.network_service
                    .entry(network_name.clone())
                    .or_default()
                    .push((srv.clone(), srv_spec.clone()));

                // add to map
                self.map.insert(
                    network_name,
                    NetworkSpec {
                        external: Option::None,
                        driver: Some(Bridge),
                    },
                );
            }
            for network_name in &srv_spec.networks {
                if !self.map.contains_key(network_name) {
                    return Err(anyhow!(
                        "bad network's definition network {} is not defined",
                        network_name
                    ));
                }
                self.service_mapping
                    .entry(srv.clone())
                    .or_default()
                    .push(network_name.clone());

                self.network_service
                    .entry(network_name.clone())
                    .or_default()
                    .push((srv.clone(), srv_spec.clone()));
            }
        }
        // all the services don't have the network definition then create a default network
        if self.is_default {
            let network_name = format!("{}_default", self.project_name);

            let services: Vec<(String, ServiceSpec)> = spec
                .services
                .iter()
                .map(|(name, spec)| (name.clone(), spec.clone()))
                .collect();

            self.network_service.insert(network_name.clone(), services);
            self.map.insert(
                network_name,
                NetworkSpec {
                    external: Option::None,
                    driver: Some(Bridge),
                },
            );
        }
        Ok(())
    }

    fn allocate_interface(&mut self) -> Result<()> {
        for (i, (k, v)) in self.map.iter().enumerate() {
            if let Some(driver) = &v.driver {
                match driver {
                    // add the bridge default is cni0
                    Bridge => self
                        .network_interface
                        .insert(k.to_string(), format!("cni{}", i + 1).to_string()),
                    Overlay => todo!(),
                    Host => todo!(),
                    None => todo!(),
                };
            }
        }
        Ok(())
    }
}
