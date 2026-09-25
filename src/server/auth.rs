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
//! refuse a reachable bind that nothing gates. [`startup_auth`] is that check
//! for the common case, a service gated by one shared key, with the posture
//! logged: every satellite used to carry its own copy of those ten lines.
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
use tracing::{info, warn};

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

    match auth_header.map(bearer_credential) {
        Some(Some(token)) => {
            if token.as_bytes().ct_eq(expected_key.as_bytes()).into() {
                next.run(request).await
            } else {
                auth_error("Invalid API key")
            }
        }
        Some(None) => auth_error("Authorization header must use Bearer scheme"),
        None => auth_error("Missing Authorization header"),
    }
}

/// The credential carried by an `Authorization` header value that uses the
/// `Bearer` scheme, or `None` for any other scheme or an empty credential.
///
/// The scheme is matched case-insensitively — RFC 7235 §2.1 makes every
/// auth-scheme case-insensitive, so `bearer x` and `BEARER x` present the same
/// credential as `Bearer x` — and the one or more spaces separating it from the
/// credential are consumed. Every bearer check in this crate reads the header
/// through this function, so no two of them can disagree about which forms
/// authenticate.
#[must_use]
pub fn bearer_credential(header_value: &str) -> Option<&str> {
    let (scheme, credential) = header_value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let credential = credential.trim_start_matches(' ');
    (!credential.is_empty()).then_some(credential)
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
    /// The environment variable that arms the gate, when the gate is a shared
    /// key, so the refusal can say what to set. `None` when the gate is
    /// something else (an [`AuthHook`](crate::mcp::auth::AuthHook), Google ID
    /// tokens) and there is no single variable to name.
    pub gate: Option<String>,
}

impl InsecureBindError {
    /// A refusal of `host` for a service whose gate is not a single variable.
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            gate: None,
        }
    }
}

impl fmt::Display for InsecureBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing to start: nothing authenticates requests while binding non-loopback host \
             '{}'. ",
            self.host
        )?;
        match &self.gate {
            Some(var) => write!(
                f,
                "Set {var} (every request must then carry it as a bearer token), or bind \
                 127.0.0.1 for local development."
            ),
            None => {
                f.write_str("Arm this service's gate, or bind 127.0.0.1 for local development.")
            }
        }
    }
}

impl Error for InsecureBindError {}

/// Whether `host` names a loopback interface.
///
/// Accepts `localhost` in any case, `127.0.0.0/8`, `::1`, and `::1` in its
/// bracketed form. **Everything else is non-loopback, including `0.0.0.0`, `::`
/// and any other name** — a name this cannot classify from its text is treated
/// as reachable so the check fails closed rather than guessing in the caller's
/// favour.
///
/// `localhost` counts as loopback because RFC 6761 §6.3 reserves it for the
/// loopback interface, and browsers hard-wire it there: an origin of
/// `http://localhost:3000` is a page on this machine. A name is still only a
/// name, though. Binding one goes through the system resolver, which answers
/// `localhost` with `127.0.0.1`, `::1`, or both — and, on a host whose resolver
/// is configured otherwise, with anything at all. So this is a judgement of the
/// text, made before any socket exists; [`crate::mcp::transport::http::serve`]
/// goes further and judges every address the name resolves to before it binds.
/// A binary that binds its own listener and wants the same certainty checks
/// `local_addr()` of what it bound.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim();
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let stripped = trimmed.strip_circumfix('[', ']').unwrap_or(trimmed);
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
        Err(InsecureBindError::new(host))
    }
}

