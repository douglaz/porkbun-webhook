use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Porkbun API error: {0}")]
    PorkbunApi(String),

    #[error("Rate limited by Porkbun API")]
    RateLimited,

    #[error("Porkbun upstream error: {0}")]
    PorkbunUpstream(String),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Domain not allowed: {0}")]
    DomainNotAllowed(String),

    #[error("Record not found: {0}")]
    RecordNotFound(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    #[allow(dead_code)]
    Internal(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Returns true if the error is transient and the request should be retried.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::RateLimited => true,
            Error::PorkbunUpstream(_) => true,
            Error::Network(e) => {
                // Timeouts and connection errors are transient
                e.is_timeout() || e.is_connect()
            }
            Error::PorkbunApi(_)
            | Error::Internal(_)
            | Error::Authentication(_)
            | Error::InvalidRequest(_)
            | Error::DomainNotAllowed(_)
            | Error::RecordNotFound(_)
            | Error::Configuration(_)
            | Error::Json(_)
            | Error::Other(_) => false,
        }
    }

    /// Maps the error to an HTTP status code.
    pub fn status_code(&self) -> StatusCode {
        if self.is_transient() {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        match self {
            Error::PorkbunApi(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::RateLimited => StatusCode::SERVICE_UNAVAILABLE,
            Error::PorkbunUpstream(_) => StatusCode::SERVICE_UNAVAILABLE,
            Error::Authentication(_) => StatusCode::FORBIDDEN,
            Error::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Error::DomainNotAllowed(_) => StatusCode::FORBIDDEN,
            Error::RecordNotFound(_) => StatusCode::NOT_FOUND,
            Error::Configuration(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Network(_) => StatusCode::SERVICE_UNAVAILABLE,
            Error::Json(_) => StatusCode::BAD_REQUEST,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_message = self.to_string();

        let body = Json(json!({
            "error": error_message,
            "status": status.as_u16(),
        }));

        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_is_transient() {
        let err = Error::RateLimited;
        assert!(err.is_transient());
        assert_eq!(err.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn auth_error_is_permanent() {
        let err = Error::Authentication("invalid API key".to_string());
        assert!(!err.is_transient());
        assert_eq!(err.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn domain_not_allowed_is_permanent() {
        let err = Error::DomainNotAllowed("evil.com".to_string());
        assert!(!err.is_transient());
        assert_eq!(err.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn invalid_request_is_permanent() {
        let err = Error::InvalidRequest("bad payload".to_string());
        assert!(!err.is_transient());
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn porkbun_api_non_rate_limit_is_not_transient() {
        let err = Error::PorkbunApi("invalid domain".to_string());
        assert!(!err.is_transient());
        assert_eq!(err.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn porkbun_upstream_5xx_is_transient() {
        let err = Error::PorkbunUpstream("HTTP 500: internal server error".to_string());
        assert!(err.is_transient());
        assert_eq!(err.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
