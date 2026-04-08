use axum::http::StatusCode;
use axum::Router;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use porkbun_webhook::{Config, PorkbunClient};

fn test_config(base_url: &str) -> Config {
    Config {
        porkbun_api_key: "pk1_test".to_string(),
        porkbun_secret_api_key: "sk1_test".to_string(),
        porkbun_api_base: base_url.to_string(),
        webhook_host: "127.0.0.1".to_string(),
        webhook_port: 8888,
        domain_filter: Some(vec!["example.com".to_string()]),
        dry_run: false,
        cache_ttl_seconds: 60,
        http_timeout_seconds: 30,
        trace_request_bodies: false,
    }
}

fn build_app(base_url: &str) -> Router {
    let config = test_config(base_url);
    let client = PorkbunClient::new(
        &config.porkbun_api_key,
        &config.porkbun_secret_api_key,
        &config.porkbun_api_base,
        Duration::from_secs(30),
    )
    .expect("client should build");
    porkbun_webhook::webhook::routes::create_routes(client, config)
}

fn json_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> axum::http::Request<axum::body::Body> {
    let builder = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");

    match body {
        Some(b) => builder
            .body(axum::body::Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(axum::body::Body::empty()).unwrap(),
    }
}

// --- GET / (negotiate) ---

#[tokio::test]
async fn negotiate_returns_filters() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("application/external.dns.webhook+json"),
        "unexpected content-type: {content_type}"
    );

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["filters"], json!(["example.com"]));
}

// --- GET /healthz ---

#[tokio::test]
async fn healthz_returns_healthy() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "healthy");
}

// --- GET /ready ---

#[tokio::test]
async fn ready_succeeds_with_valid_ping() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "yourIp": "1.2.3.4"
        })))
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/ready")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ready");
}

#[tokio::test]
async fn ready_fails_with_bad_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ERROR",
            "message": "Invalid API key"
        })))
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/ready")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // Authentication errors should return 403
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// --- GET /records ---

#[tokio::test]
async fn get_records_with_zone_returns_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "records": [
                {
                    "id": "100",
                    "name": "www.example.com",
                    "type": "A",
                    "content": "1.2.3.4",
                    "ttl": "600",
                    "prio": null
                },
                {
                    "id": "101",
                    "name": "www.example.com",
                    "type": "A",
                    "content": "5.6.7.8",
                    "ttl": "600",
                    "prio": null
                }
            ]
        })))
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/records?zone=example.com")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let endpoints: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    // Two A records for www should be grouped into one endpoint
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0]["dnsName"], "www.example.com");
    let targets = endpoints[0]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
}

#[tokio::test]
async fn get_records_rejects_non_managed_zone() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/records?zone=evil.com")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_records_rejects_subdomain_as_zone() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    // www.example.com is a subdomain, not a managed zone
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/records?zone=www.example.com")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// --- POST /records (apply_changes) ---

#[tokio::test]
async fn apply_changes_empty_returns_204() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": [],
            "updateOld": [],
            "updateNew": [],
            "delete": []
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn apply_changes_create_calls_porkbun() {
    let server = MockServer::start().await;

    // Mock list_records for idempotency check
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "records": []
        })))
        .mount(&server)
        .await;

    // Mock create
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "id": 999
        })))
        .expect(1)
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["10.0.0.1"],
                "recordType": "A",
                "recordTTL": 600
            }]
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn apply_changes_create_is_idempotent() {
    let server = MockServer::start().await;

    // Record already exists
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "records": [{
                "id": "100",
                "name": "app.example.com",
                "type": "A",
                "content": "10.0.0.1",
                "ttl": "600",
                "prio": null
            }]
        })))
        .mount(&server)
        .await;

    // Create should NOT be called
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "id": 999
        })))
        .expect(0)
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["10.0.0.1"],
                "recordType": "A",
                "recordTTL": 600
            }]
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn apply_changes_delete_calls_porkbun() {
    let server = MockServer::start().await;

    // Record exists
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "records": [{
                "id": "100",
                "name": "app.example.com",
                "type": "A",
                "content": "10.0.0.1",
                "ttl": "600",
                "prio": null
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/dns/delete/example.com/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let app = build_app(&server.uri());
    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "delete": [{
                "dnsName": "app.example.com",
                "targets": ["10.0.0.1"],
                "recordType": "A"
            }]
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn apply_changes_rejects_unsupported_type() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["ns1.example.com"],
                "recordType": "NS"
            }]
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn apply_changes_rejects_disallowed_domain() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": [{
                "dnsName": "app.evil.com",
                "targets": ["1.2.3.4"],
                "recordType": "A"
            }]
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// --- POST /adjustendpoints ---

#[tokio::test]
async fn adjust_endpoints_passthrough() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    let req = json_request(
        "POST",
        "/adjustendpoints",
        Some(json!([
            {
                "dnsName": "app.example.com",
                "targets": ["1.2.3.4"],
                "recordType": "A"
            }
        ])),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let endpoints: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0]["dnsName"], "app.example.com");
}

// --- null-as-empty compatibility ---

#[tokio::test]
async fn apply_changes_null_arrays_returns_204() {
    let server = MockServer::start().await;
    let app = build_app(&server.uri());

    // Go clients may serialize nil slices as JSON null
    let req = json_request(
        "POST",
        "/records",
        Some(json!({
            "create": null,
            "updateOld": null,
            "updateNew": null,
            "delete": null
        })),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
