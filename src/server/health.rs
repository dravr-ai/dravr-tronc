// ABOUTME: Health check trait and default Axum handler for server readiness probes
// ABOUTME: Projects implement HealthCheck to define their own readiness logic

use std::collections::HashMap;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

/// Health check response body
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Overall server status: "ok" or "degraded"
    pub status: String,
    /// Service name
    pub service: String,
    /// Service version
    pub version: String,
    /// Additional details (per-provider status, cache stats, etc.)
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub details: HashMap<String, String>,
}

impl HealthResponse {
    /// Create a healthy response with the given service name and version
    pub fn ok(service: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            status: "ok".to_owned(),
            service: service.into(),
            version: version.into(),
            details: HashMap::new(),
        }
    }

    /// Create a degraded response with the given service name and version
    pub fn degraded(service: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            status: "degraded".to_owned(),
            service: service.into(),
            version: version.into(),
            details: HashMap::new(),
        }
    }

    /// Add a detail entry
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Return the appropriate HTTP status code based on the health status
    pub fn status_code(&self) -> StatusCode {
        if self.status == "ok" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        }
    }

    /// Convert into an Axum response with the appropriate status code
    pub fn into_axum_response(self) -> impl IntoResponse {
        let code = self.status_code();
        (code, Json(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_response() {
        let resp = HealthResponse::ok("test-service", "1.0.0");
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.service, "test-service");
        assert_eq!(resp.version, "1.0.0");
        assert!(resp.details.is_empty());
        assert_eq!(resp.status_code(), StatusCode::OK);
    }

    #[test]
    fn degraded_response() {
        let resp = HealthResponse::degraded("test-service", "1.0.0");
        assert_eq!(resp.status, "degraded");
        assert_eq!(resp.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn with_details() {
        let resp = HealthResponse::ok("svc", "1.0")
            .with_detail("db", "connected")
            .with_detail("cache", "warm");
        assert_eq!(resp.details.len(), 2);
        assert_eq!(resp.details["db"], "connected");
        assert_eq!(resp.details["cache"], "warm");
    }

    #[test]
    fn ok_serializes_without_empty_details() {
        let resp = HealthResponse::ok("svc", "1.0");
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert!(json.get("details").is_none());
    }

    #[test]
    fn ok_serializes_with_details_when_present() {
        let resp = HealthResponse::ok("svc", "1.0").with_detail("key", "val");
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert_eq!(json["details"]["key"], "val");
    }

    #[test]
    fn degraded_serializes_correctly() {
        let resp = HealthResponse::degraded("svc", "1.0");
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert_eq!(json["status"], "degraded");
        assert_eq!(json["service"], "svc");
    }
}
