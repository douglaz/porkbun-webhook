use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// Whether request body tracing is enabled. Set at startup based on config.
static TRACE_BODIES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Call once at startup to configure body tracing.
pub fn set_trace_request_bodies(enabled: bool) {
    TRACE_BODIES.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn trace_bodies_enabled() -> bool {
    TRACE_BODIES.load(std::sync::atomic::Ordering::Relaxed)
}

pub async fn logging_middleware(request: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();

    info!(
        method = %method,
        path = %path,
        query = ?uri.query(),
        "Incoming request"
    );

    // Only intercept body for POST /records when tracing is enabled
    if method == "POST" && path == "/records" && trace_bodies_enabled() {
        let (parts, body) = request.into_parts();

        // 16 MiB limit — generous enough to never reject legitimate ExternalDNS
        // payloads while still bounding memory usage.
        let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(err) => {
                error!("Failed to read request body: {err}");
                return (StatusCode::BAD_REQUEST, "Failed to read request body").into_response();
            }
        };

        let body_str = String::from_utf8_lossy(&bytes);
        // Cap logged body size to avoid flooding logs.
        // Note: request bodies may contain sensitive data (e.g. TXT record values).
        // Consider disabling TRACE_REQUEST_BODIES in production.
        let truncated = if body_str.len() > 4096 {
            // Find a safe UTF-8 character boundary at or before 4096 bytes
            let mut end = 4096.min(body_str.len());
            while end > 0 && !body_str.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}... (truncated, {} bytes total)",
                &body_str[..end],
                body_str.len()
            )
        } else {
            body_str.to_string()
        };
        debug!("Request body for POST /records: {truncated}");

        let request = Request::from_parts(parts, Body::from(bytes));
        let response = next.run(request).await;
        let duration = start.elapsed();
        let status = response.status();

        log_completion(&method, &path, status, duration);
        response
    } else {
        let response = next.run(request).await;
        let duration = start.elapsed();
        let status = response.status();

        log_completion(&method, &path, status, duration);
        response
    }
}

fn log_completion(
    method: &axum::http::Method,
    path: &str,
    status: StatusCode,
    duration: std::time::Duration,
) {
    if status.is_client_error() || status.is_server_error() {
        warn!(
            method = %method,
            path = %path,
            status = %status,
            duration_ms = %duration.as_millis(),
            "Request failed"
        );
    } else {
        info!(
            method = %method,
            path = %path,
            status = %status,
            duration_ms = %duration.as_millis(),
            "Request completed"
        );
    }
}

pub async fn error_handling_middleware(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
        error!("422 Unprocessable Entity - likely JSON deserialization issue");
    }

    response
}
