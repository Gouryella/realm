use actix_web::{
    body::BoxBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    get, post, web, Error, HttpResponse, Responder,
};
use futures::future::{ok, LocalBoxFuture, Ready};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, rc::Rc, time::Instant};

use crate::{
    endpoint::{EndpointConf, EndpointInfo},
    monitor::{ConnectionMetrics, TCP_CONNECTION_METRICS, UDP_ASSOCIATION_METRICS},
};
use log::{info, warn};
use tokio::sync::mpsc;
use dashmap::DashMap;

// Channel for sending new endpoints to the main runtime
lazy_static::lazy_static! {
    pub static ref ENDPOINT_SENDER: DashMap<String, mpsc::Sender<EndpointInfo>> = DashMap::new();
}

#[derive(Debug, Deserialize)]
pub struct AddEndpointRequest {
    pub endpoint: EndpointConf,
}

const WWW_AUTHENTICATE_HEADER: &str = "Bearer realm=\"Realm API\"";

/// --------- Request Logger Middleware ---------

pub struct RequestLogger;

impl<S> Transform<S, ServiceRequest> for RequestLogger
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = RequestLoggerMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(RequestLoggerMiddleware {
            service: Rc::new(service),
        })
    }
}

pub struct RequestLoggerMiddleware<S> {
    service: Rc<S>,
}

impl<S> Service<ServiceRequest> for RequestLoggerMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let method = req.method().to_string();
        let path = req.path().to_string();
        let start_time = Instant::now();

        info!("API Request: {} {}", method, path);
        // Log request headers (excluding Authorization for security)
        for (header_name, header_value) in req.headers() {
            if header_name != "authorization" {
                if let Ok(value_str) = header_value.to_str() {
                    info!("  Header: {}: {}", header_name, value_str);
                }
            }
        }

        let fut = self.service.call(req);

        Box::pin(async move {
            let res = fut.await?;
            let duration = start_time.elapsed();
            info!(
                "API Response: {} {} - Status: {} - Duration: {:?}",
                method,
                path,
                res.status(),
                duration
            );
            // Log response headers
            for (header_name, header_value) in res.headers() {
                if let Ok(value_str) = header_value.to_str() {
                    info!("  Response Header: {}: {}", header_name, value_str);
                }
            }
            Ok(res)
        })
    }
}

/// --------- Common Structs ---------

#[derive(Serialize, Debug)]
struct TrafficStatsResponse {
    tx_bytes: u64,
    rx_bytes: u64,
    upload_speed_bps: f64,
    download_speed_bps: f64,
    uptime_seconds: u64,
}

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

/// --------- API for managing Realm rules ---------

#[post("/rules")]
pub async fn add_rule(req: web::Json<AddEndpointRequest>) -> impl Responder {
    let endpoint_conf = req.into_inner().endpoint;
    let endpoint_id = endpoint_conf.endpoint.clone(); // Assuming endpoint has a unique ID or can be used as one

    info!("Attempting to add new rule: {:?}", endpoint_conf);

    let endpoint_info = match endpoint_conf.build() {
        Ok(info) => info,
        Err(e) => {
            warn!("Failed to build endpoint from config: {}", e);
            return HttpResponse::BadRequest().body(format!("Invalid endpoint configuration: {}", e));
        }
    };

    // Check if a sender for this endpoint already exists
    if ENDPOINT_SENDER.contains_key(&endpoint_id) {
        warn!("Rule with ID '{}' already exists.", endpoint_id);
        return HttpResponse::Conflict().body(format!("Rule with ID '{}' already exists.", endpoint_id));
    }

    // Create a new channel for this endpoint
    let (tx, rx) = mpsc::channel::<EndpointInfo>(1); // Buffer size 1, as we only send one endpoint at a time

    // Store the sender in the global map
    ENDPOINT_SENDER.insert(endpoint_id.clone(), tx);

    // Spawn a task to receive the endpoint and start it
    tokio::spawn(async move {
        if let Some(ep_info) = rx.recv().await {
            info!("Starting new endpoint: {}", ep_info.endpoint);
            use realm::core::tcp::run_tcp;
            use realm::core::udp::run_udp;

            if ep_info.use_udp {
                tokio::spawn(run_udp(ep_info.clone()));
            }

            if !ep_info.no_tcp {
                tokio::spawn(run_tcp(ep_info));
            }
        } else {
            warn!("Endpoint sender for {} was dropped before sending.", endpoint_id);
        }
    });

    // Send the endpoint info through the channel
    if let Err(e) = ENDPOINT_SENDER.get(&endpoint_id).unwrap().send(endpoint_info).await {
        warn!("Failed to send endpoint to worker: {}", e);
        ENDPOINT_SENDER.remove(&endpoint_id); // Clean up the sender if sending fails
        return HttpResponse::InternalServerError().body("Failed to activate new rule.");
    }

    HttpResponse::Created().body(format!("Rule '{}' added successfully.", endpoint_id))
}

