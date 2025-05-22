use std::io::Result;
use futures::try_join;
use std::sync::{Arc, Mutex};

use kaminari::{AsyncAccept, AsyncConnect, IOStream};
use kaminari::mix::{MixAccept, MixConnect};

use realm_io::{CopyBuffer, bidi_copy_buf, buf_size};
use crate::monitor::ConnectionMetrics;

pub async fn run_relay<S: IOStream>(
    src: S,
    dst: S,
    ac: &MixAccept,
    cc: &MixConnect,
    metrics: Arc<Mutex<ConnectionMetrics>>,
) -> Result<()> {
    macro_rules! hs_relay {
        ($ac: expr, $cc: expr) => {
            handshake_and_relay(src, dst, $ac, $cc, metrics.clone()).await
        };
    }

    #[cfg(feature = "transport-boost")]
    {
        use MixConnect::*;
        if let Some(ac) = ac.as_plain() {
            return match cc {
                Plain(cc) => hs_relay!(ac, cc),
                Ws(cc) => hs_relay!(ac, cc),
                Tls(cc) => hs_relay!(ac, cc),
                Wss(cc) => hs_relay!(ac, cc),
            };
        }
    }

    #[cfg(feature = "transport-boost")]
    {
        use MixAccept::*;
        if let Some(cc) = cc.as_plain() {
            return match ac {
                Plain(ac) => hs_relay!(ac, cc),
                Ws(ac) => hs_relay!(ac, cc),
                Tls(ac) => hs_relay!(ac, cc),
                Wss(ac) => hs_relay!(ac, cc),
            };
        }
    }

    // The direct call to handshake_and_relay also needs the metrics argument
    handshake_and_relay(src, dst, ac, cc, metrics).await
}

async fn handshake_and_relay<S, AC, CC>(
    src: S,
    dst: S,
    ac: &AC,
    cc: &CC,
    metrics: Arc<Mutex<ConnectionMetrics>>,
) -> Result<()>
where
    S: IOStream,
    AC: AsyncAccept<S>,
    CC: AsyncConnect<S>,
{
    let mut buf1 = vec![0; buf_size()];
    let mut buf2 = vec![0; buf_size()];

    let (mut src, mut dst) = try_join!(ac.accept(src, &mut buf1), cc.connect(dst, &mut buf2))?;

    let buf1 = CopyBuffer::new(buf1);
    let buf2 = CopyBuffer::new(buf2);

    let result = bidi_copy_buf(&mut src, &mut dst, buf1, buf2).await;

    if let Ok((tx_bytes, rx_bytes)) = result {
        let mut w_metrics = metrics.lock().unwrap();
        w_metrics.update_tx(tx_bytes);
        w_metrics.update_rx(rx_bytes);
    }

    result.map(|_| ())
}
