use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use serde::Serialize;

pub static TCP_CONNECTION_METRICS: Lazy<DashMap<String, Arc<Mutex<ConnectionMetrics>>>> = Lazy::new(DashMap::new);
pub static UDP_ASSOCIATION_METRICS: Lazy<DashMap<SocketAddr, Arc<Mutex<ConnectionMetrics>>>> = Lazy::new(DashMap::new);

#[derive(Debug, Serialize, Default, Clone)]
pub struct TrafficStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

#[derive(Debug, Serialize, Clone)]
pub struct ConnectionMetrics {
    pub traffic: TrafficStats,
    pub start_time: Instant,
    pub last_tx_bytes: u64, // Made public for Serialize and Clone
    pub last_rx_bytes: u64, // Made public for Serialize and Clone
    pub last_speed_update_time: Instant, // Made public for Serialize and Clone
    pub upload_speed_bps: f64,
    pub download_speed_bps: f64,
}

impl Default for ConnectionMetrics {
    fn default() -> Self {
        Self {
            traffic: TrafficStats::default(),
            start_time: Instant::now(),
            last_tx_bytes: 0,
            last_rx_bytes: 0,
            last_speed_update_time: Instant::now(),
            upload_speed_bps: 0.0,
            download_speed_bps: 0.0,
        }
    }
}

impl ConnectionMetrics {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            traffic: TrafficStats::default(),
            start_time: now,
            last_tx_bytes: 0,
            last_rx_bytes: 0,
            last_speed_update_time: now,
            upload_speed_bps: 0.0,
            download_speed_bps: 0.0,
        }
    }

    pub fn update_tx(&mut self, bytes: u64) {
        self.traffic.tx_bytes += bytes;
    }

    pub fn update_rx(&mut self, bytes: u64) {
        self.traffic.rx_bytes += bytes;
    }

    pub fn calculate_speed(&mut self) {
        let now = Instant::now();
        let duration = now.duration_since(self.last_speed_update_time);
        let seconds = duration.as_secs_f64();

        if seconds < 1e-6 { // Avoid division by zero or very small numbers
            // If the duration is too short, speeds are effectively unchanged or unreliable to calculate
            // self.upload_speed_bps = 0.0; // Or maintain last known speed, depending on desired behavior
            // self.download_speed_bps = 0.0;
            return;
        }

        let tx_diff = self.traffic.tx_bytes.saturating_sub(self.last_tx_bytes);
        let rx_diff = self.traffic.rx_bytes.saturating_sub(self.last_rx_bytes);

        self.upload_speed_bps = (tx_diff as f64 * 8.0) / seconds;
        self.download_speed_bps = (rx_diff as f64 * 8.0) / seconds;

        self.last_tx_bytes = self.traffic.tx_bytes;
        self.last_rx_bytes = self.traffic.rx_bytes;
        self.last_speed_update_time = now;
    }
}