/// Resolve the startup posture of a service gated by the shared bearer key in
/// `env_var`, and say it: INFO when the key is set, WARN when the service is
/// serving unauthenticated on loopback.
///
/// This is [`resolve_startup_auth`] composed with [`api_key_configured`], the
/// form every key-gated service in the fleet wrote out by hand. The key it
/// checks is the one [`require_auth`] and
/// [`ApiKeyAuthHook`](crate::mcp::auth::ApiKeyAuthHook) enforce, so posture
/// and enforcement read the same variable by the same rule.
///
/// A service gated by something other than one key (Google ID tokens, a
/// session) calls [`resolve_startup_auth`] with its own `gated` instead.
///
/// # Errors
///
/// [`InsecureBindError`] naming `env_var` when the key is unset or empty and
/// `host` is not loopback: nothing would authenticate requests, so the server
/// must not start.
pub fn startup_auth(env_var: &str, host: &str) -> Result<AuthMode, InsecureBindError> {
    let mode = resolve_startup_auth(host, api_key_configured(env_var)).map_err(|refused| {
        InsecureBindError {
            gate: Some(env_var.to_owned()),
            ..refused
        }
    })?;
    match mode {
        AuthMode::Enforced => info!(
            gate = env_var,
            "{env_var} set — every request must carry it as a bearer token"
        ),
        AuthMode::LoopbackDev => warn!(
            gate = env_var,
            host, "{env_var} unset — serving UNAUTHENTICATED on loopback for local development"
        ),
    }
    Ok(mode)
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

    /// RFC 7235 §2.1: the auth-scheme is case-insensitive. A client sending
    /// `bearer` in lower case holds the right key and used to get a 401.
    #[tokio::test]
    async fn lowercase_bearer_scheme_passes() {
        const ENV: &str = "TRONC_AUTH_TEST_LOWERCASE_SCHEME";
        env::set_var(ENV, "secret-key-123");
        let app = make_app(ENV);
        let req = HttpRequest::builder()
            .uri("/test")
            .header("authorization", "bearer secret-key-123")
            .body(Body::empty())
            .expect("request"); // Safe: test assertion

        let resp = app.oneshot(req).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.status(), 200);
        env::remove_var(ENV);
    }

    #[test]
    fn bearer_credential_reads_the_scheme_in_any_case() {
        for header in [
            "Bearer tok-1",
            "bearer tok-1",
            "BEARER tok-1",
            "BeArEr tok-1",
            "Bearer   tok-1",
        ] {
            assert_eq!(
                bearer_credential(header),
                Some("tok-1"),
                "{header:?} carries the credential tok-1"
            );
        }
    }

    #[test]
    fn bearer_credential_refuses_other_schemes_and_empty_credentials() {
        for header in [
            "Basic dXNlcjpwYXNz",
            "Bearertok-1",
            "Bearer",
            "Bearer ",
            "Bearer    ",
            "tok-1",
            "",
        ] {
            assert_eq!(
                bearer_credential(header),
                None,
                "{header:?} must not read as a bearer credential"
            );
        }
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

    // ---- startup_auth: the key-gated posture, logged ----

    /// Everything a subscriber wrote while `f` ran, as text.
    fn captured_logs(f: impl FnOnce()) -> String {
        use std::io;
        use std::sync::{Arc, Mutex, PoisonError};
        use tracing::subscriber::with_default;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        impl io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        with_default(subscriber, f);
        let bytes = buffer
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        String::from_utf8(bytes).expect("utf-8 log") // Safe: test assertion
    }

    #[test]
    fn startup_auth_refuses_a_reachable_bind_and_names_the_key_to_set() {
        const ENV: &str = "TRONC_STARTUP_AUTH_TEST_REFUSED";
        env::remove_var(ENV);

        let err =
            startup_auth(ENV, "0.0.0.0").expect_err("no key on a reachable bind must be refused"); // Safe: test assertion

        assert_eq!(err.host, "0.0.0.0");
        assert_eq!(err.gate.as_deref(), Some(ENV));
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("Set {ENV}")),
            "the refusal must name the variable that arms the gate; message was {msg:?}"
        );
    }

    #[test]
    fn startup_auth_enforces_when_the_key_is_set_and_says_so() {
        const ENV: &str = "TRONC_STARTUP_AUTH_TEST_ENFORCED";
        env::set_var(ENV, "k-123");

        let mut mode = None;
        let logs = captured_logs(|| mode = Some(startup_auth(ENV, "0.0.0.0")));

        assert_eq!(mode, Some(Ok(AuthMode::Enforced)));
        assert!(logs.contains(" INFO "), "logs were {logs:?}");
        assert!(
            logs.contains(&format!("{ENV} set")),
            "the INFO line must name the key; logs were {logs:?}"
        );
        assert!(
            !logs.contains("k-123"),
            "the key's value must never be logged"
        );
        env::remove_var(ENV);
    }

    #[test]
    fn startup_auth_warns_when_loopback_serves_unauthenticated() {
        const ENV: &str = "TRONC_STARTUP_AUTH_TEST_LOOPBACK";
        env::set_var(ENV, "");

        let mut mode = None;
        let logs = captured_logs(|| mode = Some(startup_auth(ENV, "localhost")));

        assert_eq!(mode, Some(Ok(AuthMode::LoopbackDev)));
        assert!(logs.contains(" WARN "), "logs were {logs:?}");
        assert!(
            logs.contains(&format!("{ENV} unset")) && logs.contains("UNAUTHENTICATED"),
            "the WARN line must say which key is missing; logs were {logs:?}"
        );
        env::remove_var(ENV);
    }

    #[test]
    fn a_refusal_with_no_single_gate_asks_for_the_gate_generically() {
        let msg = resolve_startup_auth("0.0.0.0", false)
            .expect_err("refused") // Safe: test assertion
            .to_string();
        assert!(
            msg.contains("Arm this service's gate"),
            "message was {msg:?}"
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
