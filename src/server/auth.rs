// ABOUTME: Configurable bearer token authentication middleware for Axum REST APIs
// ABOUTME: Reads API key from a caller-specified env var, allows unauthenticated when unset
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Shared-bearer-key middleware for Axum, and the startup posture check.
//!
//! [`require_auth`] **fails open**: with its environment variable unset every
//! request passes through. That is deliberate and pinned by tests — stdio and
//! local runs depend on it — and it is why [`resolve_startup_auth`] exists, to
//! refuse a reachable bind that nothing gates.
//!
//! **Which mechanism?** This crate ships five, and the choice is made by what
//! the caller can present, per route — not by which one has no feature flag.
//! The table is in the crate README under "Choosing an auth mechanism"; read it
//! before reaching for a shared bearer key, which is the wrong answer whenever
//! the caller is another Google workload, a browser session, or a webhook.

use std::env;
use std::error::Error;
use std::fmt;
use std::net::IpAddr;

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
    let expected_key = match env::var(env_var) {
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

/// Whether `env_var` holds a usable API key.
///
/// The same rule [`require_auth`] applies: unset **or** empty means no key. One
/// definition, so a caller cannot decide "configured" differently from the
/// middleware that enforces it.
#[must_use]
pub fn api_key_configured(env_var: &str) -> bool {
    matches!(env::var(env_var), Ok(key) if !key.is_empty())
}

/// The authentication posture a server resolved at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// A key is set, so every request is authenticated.
    Enforced,
    /// No key, but the bind is loopback-only — unauthenticated local development.
    ///
    /// A caller should say so at startup. Silence here is how a machine ends up
    /// serving without a key and nobody noticing.
    LoopbackDev,
}

/// A server was asked to serve a reachable interface with no key.
///
/// [`require_auth`] fails **open** when its key is unset — deliberately, so a
/// developer can run without ceremony, and unchanged here because it is a
/// documented contract with reverse dependencies this workspace cannot
/// enumerate. That default is survivable on loopback and not on an interface
/// something else can reach.
///
/// This is the other half: a startup check the binary opts into, because
/// deciding to terminate a process belongs to `main`, not to a library. The
/// middleware cannot make this call itself — it sees a request, never the bind
/// address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsecureBindError {
    /// The non-loopback host the server was asked to bind.
    pub host: String,
}

impl fmt::Display for InsecureBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing to start: nothing authenticates requests while binding non-loopback host \
             '{}'. Arm this service's gate, or bind 127.0.0.1 for local development.",
            self.host
        )
    }
}

impl Error for InsecureBindError {}

