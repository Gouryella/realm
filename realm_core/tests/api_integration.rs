// Ensure this test file is part of the realm_core crate if api.rs was moved there.
// If api.rs is in its own new crate, this test would be for that crate.
// Assuming api.rs is now in realm_core.

use actix_web::{test, web, App};
use realm_core::monitor::{ConnectionMetrics, TCP_CONNECTION_METRICS, UDP_ASSOCIATION_METRICS};
use realm_core::api::{list_tcp_connections, get_tcp_connection_stats, list_udp_associations, get_udp_association_stats}; // Adjusted path
use std::sync::{Arc, Mutex};
use std::net::SocketAddr;
use uuid::Uuid;
use serde_json::Value;

fn setup_test_app() -> App<impl actix_web::dev::ServiceFactory<
    actix_web::dev::ServiceRequest,
    Config = (),
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
>> {
    App::new()
        .service(list_tcp_connections)
        .service(get_tcp_connection_stats)
        .service(list_udp_associations)
        .service(get_udp_association_stats)
}

#[actix_rt::test]
async fn test_tcp_stats_endpoints_integration() {
    TCP_CONNECTION_METRICS.clear();

    let conn_id1 = Uuid::new_v4().to_string();
    let metrics1 = Arc::new(Mutex::new(ConnectionMetrics::new()));
    metrics1.lock().unwrap().update_tx(1000);
    metrics1.lock().unwrap().update_rx(2000);
    // Note: calculate_speed() is not explicitly called here, so speeds might be 0 if no global task runs in test.
    // Uptime will be based on the Instant::now() in ConnectionMetrics::new().
    TCP_CONNECTION_METRICS.insert(conn_id1.clone(), metrics1.clone());

    let conn_id2 = Uuid::new_v4().to_string();
    let metrics2 = Arc::new(Mutex::new(ConnectionMetrics::new()));
    metrics2.lock().unwrap().update_tx(3000);
    TCP_CONNECTION_METRICS.insert(conn_id2.clone(), metrics2.clone());
    
    let srv = test::init_service(setup_test_app()).await;

    // Test GET /rules/tcp
    let req_list = test::TestRequest::get().uri("/rules/tcp").to_request();
    let resp_list: Vec<Value> = test::call_and_read_body_json(&srv, req_list).await;
    assert_eq!(resp_list.len(), 2, "Should list two TCP connections");

    let conn1_data = resp_list.iter().find(|x| x["id"] == conn_id1).expect("conn_id1 not found");
    assert_eq!(conn1_data["stats"]["tx_bytes"], 1000);
    assert_eq!(conn1_data["stats"]["rx_bytes"], 2000);
    assert!(conn1_data["stats"]["uptime_seconds"].as_u64().is_some());


    // Test GET /rules/tcp/{conn_id}/stats for conn_id1
    let req_conn1 = test::TestRequest::get().uri(&format!("/rules/tcp/{}/stats", conn_id1)).to_request();
    let resp_conn1: Value = test::call_and_read_body_json(&srv, req_conn1).await;
    assert_eq!(resp_conn1["tx_bytes"], 1000);

    // Test GET /rules/tcp/{conn_id}/stats for a non-existent ID
    let non_existent_id = Uuid::new_v4().to_string();
    let req_non_existent = test::TestRequest::get().uri(&format!("/rules/tcp/{}/stats", non_existent_id)).to_request();
    let resp_status_non_existent = test::call_service(&srv, req_non_existent).await;
    assert_eq!(resp_status_non_existent.status(), actix_web::http::StatusCode::NOT_FOUND);

    TCP_CONNECTION_METRICS.clear();
}

#[actix_rt::test]
async fn test_udp_stats_endpoints_integration() {
    UDP_ASSOCIATION_METRICS.clear();

    let addr1_str = "1.2.3.4:1234";
    let addr1: SocketAddr = addr1_str.parse().unwrap();
    let metrics_udp1 = Arc::new(Mutex::new(ConnectionMetrics::new()));
    metrics_udp1.lock().unwrap().update_tx(500);
    metrics_udp1.lock().unwrap().update_rx(1500);
    UDP_ASSOCIATION_METRICS.insert(addr1, metrics_udp1.clone());

    let srv_udp = test::init_service(setup_test_app()).await;

    // Test GET /rules/udp
    let req_list_udp = test::TestRequest::get().uri("/rules/udp").to_request();
    let resp_list_udp: Vec<Value> = test::call_and_read_body_json(&srv_udp, req_list_udp).await;
    assert_eq!(resp_list_udp.len(), 1);
    assert_eq!(resp_list_udp[0]["client_addr"], addr1_str);
    assert_eq!(resp_list_udp[0]["stats"]["tx_bytes"], 500);

    // Test GET /rules/udp/{client_addr}/stats for addr1
    let req_addr1 = test::TestRequest::get().uri(&format!("/rules/udp/{}/stats", addr1_str)).to_request();
    let resp_addr1: Value = test::call_and_read_body_json(&srv_udp, req_addr1).await;
    assert_eq!(resp_addr1["tx_bytes"], 500);

    // Test with invalid client_addr format
    let req_invalid_addr = test::TestRequest::get().uri("/rules/udp/invalid-addr/stats").to_request();
    let resp_status_invalid_addr = test::call_service(&srv_udp, req_invalid_addr).await;
    assert_eq!(resp_status_invalid_addr.status(), actix_web::http::StatusCode::BAD_REQUEST);
    
    UDP_ASSOCIATION_METRICS.clear();
}
