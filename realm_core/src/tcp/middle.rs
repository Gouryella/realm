use std::io::Result;
use tokio::net::TcpStream;

use super::socket;
use super::plain;

#[cfg(feature = "hook")]
use super::hook;

#[cfg(feature = "proxy")]
use super::proxy;

#[cfg(feature = "transport")]
use super::transport;

use crate::trick::Ref;
use crate::endpoint::{RemoteAddr, ConnectOpts};
use crate::monitor::{ConnectionMetrics, TCP_CONNECTION_METRICS};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[allow(unused)]
pub async fn connect_and_relay(
    mut local: TcpStream,
    raddr: Ref<RemoteAddr>,
    conn_opts: Ref<ConnectOpts>,
    extra_raddrs: Ref<Vec<RemoteAddr>>,
) -> Result<()> {
    let ConnectOpts {
        #[cfg(feature = "proxy")]
        proxy_opts,

        #[cfg(feature = "transport")]
        transport,

        #[cfg(feature = "balance")]
        balancer,

        tcp_keepalive,
        ..
    } = conn_opts.as_ref();

    // before connect:
    // - pre-connect hook
    // - load balance
    // ..
    let raddr = {
        #[cfg(feature = "hook")]
        {
            // accept or deny connection.
            #[cfg(feature = "balance")]
            {
                hook::pre_connect_hook(&mut local, raddr.as_ref(), extra_raddrs.as_ref()).await?;
            }

            // accept or deny connection, or select a remote peer.
            #[cfg(not(feature = "balance"))]
            {
                hook::pre_connect_hook(&mut local, raddr.as_ref(), extra_raddrs.as_ref()).await?
            }
        }

        #[cfg(feature = "balance")]
        {
            use realm_lb::{Token, BalanceCtx};
            let token = balancer.next(BalanceCtx {
                src_ip: &local.peer_addr()?.ip(),
            });
            log::debug!("[tcp]select remote peer, token: {:?}", token);
            match token {
                None | Some(Token(0)) => raddr.as_ref(),
                Some(Token(idx)) => &extra_raddrs.as_ref()[idx as usize - 1],
            }
        }

        #[cfg(not(any(feature = "hook", feature = "balance")))]
        raddr.as_ref()
    };

    // connect!
    let mut remote = socket::connect(raddr, conn_opts.as_ref()).await?;
    log::info!("[tcp]{} => {} as {}", local.peer_addr()?, raddr, remote.peer_addr()?);

    // after connected
    // ..
    #[cfg(feature = "proxy")]
    if proxy_opts.enabled() {
        proxy::handle_proxy(&mut local, &mut remote, *proxy_opts).await?;
    }

    // relay
    let metrics = Arc::new(Mutex::new(ConnectionMetrics::new()));
    let conn_id = Uuid::new_v4().to_string();
    TCP_CONNECTION_METRICS.insert(conn_id.clone(), metrics.clone());
    log::debug!("[tcp] Stored metrics for connection {}", conn_id);

    let relay_result = async {
        #[cfg(feature = "transport")]
        {
            if let Some((ac, cc)) = transport {
                transport::run_relay(local, remote, ac, cc, metrics.clone()).await
            } else {
                plain::run_relay(local, remote, metrics.clone()).await
            }
        }
        #[cfg(not(feature = "transport"))]
        {
            plain::run_relay(local, remote, metrics.clone()).await
        }
    }.await;

    TCP_CONNECTION_METRICS.remove(&conn_id);
    log::debug!("[tcp] Removed metrics for connection {}", conn_id);

    // ignore relay error
    if let Err(e) = &relay_result {
        log::debug!("[tcp]forward error: {}, ignored", e);
    }

    relay_result.map(|_| ())
}
