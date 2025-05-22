use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
use crate::monitor::{ConnectionMetrics, TCP_CONNECTION_METRICS, UDP_ASSOCIATION_METRICS}; // Adjusted path
use serde::Serialize;
use std::net::SocketAddr;
// use std::sync::{Arc, Mutex}; // Not strictly required here as ConnectionMetrics is Clone and fields are public

// Structs used for API responses can remain private to this module
#[derive(Serialize, Debug)]
struct TrafficStatsResponse {
    tx_bytes: u64,
    rx_bytes: u64,
    upload_speed_bps: f64,
    download_speed_bps: f64,
    uptime_seconds: u64,
}

// Helper to create TrafficStatsResponse from ConnectionMetrics
// Assumes metrics are locked before calling this.
fn create_traffic_stats_response(metrics: &ConnectionMetrics) -> TrafficStatsResponse {
    TrafficStatsResponse {
        tx_bytes: metrics.traffic.tx_bytes,
        rx_bytes: metrics.traffic.rx_bytes,
        upload_speed_bps: metrics.upload_speed_bps,
        download_speed_bps: metrics.download_speed_bps,
        uptime_seconds: metrics.start_time.elapsed().as_secs(),
    }
}

#[derive(Serialize, Debug)]
struct TcpConnectionInfo {
    id: String,
    stats: TrafficStatsResponse,
}

#[derive(Serialize, Debug)]
struct UdpAssociationResponse {
    client_addr: String,
    stats: TrafficStatsResponse,
}

#[get("/rules/tcp")]
pub async fn list_tcp_connections() -> impl Responder {
    let mut conns = Vec::new();
    for entry in TCP_CONNECTION_METRICS.iter() {
        let key = entry.key();
        let metrics_arc = entry.value();
        if let Ok(metrics) = metrics_arc.lock() {
            conns.push(TcpConnectionInfo {
                id: key.clone(),
                stats: create_traffic_stats_response(&metrics),
            });
        } else {
            log::warn!("Failed to lock TCP metrics for API for key: {}", key);
        }
    }
    HttpResponse::Ok().json(conns)
}

#[get("/rules/tcp/{conn_id}/stats")]
pub async fn get_tcp_connection_stats(conn_id: web::Path<String>) -> impl Responder {
    let conn_id_str = conn_id.into_inner();
    if let Some(metrics_entry) = TCP_CONNECTION_METRICS.get(&conn_id_str) {
        let metrics_arc = metrics_entry.value();
        if let Ok(metrics) = metrics_arc.lock() {
            HttpResponse::Ok().json(create_traffic_stats_response(&metrics))
        } else {
            HttpResponse::InternalServerError().body(format!("Failed to lock TCP metrics for conn_id: {}", conn_id_str))
        }
    } else {
        HttpResponse::NotFound().body(format!("TCP Connection ID not found: {}", conn_id_str))
    }
}

#[get("/rules/udp")]
pub async fn list_udp_associations() -> impl Responder {
    let mut assocs = Vec::new();
    for entry in UDP_ASSOCIATION_METRICS.iter() {
        let client_socket_addr = entry.key();
        let metrics_arc = entry.value();
        if let Ok(metrics) = metrics_arc.lock() {
            assocs.push(UdpAssociationResponse {
                client_addr: client_socket_addr.to_string(),
                stats: create_traffic_stats_response(&metrics),
            });
        } else {
            log::warn!("Failed to lock UDP metrics for API for key: {:?}", client_socket_addr);
        }
    }
    HttpResponse::Ok().json(assocs)
}

#[get("/rules/udp/{client_addr}/stats")]
pub async fn get_udp_association_stats(client_addr_path: web::Path<String>) -> impl Responder {
    let client_addr_str = client_addr_path.into_inner();
    match client_addr_str.parse::<SocketAddr>() {
        Ok(client_addr) => {
            if let Some(metrics_entry) = UDP_ASSOCIATION_METRICS.get(&client_addr) {
                let metrics_arc = metrics_entry.value();
                if let Ok(metrics) = metrics_arc.lock() {
                    HttpResponse::Ok().json(create_traffic_stats_response(&metrics))
                } else {
                    HttpResponse::InternalServerError().body(format!("Failed to lock UDP metrics for client: {}", client_addr_str))
                }
            } else {
                HttpResponse::NotFound().body(format!("UDP Association not found for client address: {}", client_addr_str))
            }
        }
        Err(_) => HttpResponse::BadRequest().body(format!("Invalid client address format: {}", client_addr_str)),
    }
}
