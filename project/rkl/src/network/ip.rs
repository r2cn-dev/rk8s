use anyhow::{Result, anyhow};
use common::ExternalInterface;
use ipnetwork::{Ipv4Network, Ipv6Network};
use libcni::ip::route::{self, Interface};
use log::info;
use regex::Regex;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::network::iface;

pub trait AddIP<N> {
    fn add(self, n: N) -> Self;
}

pub trait SubIP<N> {
    fn sub(self, n: N) -> Self;
}

impl AddIP<u32> for Ipv4Addr {
    fn add(self, n: u32) -> Self {
        Ipv4Addr::from(u32::from(self) + n)
    }
}

impl SubIP<u32> for Ipv4Addr {
    fn sub(self, n: u32) -> Self {
        Ipv4Addr::from(u32::from(self) - n)
    }
}

impl AddIP<u128> for Ipv6Addr {
    fn add(self, n: u128) -> Self {
        let base = u128::from(self);
        Ipv6Addr::from(base.saturating_add(n))
    }
}

impl SubIP<u128> for Ipv6Addr {
    fn sub(self, n: u128) -> Self {
        let base = u128::from(self);
        Ipv6Addr::from(base.saturating_sub(n))
    }
}

pub fn next_ipv4_network(net: Ipv4Network) -> Result<Ipv4Network> {
    let next_ip = u32::from(net.ip()).wrapping_add(1 << (32 - net.prefix()));
    let next_ip = Ipv4Addr::from(next_ip);
    Ipv4Network::new(next_ip, net.prefix()).map_err(|e| anyhow!(e))
}