#[actix_web::delete("/rules/{endpoint_id}")]
pub async fn delete_rule(endpoint_id: web::Path<String>) -> impl Responder {
    let id = endpoint_id.into_inner();
    info!("Attempting to delete rule with ID: {}", id);

    if ENDPOINT_SENDER.remove(&id).is_some() {
        info!("Rule '{}' deleted successfully.", id);
        HttpResponse::Ok().body(format!("Rule '{}' deleted successfully.", id))
    } else {
        warn!("Rule with ID '{}' not found for deletion.", id);
        HttpResponse::NotFound().body(format!("Rule with ID '{}' not found.", id))
    }
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
            warn!("Failed to lock TCP metrics for API for key: {}", key);
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
            HttpResponse::InternalServerError()
                .body(format!("Failed to lock TCP metrics for conn_id: {}", conn_id_str))
        }
    } else {
        HttpResponse::NotFound().body(format!("TCP Connection ID not found: {}", conn_id_str))
    }
}

/// --------- UDP 相关 API ---------

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
            warn!("Failed to lock UDP metrics for API for key: {:?}", client_socket_addr);
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
                    HttpResponse::InternalServerError()
                        .body(format!("Failed to lock UDP metrics for client: {}", client_addr_str))
                }
            } else {
                HttpResponse::NotFound()
                    .body(format!("UDP Association not found for client address: {}", client_addr_str))
            }
        }
        Err(_) => HttpResponse::BadRequest().body(format!("Invalid client address format: {}", client_addr_str)),
    }
}

/// --------- 鉴权中间件 ---------

pub struct Authenticate {
    expected_token: Option<String>,
}

impl Authenticate {
    pub fn new(expected_token: Option<String>) -> Self {
        Self { expected_token }
    }
}

impl<S> Transform<S, ServiceRequest> for Authenticate
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(AuthMiddleware {
            service: Rc::new(service),
            expected_token: self.expected_token.clone(),
        })
    }
}

pub struct AuthMiddleware<S> {
    service: Rc<S>,
    expected_token: Option<String>,
}

impl<S> Service<ServiceRequest> for AuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // 如果服务器未配置 token，直接放行
        let Some(expected) = &self.expected_token else {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await });
        };

        // 解析 Authorization: Bearer <token>
        if let Some(auth_header) = req.headers().get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str.starts_with("Bearer ") {
                    let token = &auth_str["Bearer ".len()..];
                    if token == expected {
                        let fut = self.service.call(req);
                        return Box::pin(async move { fut.await });
                    }
                }
            }
        }

        // 缺失或错误的 token，返回 401
        Box::pin(async move {
            Ok(req.into_response(
                HttpResponse::Unauthorized()
                    .insert_header(("WWW-Authenticate", WWW_AUTHENTICATE_HEADER))
                    .finish(),
            ))
        })
    }
}