pub async fn periodically_calculate_speeds() {
    log::info!("Starting periodic speed calculation task.");
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await; // Interval can be configurable later

        for entry in TCP_CONNECTION_METRICS.iter() {
            let Ok(mut metrics) = entry.value().lock() else {
                log::warn!("Failed to lock TCP metrics for speed calculation for key: {}", entry.key());
                continue;
            };
            metrics.calculate_speed();
        }

        for entry in UDP_ASSOCIATION_METRICS.iter() {
             let Ok(mut metrics) = entry.value().lock() else {
                log::warn!("Failed to lock UDP metrics for speed calculation for key: {:?}", entry.key());
                continue;
            };
            metrics.calculate_speed();
        }
        log::debug!("Periodic speed calculation complete.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_traffic_stats_initialization() {
        let stats = TrafficStats::default();
        assert_eq!(stats.tx_bytes, 0);
        assert_eq!(stats.rx_bytes, 0);
    }

    #[test]
    fn test_connection_metrics_initialization() {
        let metrics = ConnectionMetrics::new();
        assert_eq!(metrics.traffic.tx_bytes, 0);
        assert_eq!(metrics.traffic.rx_bytes, 0);
        assert_eq!(metrics.upload_speed_bps, 0.0);
        assert_eq!(metrics.download_speed_bps, 0.0);
        assert_eq!(metrics.last_tx_bytes, 0);
        assert_eq!(metrics.last_rx_bytes, 0);
    }

    #[test]
    fn test_update_tx_rx() {
        let mut metrics = ConnectionMetrics::new();

        metrics.update_tx(100);
        assert_eq!(metrics.traffic.tx_bytes, 100);
        assert_eq!(metrics.traffic.rx_bytes, 0);

        metrics.update_rx(200);
        assert_eq!(metrics.traffic.tx_bytes, 100);
        assert_eq!(metrics.traffic.rx_bytes, 200);

        metrics.update_tx(50);
        assert_eq!(metrics.traffic.tx_bytes, 150);
        assert_eq!(metrics.traffic.rx_bytes, 200);

        metrics.update_rx(100);
        assert_eq!(metrics.traffic.tx_bytes, 150);
        assert_eq!(metrics.traffic.rx_bytes, 300);
    }

    #[test]
    fn test_calculate_speed_no_time_elapsed() {
        let mut metrics = ConnectionMetrics::new();
        metrics.update_tx(100);
        // Directly manipulate last_speed_update_time to simulate no time elapsed.
        // This is more reliable than relying on thread::sleep(0) or very short sleeps.
        metrics.last_speed_update_time = Instant::now(); 
        metrics.calculate_speed();

        assert_eq!(metrics.upload_speed_bps, 0.0);
        assert_eq!(metrics.download_speed_bps, 0.0);
    }

    #[test]
    fn test_calculate_speed_no_new_bytes() {
        let mut metrics = ConnectionMetrics::new();
        thread::sleep(Duration::from_millis(60)); // Ensure some time passes
        metrics.calculate_speed();

        assert_eq!(metrics.upload_speed_bps, 0.0);
        assert_eq!(metrics.download_speed_bps, 0.0);
        assert_eq!(metrics.last_tx_bytes, 0);
        assert_eq!(metrics.last_rx_bytes, 0);
    }

    #[test]
    fn test_calculate_speed_multiple_calls() {
        let mut metrics = ConnectionMetrics::new();
        let tolerance = 0.30; // 30% tolerance

        // --- First interval ---
        metrics.update_tx(1000);
        metrics.update_rx(2000);
        let sleep_duration1 = Duration::from_millis(500);
        thread::sleep(sleep_duration1);
        metrics.calculate_speed();

        let elapsed_secs1 = sleep_duration1.as_secs_f64();
        let expected_upload_bps1 = (1000.0 * 8.0) / elapsed_secs1;
        let expected_download_bps1 = (2000.0 * 8.0) / elapsed_secs1;

        assert!(
            metrics.upload_speed_bps >= expected_upload_bps1 * (1.0 - tolerance) &&
            metrics.upload_speed_bps <= expected_upload_bps1 * (1.0 + tolerance),
            "Upload speed {} not within {}% tolerance of {}", metrics.upload_speed_bps, tolerance * 100.0, expected_upload_bps1
        );
        assert!(
            metrics.download_speed_bps >= expected_download_bps1 * (1.0 - tolerance) &&
            metrics.download_speed_bps <= expected_download_bps1 * (1.0 + tolerance),
            "Download speed {} not within {}% tolerance of {}", metrics.download_speed_bps, tolerance * 100.0, expected_download_bps1
        );
        assert_eq!(metrics.last_tx_bytes, 1000);
        assert_eq!(metrics.last_rx_bytes, 2000);

        // --- Second interval ---
        // Simulate some more time passing before next byte update for more realism
        thread::sleep(Duration::from_millis(10)); 
        
        metrics.update_tx(500); // Total tx is now 1500
        metrics.update_rx(1000); // Total rx is now 3000

        let sleep_duration2 = Duration::from_millis(500);
        thread::sleep(sleep_duration2);
        metrics.calculate_speed(); // Calculates based on delta: 500 tx, 1000 rx over ~0.5s

        let elapsed_secs2 = sleep_duration2.as_secs_f64();

        // The bytes for the second calculation are the *difference* from the last update
        let expected_upload_bps2 = (500.0 * 8.0) / elapsed_secs2; 
        let expected_download_bps2 = (1000.0 * 8.0) / elapsed_secs2;

        assert!(
            metrics.upload_speed_bps >= expected_upload_bps2 * (1.0 - tolerance) &&
            metrics.upload_speed_bps <= expected_upload_bps2 * (1.0 + tolerance),
            "Upload speed {} not within {}% tolerance of {}", metrics.upload_speed_bps, tolerance * 100.0, expected_upload_bps2
        );
        assert!(
            metrics.download_speed_bps >= expected_download_bps2 * (1.0 - tolerance) &&
            metrics.download_speed_bps <= expected_download_bps2 * (1.0 + tolerance),
            "Download speed {} not within {}% tolerance of {}", metrics.download_speed_bps, tolerance * 100.0, expected_download_bps2
        );
        assert_eq!(metrics.last_tx_bytes, 1500);
        assert_eq!(metrics.last_rx_bytes, 3000);
    }
}
