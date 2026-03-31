// ABOUTME: Configurable bearer token authentication middleware for Axum REST APIs
// ABOUTME: Reads API key from a caller-specified env var, allows unauthenticated when unset

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use subtle::ConstantTimeEq;

use crate::error::ErrorResponse;

/// Create an Axum middleware function that validates bearer tokens
///
/// Reads the API key from the given environment variable on every request
/// to allow runtime key rotation without restarting. If the variable is not
/// set or empty, all requests pass through (development mode).
///
/// # Usage
///
/// ```rust,ignore
/// use axum::middleware;
/// use dravr_tronc::server::auth::require_auth;
///
/// let app = Router::new()
///     .route("/api/endpoint", get(handler))
///     .layer(middleware::from_fn(|req, next| {
///         require_auth("MY_API_KEY_ENV", req, next)
///     }));
/// ```
pub async fn require_auth(env_var: &str, request: Request, next: Next) -> Response {
    let expected_key = match std::env::var(env_var) {
        Ok(key) if !key.is_empty() => key,
        _ => return next.run(request).await,
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header.as_bytes()["Bearer ".len()..];
            let expected = expected_key.as_bytes();
            if token.ct_eq(expected).into() {
                next.run(request).await
            } else {
                auth_error("Invalid API key")
            }
        }
        Some(_) => auth_error("Authorization header must use Bearer scheme"),
        None => auth_error("Missing Authorization header"),
    }
}

/// Build a 401 error response
fn auth_error(message: &str) -> Response {
    let body = ErrorResponse::new("authentication_error", message);
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::{middleware, Router};
    use http::Request as HttpRequest;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    // Each test uses a unique env var to avoid races in parallel execution
    async fn dummy_handler() -> &'static str {
        "ok"
    }

    fn make_app(env_var: &'static str) -> Router {
        Router::new()
            .route("/test", get(dummy_handler))
            .layer(middleware::from_fn(move |req, next| {
                require_auth(env_var, req, next)
            }))
    }

    #[tokio::test]
    async fn no_env_allows_all_requests() {
        const ENV: &str = "TRONC_AUTH_TEST_NO_ENV";
        std::env::remove_var(ENV);
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn empty_env_allows_all_requests() {
        const ENV: &str = "TRONC_AUTH_TEST_EMPTY";
        std::env::set_var(ENV, "");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
        std::env::remove_var(ENV);
    }

    #[tokio::test]
    async fn valid_bearer_token_passes() {
        const ENV: &str = "TRONC_AUTH_TEST_VALID";
        std::env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .header("authorization", "Bearer secret-key-123")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
        std::env::remove_var(ENV);
    }

    #[tokio::test]
    async fn invalid_bearer_token_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_INVALID";
        std::env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .header("authorization", "Bearer wrong-key")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 401);

        let bytes = resp.into_body().collect().await.expect("body").to_bytes(); // Safe: test assertion
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["error"]["type"], "authentication_error");
        std::env::remove_var(ENV);
    }

    #[tokio::test]
    async fn missing_header_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_MISSING";
        std::env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 401);
        std::env::remove_var(ENV);
    }

    #[tokio::test]
    async fn non_bearer_scheme_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_SCHEME";
        std::env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .header("authorization", "Basic dXNlcjpwYXNz")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 401);

        let bytes = resp.into_body().collect().await.expect("body").to_bytes(); // Safe: test assertion
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert!(json["error"]["message"]
            .as_str()
            .expect("msg") // Safe: test assertion
            .contains("Bearer"));
        std::env::remove_var(ENV);
    }
}
