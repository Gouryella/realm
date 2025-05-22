use std::io::Result;
use tokio::net::TcpStream;
use crate::monitor::ConnectionMetrics;
use std::sync::{Arc, Mutex};

#[inline]
pub async fn run_relay(mut local: TcpStream, mut remote: TcpStream, metrics: Arc<Mutex<ConnectionMetrics>>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::io::ErrorKind;
        let result = realm_io::bidi_zero_copy(&mut local, &mut remote).await;
        match result {
            Ok((a_to_b, b_to_a)) => {
                let mut w_metrics = metrics.lock().unwrap();
                w_metrics.update_tx(a_to_b);
                w_metrics.update_rx(b_to_a);
                Ok(())
            }
            Err(ref e) if e.kind() == ErrorKind::InvalidInput => {
                // Fallback to bidi_copy if zero_copy is not supported or fails with InvalidInput
                let fallback_result = realm_io::bidi_copy(&mut local, &mut remote).await;
                if let Ok((a_to_b, b_to_a)) = fallback_result {
                    let mut w_metrics = metrics.lock().unwrap();
                    w_metrics.update_tx(a_to_b);
                    w_metrics.update_rx(b_to_a);
                }
                fallback_result.map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let result = realm_io::bidi_copy(&mut local, &mut remote).await;
        if let Ok((a_to_b, b_to_a)) = result {
            let mut w_metrics = metrics.lock().unwrap();
            w_metrics.update_tx(a_to_b);
            w_metrics.update_rx(b_to_a);
        }
        result.map(|_| ())
    }
}
