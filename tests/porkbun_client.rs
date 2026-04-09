use porkbun_webhook::{Error, PorkbunClient};
use pretty_assertions::assert_eq;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn test_client(base_url: &str) -> PorkbunClient {
    PorkbunClient::new("pk1_test", "sk1_test", base_url, Duration::from_secs(5))
        .expect("client should build")
}

// --- Ping ---

#[tokio::test]
async fn ping_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "yourIp": "1.2.3.4"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let resp = client.ping().await.expect("ping should succeed");
    assert_eq!(resp.your_ip, Some("1.2.3.4".to_string()));
}

#[tokio::test]
async fn ping_auth_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Invalid API key"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client.ping().await.expect_err("ping should fail");
    assert!(matches!(err, Error::Authentication(_)));
}

#[tokio::test]
async fn ping_generic_error_returns_porkbun_api() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Some other error"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client.ping().await.expect_err("ping should fail");
    // Generic errors should not be classified as Authentication
    assert!(
        matches!(err, Error::PorkbunApi(_)),
        "expected PorkbunApi, got: {err:?}"
    );
}

// --- List domains ---

#[tokio::test]
async fn list_domains_single_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/domain/listAll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "domains": [
                {"domain": "example.com"},
                {"domain": "other.org"}
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let domains = client
        .list_domains()
        .await
        .expect("list_domains should succeed");
    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].domain, "example.com");
    assert_eq!(domains[1].domain, "other.org");
}

#[tokio::test]
async fn list_domains_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/domain/listAll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "domains": []
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let domains = client
        .list_domains()
        .await
        .expect("list_domains should succeed");
    assert!(domains.is_empty());
}

// --- List records ---

#[tokio::test]
async fn list_records_returns_records() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
                    "name": "example.com",
                    "type": "MX",
                    "content": "mail.example.com",
                    "ttl": "3600",
                    "prio": "10"
                }
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let records = client
        .list_records("example.com")
        .await
        .expect("list_records should succeed");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, "100");
    assert_eq!(records[0].content, "1.2.3.4");
    assert_eq!(records[1].prio_u32(), Some(10));
}

// --- Add record ---

#[tokio::test]
async fn add_record_returns_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "id": 12345
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let params = porkbun_webhook::porkbun::CreateDnsParams {
        subdomain: "www".to_string(),
        record_type: "A".to_string(),
        content: "1.2.3.4".to_string(),
        ttl: Some(600),
        prio: None,
    };
    let id = client
        .add_record("example.com", params)
        .await
        .expect("add_record should succeed");
    assert_eq!(id, "12345");
}

// --- Edit record ---

#[tokio::test]
async fn edit_record_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/edit/example.com/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let params = porkbun_webhook::porkbun::EditDnsParams {
        subdomain: "www".to_string(),
        record_type: "A".to_string(),
        content: "5.6.7.8".to_string(),
        ttl: Some(300),
        prio: None,
    };
    client
        .edit_record("example.com", "100", params)
        .await
        .expect("edit_record should succeed");
}

// --- Remove record ---

#[tokio::test]
async fn remove_record_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/delete/example.com/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    client
        .remove_record("example.com", "100")
        .await
        .expect("remove_record should succeed");
}

#[tokio::test]
async fn remove_record_already_deleted_via_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/delete/example.com/999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Record not found"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    // Should succeed (idempotent delete)
    client
        .remove_record("example.com", "999")
        .await
        .expect("already-deleted record should not error");
}

#[tokio::test]
async fn remove_record_already_deleted_via_http_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/delete/example.com/999"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Record does not exist"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    // Should succeed — post_json returns RecordNotFound, remove_record absorbs it
    client
        .remove_record("example.com", "999")
        .await
        .expect("already-deleted record via HTTP 400 should not error");
}

// --- Error classification ---

#[tokio::test]
async fn rate_limit_429_returns_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client.ping().await.expect_err("should fail with 429");
    assert!(matches!(err, Error::RateLimited));
    assert!(err.is_transient());
}

#[tokio::test]
async fn unauthorized_401_returns_authentication() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "message": "Unauthorized"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client.ping().await.expect_err("should fail with 401");
    assert!(matches!(err, Error::Authentication(_)));
    assert!(!err.is_transient());
}

#[tokio::test]
async fn bad_api_key_400_returns_authentication() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Invalid API key."
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client
        .ping()
        .await
        .expect_err("should fail with bad api key");
    assert!(
        matches!(err, Error::Authentication(_)),
        "expected Authentication, got: {err:?}"
    );
}

#[tokio::test]
async fn server_error_500_is_transient() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "message": "Internal server error"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client.ping().await.expect_err("should fail with 500");
    assert!(matches!(err, Error::PorkbunUpstream(_)));
    assert!(err.is_transient());
}

#[tokio::test]
async fn forbidden_403_domain_issue_returns_domain_not_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "API access is not enabled for this domain"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client
        .list_records("example.com")
        .await
        .expect_err("should fail with 403");
    // Domain-related 403s should be DomainNotAllowed (→403), not Authentication
    assert!(
        matches!(err, Error::DomainNotAllowed(_)),
        "expected DomainNotAllowed, got: {err:?}"
    );
}

#[tokio::test]
async fn timeout_is_transient() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;

    // Client with 1 second timeout
    let client = PorkbunClient::new(
        "pk1_test",
        "sk1_test",
        &server.uri(),
        Duration::from_secs(1),
    )
    .expect("client should build");

    let err = client.ping().await.expect_err("should timeout");
    assert!(matches!(err, Error::Network(_)));
    assert!(err.is_transient());
}

#[tokio::test]
async fn list_records_inband_auth_error_returns_authentication() {
    // Porkbun returns HTTP 200 with {"status":"ERROR","message":"Invalid API key"}
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Invalid API key."
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client
        .list_records("example.com")
        .await
        .expect_err("should fail with in-band auth error");
    assert!(
        matches!(err, Error::Authentication(_)),
        "expected Authentication, got: {err:?}"
    );
}

#[tokio::test]
async fn list_records_inband_generic_error_returns_porkbun_api() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieve/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ERROR",
            "message": "Some other error"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server.uri()).await;
    let err = client
        .list_records("example.com")
        .await
        .expect_err("should fail with generic error");
    assert!(
        matches!(err, Error::PorkbunApi(_)),
        "expected PorkbunApi, got: {err:?}"
    );
}
