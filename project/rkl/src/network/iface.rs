#![allow(dead_code)]
use anyhow::{Context, Result, anyhow, bail};
use common::ExternalInterface;
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use libcni::ip::{
    addr::{self, Addr},
    route::{self, Interface},
};
use log::info;
use netlink_packet_route::AddressFamily;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

fn is_global_unicast(ip: &Ipv4Addr) -> bool {
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_broadcast()
        || ip.is_link_local()
        || ip.is_multicast())
}

fn is_global_unicast_v6(ip: &Ipv6Addr) -> bool {
    !(ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_unicast_link_local())
}

pub async fn get_iface_addrs(index: u32) -> Result<Vec<Addr>> {
    addr::addr_list(index, AddressFamily::Inet).await
}

pub async fn get_iface_v6_addrs(index: u32) -> Result<Vec<Addr>> {
    addr::addr_list(index, AddressFamily::Inet6).await
}

pub async fn get_interface_ipv4_addrs(index: u32) -> Result<Vec<Ipv4Addr>> {
    let addrs = addr::addr_list(index, AddressFamily::Inet).await?;
    let mut ip_addrs = Vec::new();
    let mut link_local = Vec::new();

    for addr in addrs {
        match addr.ipnet.ip() {
            IpAddr::V4(ipv4) => {
                if is_global_unicast(&ipv4) {
                    ip_addrs.push(ipv4);
                    continue;
                }
                if ipv4.is_link_local() {
                    link_local.push(ipv4);
                }
            }
            _ => continue,
        }
    }

    if ip_addrs.is_empty() && !link_local.is_empty() {
        ip_addrs.extend(link_local);
    }

    if ip_addrs.is_empty() {
        bail!("No IPv4 address found for given interface");
    }

    Ok(ip_addrs)
}

pub async fn get_interface_ipv6_addrs(index: u32) -> Result<Vec<Ipv6Addr>> {
    let addrs = addr::addr_list(index, AddressFamily::Inet6).await?;

    let mut ip_addrs = Vec::new();
    let mut link_local = Vec::new();

    for addr in addrs {
        match addr.ipnet.ip() {
            IpAddr::V6(ipv6) => {
                if is_global_unicast_v6(&ipv6) {
                    ip_addrs.push(ipv6);
                } else if ipv6.is_unicast_link_local() {
                    link_local.push(ipv6);
                }
            }
            _ => continue,
        }
    }

    if ip_addrs.is_empty() && !link_local.is_empty() {
        ip_addrs.extend(link_local);
    }

    if ip_addrs.is_empty() {
        bail!("No IPv6 address found for given interface");
    }

    Ok(ip_addrs)
}

pub async fn get_interface_ipv4_addr_match(index: u32, match_addr: Ipv4Addr) -> Result<()> {
    let addrs = get_interface_ipv4_addrs(index).await?;

    if addrs.contains(&match_addr) {
        Ok(())
    } else {
        anyhow::bail!("No matching IPv4 address found for given interface");
    }
}

pub async fn get_interface_ipv6_addr_match(index: u32, match_addr: Ipv6Addr) -> Result<()> {
    let addrs = get_interface_ipv6_addrs(index).await?;

    if addrs.contains(&match_addr) {
        Ok(())
    } else {
        anyhow::bail!("No matching IPv6 address found for given interface");
    }
}

pub async fn get_default_gateway_interface() -> Result<Interface> {
    let routes = route::route_list(AddressFamily::Inet).await?;

    for route in routes {
        match route.dst {
            None => {
                if let Some(index) = route.oif_index {
                    return route::interface_by_index(index).await;
                } else {
                    return Err(anyhow!(
                        "Found default route but could not determine interface"
                    ));
                }
            }
            Some(IpNetwork::V4(ipnet)) if ipnet.ip().is_unspecified() && ipnet.prefix() == 0 => {
                if let Some(index) = route.oif_index {
                    return route::interface_by_index(index).await;
                } else {
                    return Err(anyhow!(
                        "Found default route but could not determine interface"
                    ));
                }
            }
            _ => {}
        }
    }

    bail!("Unable to find default route");
}

pub async fn get_default_v6_gateway_interface() -> Result<Interface> {
    let routes = route::route_list(AddressFamily::Inet6).await?;

    for route in routes {
        match route.dst {
            None => {
                if let Some(index) = route.oif_index {
                    return route::interface_by_index(index).await;
                } else {
                    return Err(anyhow!(
                        "Found default v6 route but could not determine interface"
                    ));
                }
            }
            Some(IpNetwork::V6(ipnet)) if ipnet.ip().is_unspecified() && ipnet.prefix() == 0 => {
                if let Some(index) = route.oif_index {
                    return route::interface_by_index(index).await;
                } else {
                    return Err(anyhow!(
                        "Found default v6 route but could not determine interface"
                    ));
                }
            }
            _ => {}
        }
    }
    bail!("Unable to find default v6 route");
}