pub fn next_ipv6_network(net: Ipv6Network) -> Result<Ipv6Network> {
    let bytes = net.ip().octets();
    let increment = 1u128 << (128 - net.prefix());

    let current = u128::from_be_bytes(bytes);
    let next = current.wrapping_add(increment);
    let next_ip = Ipv6Addr::from(next.to_be_bytes());

    Ipv6Network::new(next_ip, net.prefix()).map_err(|e| anyhow!(e))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IPStack {
    Ipv4,
    Ipv6,
    Dual,
    None,
}
#[derive(Debug, Clone)]
pub struct PublicIPOpts {
    pub public_ip: Option<IpAddr>,
    pub public_ipv6: Option<IpAddr>,
}

pub fn get_ip_family(auto_detect_ipv4: bool, auto_detect_ipv6: bool) -> Result<IPStack> {
    match (auto_detect_ipv4, auto_detect_ipv6) {
        (true, false) => Ok(IPStack::Ipv4),
        (false, true) => Ok(IPStack::Ipv6),
        (true, true) => Ok(IPStack::Dual),
        _ => Err(anyhow!("none defined stack")),
    }
}

pub async fn lookup_ext_iface(
    ifname: Option<String>,
    ifregex_s: Option<String>,
    ifcanreach: Option<IpAddr>,
    ip_stack: IPStack,
    opts: PublicIPOpts,
) -> Result<ExternalInterface> {
    use regex::Regex;

    if ip_stack == IPStack::None {
        return Err(anyhow!("none matched ip stack"));
    }

    let ifregex = if let Some(s) = ifregex_s.as_ref() {
        Some(Regex::new(s).map_err(|e| anyhow!("could not compile regex: {}", e))?)
    } else {
        None
    };

    let mut iface: Option<Interface> = None;
    let mut iface_addr: Option<Ipv4Addr> = None;
    let mut iface_v6_addr: Option<Ipv6Addr> = None;
    let all_ifaces = route::interfaces().await?;
    info!("all_ifaces: {all_ifaces:?}");
    if let Some(name) = ifname {
        if let Ok(ip) = name.parse::<IpAddr>() {
            match (ip_stack, ip) {
                (IPStack::Ipv4, IpAddr::V4(v4)) => {
                    iface = Some(iface::get_interface_by_ipv4(v4).await?);
                    iface_addr = Some(v4);
                }
                (IPStack::Ipv6, IpAddr::V6(v6)) => {
                    iface = Some(iface::get_interface_by_ipv6(v6).await?);
                    iface_v6_addr = Some(v6);
                }
                (IPStack::Dual, IpAddr::V4(v4)) => {
                    iface = Some(iface::get_interface_by_ipv4(v4).await?);
                    iface_addr = Some(v4);

                    if let Some(IpAddr::V6(v6)) = opts.public_ipv6 {
                        let v6_iface = iface::get_interface_by_ipv6(v6).await?;
                        iface_v6_addr = Some(v6);
                        if iface.as_ref().unwrap().name != v6_iface.name {
                            return Err(anyhow!(
                                "v6 interface {} must be the same as v4 interface {}",
                                v6_iface.name,
                                iface.as_ref().unwrap().name
                            ));
                        }
                    }
                }
                _ => {}
            }
        } else {
            iface = Some(route::interface_by_name(name).await?);
        }
    } else if let Some(ref re) = ifregex {
        for candidate in &all_ifaces {
            let matched = match ip_stack {
                IPStack::Ipv4 => {
                    let ips = iface::get_interface_ipv4_addrs(candidate.index).await?;
                    match_ip(&ips, re)
                        .map(|ip| {
                            iface_addr = Some(ip);
                            true
                        })
                        .unwrap_or(false)
                }
                IPStack::Ipv6 => {
                    let ips = iface::get_interface_ipv6_addrs(candidate.index).await?;
                    match_ip(&ips, re)
                        .map(|ip| {
                            iface_v6_addr = Some(ip);
                            true
                        })
                        .unwrap_or(false)
                }
                IPStack::Dual => {
                    let v4ips = iface::get_interface_ipv4_addrs(candidate.index).await?;
                    let v6ips = iface::get_interface_ipv6_addrs(candidate.index).await?;
                    if let Some(ip) = match_ip(&v4ips, re) {
                        iface_addr = Some(ip);
                    } else {
                        continue;
                    }
                    if let Some(ip) = match_ip(&v6ips, re) {
                        iface_v6_addr = Some(ip);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if matched {
                iface = Some(candidate.clone());
                break;
            }
        }

        // fallback: try match interface name
        if iface.is_none() {
            for candidate in &all_ifaces {
                if re.is_match(&candidate.name) {
                    iface = Some(candidate.clone());
                    break;
                }
            }
        }

        if iface.is_none() {
            let mut available_faces = vec![];
            for f in &all_ifaces {
                let iplist = match ip_stack {
                    IPStack::Ipv4 | IPStack::Dual => iface::get_interface_ipv4_addrs(f.index)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(IpAddr::V4)
                        .collect::<Vec<_>>(),
                    IPStack::Ipv6 => iface::get_interface_ipv6_addrs(f.index)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(IpAddr::V6)
                        .collect::<Vec<_>>(),
                    _ => vec![],
                };
                available_faces.push(format!("{}:{:?}", f.name, iplist));
            }
            return Err(anyhow!(
                "Could not match pattern {} to any of the available network interfaces ({})",
                ifregex_s.clone().unwrap_or_default(),
                available_faces.join(", ")
            ));
        }
    } else if let Some(dest) = ifcanreach {
        let (i, ip) = iface::get_interface_by_specific_ip_routing(dest).await?;
        match ip {
            IpAddr::V4(v4) => iface_addr = Some(v4),
            IpAddr::V6(v6) => iface_v6_addr = Some(v6),
        }
        iface = Some(i);
    } else {
        match ip_stack {
            IPStack::Ipv4 => iface = Some(iface::get_default_gateway_interface().await?),
            IPStack::Ipv6 => iface = Some(iface::get_default_v6_gateway_interface().await?),
            IPStack::Dual => {
                let i4 = iface::get_default_gateway_interface().await?;
                let i6 = iface::get_default_v6_gateway_interface().await?;
                if i4.name != i6.name {
                    return Err(anyhow!(
                        "v6 default route interface {} must be same as v4 interface {}",
                        i6.name,
                        i4.name
                    ));
                }
                iface = Some(i4);
            }
            _ => {}
        }
    }
    let iface = iface.ok_or_else(|| anyhow!("no interface matched"))?;

    if iface.mtu.is_none() {
        return Err(anyhow!(
            "failed to determine MTU for {} interface",
            iface.name
        ));
    }

    if ip_stack == IPStack::Ipv4 && iface_addr.is_none() {
        let addrs = iface::get_interface_ipv4_addrs(iface.index).await?;
        iface_addr = addrs.first().copied();
    }
    if ip_stack == IPStack::Ipv6 && iface_v6_addr.is_none() {
        let addrs = iface::get_interface_ipv6_addrs(iface.index).await?;
        iface_v6_addr = addrs.first().copied();
    }
    if ip_stack == IPStack::Dual {
        if iface_addr.is_none() {
            let addrs = iface::get_interface_ipv4_addrs(iface.index).await?;
            iface_addr = addrs.first().copied();
        }
        if iface_v6_addr.is_none() {
            let addrs = iface::get_interface_ipv6_addrs(iface.index).await?;
            iface_v6_addr = addrs.first().copied();
        }
    }

    let ext_addr = opts
        .public_ip
        .and_then(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            _ => None,
        })
        .or(iface_addr);

    let ext_v6_addr = opts
        .public_ipv6
        .and_then(|ip| match ip {
            IpAddr::V6(v6) => Some(v6),
            _ => None,
        })
        .or(iface_v6_addr);

    Ok(ExternalInterface {
        iface,
        iface_addr,
        iface_v6_addr,
        ext_addr,
        ext_v6_addr,
    })
}

fn match_ip<T: Into<IpAddr> + Copy>(iface_ips: &[T], ifregex: &Regex) -> Option<T> {
    iface_ips
        .iter()
        .find(|&&iface_ip| ifregex.is_match(&iface_ip.into().to_string()))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_none_stack_returns_err() {
        let res = lookup_ext_iface(
            None,
            None,
            None,
            IPStack::None,
            PublicIPOpts {
                public_ip: None,
                public_ipv6: None,
            },
        )
        .await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("none matched ip stack"));
    }

    #[tokio::test]
    async fn test_invalid_regex_returns_err() {
        let res = lookup_ext_iface(
            None,
            Some("(*)[".to_string()),
            None,
            IPStack::Ipv4,
            PublicIPOpts {
                public_ip: None,
                public_ipv6: None,
            },
        )
        .await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("could not compile regex"));
    }

    #[tokio::test]
    async fn test_nonexistent_interface_name() {
        let res = lookup_ext_iface(
            Some("this_interface_does_not_exist".to_string()),
            None,
            None,
            IPStack::Ipv4,
            PublicIPOpts {
                public_ip: None,
                public_ipv6: None,
            },
        )
        .await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("no such interface")
                || err.to_lowercase().contains("not found")
        );
    }

    #[tokio::test]
    async fn test_ifcanreach_gateway_should_succeed() {
        use std::net::IpAddr;

        let reach_ip: IpAddr = "192.168.239.128".parse().unwrap();

        let result = lookup_ext_iface(
            None,
            None,
            Some(reach_ip),
            IPStack::Ipv4,
            PublicIPOpts {
                public_ip: None,
                public_ipv6: None,
            },
        )
        .await;

        assert!(result.is_ok(), "Expected success, got error: {:?}", result);
        let iface = result.unwrap();
        println!("get the interface : {iface:?}");
        assert!(
            !iface.iface.name.is_empty(),
            "Interface name should not be empty"
        );
    }
}
