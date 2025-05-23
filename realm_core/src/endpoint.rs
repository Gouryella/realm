//! Relay endpoint.

use std::fmt::{Display, Formatter};
use std::net::SocketAddr;

use serde::Deserialize;

#[cfg(feature = "transport")]
use kaminari::mix::{MixAccept, MixConnect};

#[cfg(feature = "balance")]
use realm_lb::Balancer;

/// Remote address.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum RemoteAddr {
    SocketAddr(SocketAddr),
    DomainName(String, u16),
}

/// Proxy protocol options.
#[cfg(feature = "proxy")]
#[derive(Debug, Default, Clone, Copy, Deserialize)]
pub struct ProxyOpts {
    #[serde(default)]
    pub send_proxy: bool,
    #[serde(default)]
    pub accept_proxy: bool,
    #[serde(default)]
    pub send_proxy_version: usize,
    #[serde(default)]
    pub accept_proxy_timeout: usize,
}

#[cfg(feature = "proxy")]
impl ProxyOpts {
    #[inline]
    pub(crate) const fn enabled(&self) -> bool {
        self.send_proxy || self.accept_proxy
    }
}

/// Connect or associate options.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ConnectOpts {
    #[serde(default)]
    pub connect_timeout: usize,
    #[serde(default)]
    pub associate_timeout: usize,
    #[serde(default)]
    pub tcp_keepalive: usize,
    #[serde(default)]
    pub tcp_keepalive_probe: usize,
    #[serde(default)]
    pub bind_address: Option<SocketAddr>,
    #[serde(default)]
    pub bind_interface: Option<String>,

    #[cfg(feature = "proxy")]
    #[serde(default)]
    pub proxy_opts: ProxyOpts,

    #[cfg(feature = "transport")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(skip_deserializing)]
    pub transport: Option<(MixAccept, MixConnect)>,

    #[cfg(feature = "balance")]
    #[serde(default)]
    pub balancer: Balancer,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct BindOpts {
    #[serde(default)]
    pub ipv6_only: bool,
    #[serde(default)]
    pub bind_interface: Option<String>,
}

/// Relay endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct Endpoint {
    pub laddr: SocketAddr,
    pub raddr: RemoteAddr,
    #[serde(default)]
    pub bind_opts: BindOpts,
    #[serde(default)]
    pub conn_opts: ConnectOpts,
    #[serde(default)]
    pub extra_raddrs: Vec<RemoteAddr>,
}

// display impl below

impl Display for RemoteAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use RemoteAddr::*;
        match self {
            SocketAddr(addr) => write!(f, "{}", addr),
            DomainName(host, port) => write!(f, "{}:{}", host, port),
        }
    }
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> [{}", &self.laddr, &self.raddr)?;
        for raddr in self.extra_raddrs.iter() {
            write!(f, "|{}", raddr)?;
        }
        write!(f, "]; options: {}; {}", &self.bind_opts, &self.conn_opts)
    }
}

impl Display for BindOpts {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let BindOpts {
            ipv6_only,
            bind_interface,
        } = self;

        write!(f, "ipv6-only={}", ipv6_only)?;

        if let Some(iface) = bind_interface {
            write!(f, "listen-iface={}", iface)?;
        }

        Ok(())
    }
}

impl Display for ConnectOpts {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let ConnectOpts {
            connect_timeout,
            associate_timeout,
            tcp_keepalive,
            tcp_keepalive_probe,
            bind_address,
            bind_interface,

            #[cfg(feature = "proxy")]
            proxy_opts,

            #[cfg(feature = "transport")]
            transport,

            #[cfg(feature = "balance")]
            balancer,
        } = self;

        if let Some(iface) = bind_interface {
            write!(f, "send-iface={}, ", iface)?;
        }

        if let Some(send_through) = bind_address {
            write!(f, "send-through={}; ", send_through)?;
        }

        #[cfg(feature = "proxy")]
        {
            let ProxyOpts {
                send_proxy,
                accept_proxy,
                send_proxy_version,
                accept_proxy_timeout,
            } = proxy_opts;
            write!(
                f,
                "send-proxy={0}, send-proxy-version={2}, accept-proxy={1}, accept-proxy-timeout={3}s; ",
                send_proxy, accept_proxy, send_proxy_version, accept_proxy_timeout
            )?;
        }

        write!(
            f,
            "tcp-keepalive={}s[{}] connect-timeout={}s, associate-timeout={}s; ",
            tcp_keepalive, tcp_keepalive_probe, connect_timeout, associate_timeout
        )?;

        #[cfg(feature = "transport")]
        if let Some((ac, cc)) = transport {
            write!(f, "transport={}||{}; ", ac, cc)?;
        }

        #[cfg(feature = "balance")]
        write!(f, "balance={}", balancer.strategy())?;
        Ok(())
    }
}