pub async fn get_interface_by_ipv4(ip: Ipv4Addr) -> Result<Interface> {
    let ifaces = route::interfaces().await?;

    for iface in &ifaces {
        if let Ok(addrs) = get_interface_ipv4_addrs(iface.index).await
            && addrs.contains(&ip)
        {
            return Ok(iface.clone());
        }
    }

    bail!("No interface with given IPv4 address found");
}

pub async fn get_interface_by_ipv6(ip: Ipv6Addr) -> Result<Interface> {
    let ifaces = route::interfaces().await?;

    for iface in &ifaces {
        if let Ok(addrs) = get_interface_ipv6_addrs(iface.index).await
            && addrs.contains(&ip)
        {
            return Ok(iface.clone());
        }
    }

    bail!("No interface with given IPv6 address found");
}

pub async fn get_interface_by_specific_ip_routing(ip: IpAddr) -> Result<(Interface, IpAddr)> {
    let routes = route::route_get(ip)
        .await
        .with_context(|| format!("couldn't lookup route to {ip}"))?;

    if let Some(route) = routes.into_iter().next() {
        let iface_index = route
            .oif_index
            .ok_or_else(|| anyhow::anyhow!("route has no oif_index"))?;
        let iface = route::interface_by_index(iface_index)
            .await
            .context("couldn't lookup interface")?;

        if let Some(src_ip) = route.src {
            return Ok((iface, src_ip));
        } else {
            return Err(anyhow::anyhow!("route has no source IP"));
        }
    }

    bail!("No interface with given IP found")
}

pub async fn direct_routing(ip: IpAddr) -> Result<bool> {
    let routes = route::route_get(ip)
        .await
        .with_context(|| format!("couldn't lookup route to {ip}"))?;

    if routes.len() == 1 {
        let route = &routes[0];
        if route.gateway.is_none() {
            return Ok(true);
        }
    }

    Ok(false)
}

pub async fn ensure_v4_address_on_link(
    ipa: Ipv4Network,
    ipn: Ipv4Network,
    index: u32,
) -> Result<()> {
    let addr = Addr {
        ipnet: IpNetwork::V4(ipa),
        ..Default::default()
    };

    let existing_addrs = addr::addr_list(index, AddressFamily::Inet).await?;
    let mut has_addr = false;

    for existing in &existing_addrs {
        if *existing == addr {
            has_addr = true;
            continue;
        }

        if let IpNetwork::V4(existing_net) = existing.ipnet
            && ipn.contains(existing_net.ip())
        {
            addr::addr_del(index, existing_net.ip().into()).await?;
            info!("removed IP address {existing_net} from ifindex {index}");
        }
    }

    if !has_addr {
        addr::addr_add(index, ipa.ip().into(), ipa.prefix()).await?;
        info!("added IP address {ipa} to ifindex {index}");
    }

    Ok(())
}

pub async fn ensure_v6_address_on_link(
    ipa: Ipv6Network,
    _ipn: Ipv6Network,
    index: u32,
) -> Result<()> {
    let addr = Addr {
        ipnet: IpNetwork::V6(ipa),
        ..Default::default()
    };

    let mut existing_addrs = addr::addr_list(index, AddressFamily::Inet6).await?;
    let mut only_link_local = true;

    for existing in &existing_addrs {
        if let IpNetwork::V6(existing_net) = existing.ipnet
            && !existing_net.ip().is_unicast_link_local()
        {
            if *existing != addr {
                addr::addr_del(index, existing_net.ip().into()).await?;
                info!("removed v6 IP address {existing_net} from ifindex {index}");
                existing_addrs.clear();
                only_link_local = false;
                break;
            } else {
                return Ok(());
            }
        }
    }

    if only_link_local {
        existing_addrs.clear();
    }

    if existing_addrs.is_empty() {
        addr::addr_add(index, ipa.ip().into(), ipa.prefix()).await?;
        info!("added v6 IP address {ipa} to ifindex {index}");
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endianness {
    Little,
    Big,
}

pub static NATIVE_ENDIAN: LazyLock<Endianness> = LazyLock::new(|| {
    let num: u16 = 1;
    let bytes = num.to_ne_bytes();
    if bytes[0] == 1 {
        Endianness::Little
    } else {
        Endianness::Big
    }
});

pub fn natively_little() -> bool {
    *NATIVE_ENDIAN == Endianness::Little
}

/// Check if the interface configuration is compatible with host-gw backend
pub fn check_hostgw_compatibility(interface: &ExternalInterface) -> Result<()> {
    match (interface.ext_addr, interface.iface_addr) {
        (Some(ext_addr), Some(iface_addr)) => {
            if ext_addr != iface_addr {
                return Err(anyhow!(
                    "Your PublicIP ({}) differs from interface IP ({}), meaning that probably you're on a NAT, which is not supported by host-gw backend",
                    ext_addr,
                    iface_addr
                ));
            }
        }
        (None, _) => {
            return Err(anyhow!("No external IP address available"));
        }
        (_, None) => {
            return Err(anyhow!("No interface IP address available"));
        }
    }

    info!("Host-GW compatibility check passed");
    Ok(())
}
