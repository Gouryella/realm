use std::io::Result;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;

use crate::monitor::{ConnectionMetrics, UDP_ASSOCIATION_METRICS};
use super::SockMap;
use super::{socket, batched};

use crate::trick::Ref;
use crate::time::timeoutfut;
use crate::dns::resolve_addr;
use crate::endpoint::{RemoteAddr, ConnectOpts};

use batched::{Packet, SockAddrStore};
use registry::Registry;
mod registry {
    use super::*;
    type Range = std::ops::Range<u16>;

    pub struct Registry {
        pkts: Box<[Packet]>,
        groups: Vec<Range>,
        cursor: u16,
    }

    impl Registry {
        pub fn new(npkts: usize) -> Self {
            debug_assert!(npkts <= batched::MAX_PACKETS);
            Self {
                pkts: vec![Packet::new(); npkts].into_boxed_slice(),
                groups: Vec::with_capacity(npkts),
                cursor: 0u16,
            }
        }

        pub async fn batched_recv_on(&mut self, sock: &UdpSocket) -> Result<()> {
            let n = batched::recv_some(sock, &mut self.pkts).await?;
            self.cursor = n as u16;
            Ok(())
        }

        pub fn group_by_addr(&mut self) {
            let n = self.cursor as usize;
            self.groups.clear();
            group_by_inner(&mut self.pkts[..n], &mut self.groups, |a, b| a.addr == b.addr);
        }

        pub fn group_iter(&self) -> GroupIter {
            GroupIter {
                pkts: &self.pkts,
                ranges: self.groups.iter(),
            }
        }

        pub fn iter(&self) -> std::slice::Iter<'_, Packet> {
            self.pkts[..self.cursor as usize].iter()
        }

        pub const fn count(&self) -> usize {
            self.cursor as usize
        }
    }

    use std::slice::Iter;
    use std::iter::Iterator;
    pub struct GroupIter<'a> {
        pkts: &'a [Packet],
        ranges: Iter<'a, Range>,
    }

    impl<'a> Iterator for GroupIter<'a> {
        type Item = &'a [Packet];

        fn next(&mut self) -> Option<Self::Item> {
            self.ranges
                .next()
                .map(|Range { start, end }| &self.pkts[*start as usize..*end as usize])
        }
    }

    fn group_by_inner<T, F>(data: &mut [T], groups: &mut Vec<Range>, eq: F)
    where
        F: Fn(&T, &T) -> bool,
    {
        let maxn = data.len();
        let (mut beg, mut end) = (0, 1);
        while end < maxn {
            // go ahead if addr is same
            if eq(&data[end], &data[beg]) {
                end += 1;
                continue;
            }
            // pick packets afterwards
            let mut probe = end + 1;
            while probe < maxn {
                if eq(&data[probe], &data[beg]) {
                    data.swap(probe, end);
                    end += 1;
                }
                probe += 1;
            }
            groups.push(beg as _..end as _);
            (beg, end) = (end, end + 1);
        }
        groups.push(beg as _..end as _);
    }
}

pub async fn associate_and_relay(
    lis: Ref<UdpSocket>,
    rname: Ref<RemoteAddr>,
    conn_opts: Ref<ConnectOpts>,
    sockmap: Ref<SockMap>,
) -> Result<()> {
    let mut registry = Registry::new(batched::MAX_PACKETS);

    loop {
        registry.batched_recv_on(&lis).await?;
        log::debug!("[udp]entry batched recvfrom[{}]", registry.count());
        let raddr = resolve_addr(&rname).await?.iter().next().unwrap();
        log::debug!("[udp]{} resolved as {}", *rname, raddr);

        registry.group_by_addr();
        for pkts in registry.group_iter() {
            let laddr = pkts[0].addr.clone().into();
            let rsock = sockmap.find_or_insert(&laddr, || {
                let s = Arc::new(socket::associate(&raddr, &conn_opts)?);
                let metrics_for_laddr = UDP_ASSOCIATION_METRICS
                    .entry(laddr)
                    .or_insert_with(|| Arc::new(Mutex::new(ConnectionMetrics::new())))
                    .value()
                    .clone();
                log::debug!("[udp] Ensuring metrics for association {} stored/retrieved.", laddr);
                tokio::spawn(send_back(
                    lis,
                    laddr,
                    s.clone(),
                    conn_opts,
                    sockmap,
                    metrics_for_laddr,
                ));
                log::info!("[udp]new association {} => {} as {}", laddr, *rname, raddr);
                Result::Ok(s)
            })?;

            // Uplink traffic processing
            let packets_to_send_iter_vec: Vec<_> = pkts.iter().map(|x| x.ref_with_addr(&raddr.into())).collect();
            let total_bytes_uplink: usize = packets_to_send_iter_vec.iter().map(|p_ref| p_ref.len()).sum();

            batched::send_all(&rsock, packets_to_send_iter_vec.into_iter()).await?;

            if let Some(metrics_entry) = UDP_ASSOCIATION_METRICS.get(&laddr) {
                let metrics = metrics_entry.value(); // This is &Arc<Mutex<ConnectionMetrics>>
                if let Ok(mut w_metrics) = metrics.lock() {
                    w_metrics.update_tx(total_bytes_uplink as u64);
                } else {
                    log::warn!("[udp] Failed to lock metrics for TX update for {}", laddr);
                }
            } else {
                log::warn!("[udp] No metrics found for uplink for {} (key: {}). Total uplink bytes: {}", rname.to_string(), laddr, total_bytes_uplink);
            }
        }
    }
}

async fn send_back(
    lsock: Ref<UdpSocket>,
    laddr: SocketAddr,
    rsock: Arc<UdpSocket>,
    conn_opts: Ref<ConnectOpts>,
    sockmap: Ref<SockMap>,
    metrics: Arc<Mutex<ConnectionMetrics>>,
) {
    let mut registry = Registry::new(batched::MAX_PACKETS);
    let timeout = conn_opts.associate_timeout;
    let laddr_s: SockAddrStore = laddr.into();

    loop {
        match timeoutfut(registry.batched_recv_on(&rsock), timeout).await {
            Err(_) => {
                log::debug!("[udp]rear recvfrom timeout");
                break;
            }
            Ok(Err(e)) => {
                log::error!("[udp]rear recvfrom failed: {}", e);
                break;
            }
            Ok(Ok(())) => {
                log::debug!("[udp]rear batched recvfrom[{}]", registry.count())
            }
        };

        let packets_to_send_iter_vec: Vec<_> = registry.iter().map(|pkt| pkt.ref_with_addr(&laddr_s)).collect();
        let total_bytes_downlink: usize = packets_to_send_iter_vec.iter().map(|p_ref| p_ref.len()).sum();

        if let Err(e) = batched::send_all(&lsock, packets_to_send_iter_vec.into_iter()).await {
            log::error!("[udp]failed to sendto client{}: {}", &laddr, e);
            break;
        } else {
            if let Ok(mut w_metrics) = metrics.lock() {
                 w_metrics.update_rx(total_bytes_downlink as u64);
            } else {
                log::warn!("[udp] Failed to lock metrics for RX update for {}", laddr);
            }
        }
    }

    sockmap.remove(&laddr);
    UDP_ASSOCIATION_METRICS.remove(&laddr);
    log::debug!("[udp]remove association and metrics for {}", &laddr);
}