/// Whether `host` names a loopback interface.
///
/// Accepts `localhost` in any case, `127.0.0.0/8`, `::1`, and `::1` in its
/// bracketed form. **Everything else is non-loopback, including `0.0.0.0`, `::`
/// and any name this cannot parse** — an unresolvable name is treated as
/// reachable so the check fails closed rather than guessing in the caller's
/// favour.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim();
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let stripped = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    stripped.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Resolve the startup posture for a bind, or refuse it.
///
/// - gated → [`AuthMode::Enforced`], whatever the host
/// - ungated, loopback host → [`AuthMode::LoopbackDev`]
/// - ungated, reachable host → [`InsecureBindError`], and the server must not start
///
/// `gated` is **whether anything at all will authenticate requests on this
/// bind** — not whether a shared key is set. That distinction is the whole
/// point of the parameter. A service gated by Google ID tokens holds no key
/// and is correctly secured; an earlier version of this function asked only
/// `api_key_configured`, which would have refused to start photograveur,
/// sciotte and the platform backend — the three services in the fleet that got
/// their auth most right. Every Cloud Run container must bind `0.0.0.0`, so a
/// check that reads a reachable bind as unsafe on its own is wrong about the
/// entire deployed estate.
///
/// Compose it with whatever actually gates you:
///
/// ```rust,ignore
/// // shared key
/// resolve_startup_auth(&args.host, api_key_configured("CAGEUX_API_TOKEN"))?;
/// // Google ID tokens — no key to check, the middleware is the gate
/// resolve_startup_auth(&args.host, true)?;
/// ```
///
/// Call this in `main` before binding, and propagate the error. Nothing here
/// exits the process on the caller's behalf: terminating belongs to the binary.
///
/// # Errors
///
/// Returns [`InsecureBindError`] when nothing gates the service and `host` is
/// not loopback.
pub fn resolve_startup_auth(host: &str, gated: bool) -> Result<AuthMode, InsecureBindError> {
    if gated {
        Ok(AuthMode::Enforced)
    } else if is_loopback_host(host) {
        Ok(AuthMode::LoopbackDev)
    } else {
        Err(InsecureBindError {
            host: host.to_owned(),
        })
    }
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
        env::remove_var(ENV);
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
        env::set_var(ENV, "");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
        env::remove_var(ENV);
    }

    #[tokio::test]
    async fn valid_bearer_token_passes() {
        const ENV: &str = "TRONC_AUTH_TEST_VALID";
        env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .header("authorization", "Bearer secret-key-123")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
        env::remove_var(ENV);
    }

    #[tokio::test]
    async fn invalid_bearer_token_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_INVALID";
        env::set_var(ENV, "secret-key-123");
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
        env::remove_var(ENV);
    }

    #[tokio::test]
    async fn missing_header_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_MISSING";
        env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 401);
        env::remove_var(ENV);
    }

    #[tokio::test]
    async fn non_bearer_scheme_returns_401() {
        const ENV: &str = "TRONC_AUTH_TEST_SCHEME";
        env::set_var(ENV, "secret-key-123");
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
        env::remove_var(ENV);
    }

    // ---- startup posture ----

    #[test]
    fn loopback_hosts_are_recognised() {
        for host in [
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
            "localhost",
            "LocalHost",
        ] {
            assert!(is_loopback_host(host), "{host} should be loopback");
        }
    }

    #[test]
    fn anything_reachable_or_unparseable_is_not_loopback() {
        // 0.0.0.0 and :: are the wildcards that bind every interface, and an
        // unresolvable name is treated as reachable so the check fails closed
        // rather than guessing in the caller's favour.
        for host in ["0.0.0.0", "::", "192.168.1.10", "example.com", "", "   "] {
            assert!(
                !is_loopback_host(host),
                "{host:?} must not read as loopback"
            );
        }
    }

    #[test]
    fn a_reachable_bind_with_nothing_gating_it_is_refused() {
        let err = resolve_startup_auth("0.0.0.0", false)
            .expect_err("a reachable bind with no gate must be refused"); // Safe: test assertion

        assert_eq!(err.host, "0.0.0.0");
        let msg = err.to_string();
        assert!(msg.contains("refusing to start"), "message was {msg:?}");
        assert!(
            msg.contains("0.0.0.0"),
            "the message must name the bind that was refused; message was {msg:?}"
        );
    }

    #[test]
    fn an_identity_gated_service_binds_a_reachable_host_holding_no_key() {
        // The regression this pins: an earlier draft resolved posture from
        // `api_key_configured` alone. Every Cloud Run container binds 0.0.0.0,
        // and the three services in the fleet with the strongest auth —
        // photograveur, sciotte, the platform backend — gate on Google ID
        // tokens and hold no API key at all. Under the key-only rule each one
        // refuses to start. `gated` is "something authenticates requests",
        // whatever that something is.
        const ENV: &str = "TRONC_AUTH_TEST_POSTURE_NO_KEY_HERE";
        env::remove_var(ENV);
        assert!(!api_key_configured(ENV));

        assert_eq!(
            resolve_startup_auth("0.0.0.0", true),
            Ok(AuthMode::Enforced)
        );
    }

    #[test]
    fn loopback_without_a_gate_is_development_not_a_refusal() {
        assert_eq!(
            resolve_startup_auth("127.0.0.1", false),
            Ok(AuthMode::LoopbackDev)
        );
    }

    #[test]
    fn a_gate_enforces_on_every_host() {
        assert_eq!(
            resolve_startup_auth("0.0.0.0", true),
            Ok(AuthMode::Enforced)
        );
        assert_eq!(
            resolve_startup_auth("127.0.0.1", true),
            Ok(AuthMode::Enforced)
        );
    }

    #[test]
    fn an_empty_key_is_no_key_here_too() {
        // require_auth treats unset and empty alike; a caller composing
        // api_key_configured into resolve_startup_auth must agree, or a server
        // boots believing it is Enforced while every request sails through.
        const ENV: &str = "TRONC_AUTH_TEST_POSTURE_EMPTY";
        env::set_var(ENV, "");
        assert!(!api_key_configured(ENV));
        assert!(resolve_startup_auth("0.0.0.0", api_key_configured(ENV)).is_err());
        env::remove_var(ENV);
    }
}
