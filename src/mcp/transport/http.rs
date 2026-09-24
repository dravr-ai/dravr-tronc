// ABOUTME: HTTP transport implementing MCP Streamable HTTP with JSON and SSE responses
// ABOUTME: Serves a POST /mcp endpoint that accepts JSON-RPC and responds via JSON or event stream
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::convert::Infallible;
use std::error::Error;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::from_fn;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::stream;
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use crate::error::{UNAUTHORIZED, UNSUPPORTED_PROTOCOL_VERSION};
use crate::mcp::auth::AuthError;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;
use crate::server::auth::{bearer_credential, is_loopback_host};
use crate::server::request_guard::guard_requests;

/// The `MCP-Protocol-Version` HTTP header (revision 2026-07-28). The transport
/// forwards its value into the request metadata for the dispatch layer.
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Build an Axum router with the `/mcp` POST endpoint
///
/// Returns a `Router` that can be merged into a larger application router
/// or served standalone. The router is parameterized over the MCP server's
/// state type.
///
/// It carries no request guard of its own: the application that merges it
/// layers [`guard_requests`] once over the whole router, and [`serve`] does so
/// for the standalone case. Two guards on one route would log it twice. Never
/// layer a request deadline over it — a tool call is dispatched whole before
/// the response is written.
pub fn mcp_router<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp_post::<S>))
        .with_state(server)
}

/// Start a standalone HTTP server serving only the `/mcp` endpoint
///
/// Binds to the given host and port, serves until shutdown. Every request goes
/// through [`guard_requests`]: it gets a request id, a completion log line, and
/// a JSON `500` if a tool handler panics, instead of a dropped connection.
pub async fn serve<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let app = mcp_router(server).layer(from_fn(guard_requests));

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind {addr}: {e}"))?;

    info!(
        address = %addr,
        protocol_version = PROTOCOL_VERSION,
        "HTTP MCP transport listening"
    );

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("HTTP server error: {e}"))?;

    Ok(())
}

/// Handle an incoming MCP POST request
///
/// Enforces the `Origin` allowlist (403), refuses a body that is not a
/// JSON-RPC Request (400), authenticates via the server's hook (401 +
/// `WWW-Authenticate` on rejection, per RFC 9728), then dispatches under the
/// resolved per-call context. A request's response is rendered as JSON or SSE;
/// an accepted notification is answered 202 Accepted with no body.
pub async fn handle_mcp_post<S: Send + Sync + ?Sized + 'static>(
    State(server): State<Arc<McpServer<S>>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // 1. Origin allowlist (DNS-rebinding protection). A present Origin that is
    // not readable text is judged as the empty origin, which no list admits
    // but `"*"`: present-and-invalid is a 403, never read as absent.
    let origin = headers
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or_default());
    if !is_origin_allowed(origin, server.allowed_origins()) {
        debug!(?origin, "Rejected MCP request: origin not allowed");
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }

    // 2. Parse the JSON-RPC envelope. A body that is not a Request — not JSON,
    // a batch, a client's response, an id that is neither a string nor an
    // integer — is one this server cannot accept, which Streamable HTTP
    // answers with an HTTP error status, never a 2xx.
    let mut request = match JsonRpcRequest::parse(&body) {
        Ok(request) => request,
        Err(refusal) => return (StatusCode::BAD_REQUEST, Json(*refusal)).into_response(),
    };

    // 3. Populate transport-derived fields for the auth hook. The credential
    // comes from the `Authorization` header only; `parse` never reads one out
    // of the body.
    request.auth_token = bearer_token(&headers);
    // `MCP-Protocol-Version` is the client's standing assertion of what was
    // negotiated at `initialize`. On a stateless server there is no session to
    // check it against, so this is the only place it can be judged — and until
    // it was, the header was read into metadata that nothing read back, which
    // reads as wired and is not. A revision we do not speak is refused here
    // with -32004 rather than being served as if we did.
    if let Some(version) = headers
        .get(MCP_PROTOCOL_VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        if !server.accepts_protocol_version(version) {
            return (
                StatusCode::BAD_REQUEST,
                Json(JsonRpcResponse::error_with_data(
                    None,
                    UNSUPPORTED_PROTOCOL_VERSION,
                    "Unsupported protocol version".to_owned(),
                    serde_json::json!({
                        "supported": server.advertised_protocol_versions(),
                        "requested": version,
                    }),
                )),
            )
                .into_response();
        }
        request = request.with_metadata(MCP_PROTOCOL_VERSION_HEADER, version);
    }

    // 4. Authenticate (RFC 9728 resource-server posture).
    let ctx = match server.authenticate(&request).await {
        Ok(ctx) => ctx,
        Err(AuthError::Unauthorized { www_authenticate }) => {
            // RFC 9728 401 + WWW-Authenticate, with a JSON-RPC error body so
            // clients can parse the rejection (not a bare text body).
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, www_authenticate)],
                Json(JsonRpcResponse::error(
                    None,
                    UNAUTHORIZED,
                    "Unauthorized".to_owned(),
                )),
            )
                .into_response();
        }
        Err(AuthError::Forbidden { reason }) => {
            return (
                StatusCode::FORBIDDEN,
                Json(JsonRpcResponse::error(None, UNAUTHORIZED, reason)),
            )
                .into_response();
        }
        Err(AuthError::InsufficientScope {
            www_authenticate,
            reason,
        }) => {
            // RFC 6750 §3.1: an insufficient-scope refusal is a 403 that still
            // carries the challenge, so the client learns which grant it needs
            // rather than only that it was refused.
            return (
                StatusCode::FORBIDDEN,
                [(header::WWW_AUTHENTICATE, www_authenticate)],
                Json(JsonRpcResponse::error(None, UNAUTHORIZED, reason)),
            )
                .into_response();
        }
    };

    // 5. Dispatch under the resolved context.
    let Some(response) = server.handle_request_with_context(request, &ctx).await else {
        // An accepted notification: Streamable HTTP requires 202 Accepted with
        // no body (basic/transports §Sending Messages to the Server).
        return StatusCode::ACCEPTED.into_response();
    };

    debug!(method = "mcp", "Handled HTTP MCP request");

    // 6. Render as JSON or a single SSE event.
    let wants_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/event-stream"));

    if wants_sse {
        respond_sse(&response)
    } else {
        Json(response).into_response()
    }
}

/// Whether a request carrying `origin` passes the `allowed` list — the gate
/// MCP requires on every Streamable HTTP request to stop DNS rebinding.
///
/// - no `Origin` header (a non-browser client): accepted
/// - an empty list: only a loopback origin is accepted (see
///   [`McpServer::with_allowed_origins`]), so a page on another site that
///   rebinds its name to 127.0.0.1 cannot drive a local server
/// - a list containing `"*"`: every origin is accepted, by explicit opt-in
/// - otherwise the origin must be listed exactly
///
/// Public so a host serving another route over the same catalog can apply the
/// same gate rather than a copy of it.
#[must_use]
pub fn is_origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    origin.is_none_or(|origin| {
        if allowed.is_empty() {
            is_loopback_origin(origin)
        } else {
            allowed.iter().any(|a| a == "*" || a == origin)
        }
    })
}

/// Whether `origin` is a serialized `http` or `https` origin (RFC 6454
/// `scheme://host[:port]`) whose host is a loopback interface.
///
/// Anything that is not exactly that shape is refused rather than guessed at:
/// the opaque origin `null`, other schemes, and any authority carrying a path,
/// userinfo, query, fragment or whitespace. The host test is
/// [`is_loopback_host`], the same one the startup posture check uses.
fn is_loopback_origin(origin: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return false;
    }
    if authority.contains(|c: char| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#')) {
        return false;
    }

    // Split off an optional `:port`, minding the colons inside a bracketed
    // IPv6 literal, after whose `]` only nothing or `:port` may follow.
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let Some((literal, rest)) = bracketed.split_once(']') else {
                return false;
            };
            if rest.is_empty() {
                (literal, None)
            } else if let Some(port) = rest.strip_prefix(':') {
                (literal, Some(port))
            } else {
                return false;
            }
        }
        None => authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port))),
    };
    port.is_none_or(is_port) && is_loopback_host(host)
}

/// Whether `port` is a decimal TCP port.
fn is_port(port: &str) -> bool {
    port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
}

/// The bearer credential of the request's `Authorization` header, read with
/// the crate's one scheme parser ([`bearer_credential`]).
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_credential)
        .map(str::to_owned)
}

/// Wrap a JSON-RPC response in a single SSE event
fn respond_sse(response: &JsonRpcResponse) -> Response {
    let data = serde_json::to_string(&response).unwrap_or_else(|e| {
        error!(error = %e, "SSE serialization failed");
        format!(
            r#"{{"jsonrpc":"2.0","error":{{"code":-32603,"message":"Serialization failed: {e}"}}}}"#
        )
    });

    let event = Event::default().data(data);
    let event_stream = stream::once(async { Ok::<_, Infallible>(event) });

    Sse::new(event_stream).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{INVALID_REQUEST, PARSE_ERROR};
    use crate::mcp::auth::AuthHook;
    use crate::mcp::schema::{Tool, ToolResponse};
    use crate::mcp::tool::{McpTool, ToolCapabilities, ToolContext, ToolRegistry};
    use http::Request;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    struct TestState;

    struct HelloTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for HelloTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "hello".to_owned(),
                description: "Says hello".to_owned(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(
            &self,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("hello world".to_owned())
        }
    }

    fn make_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let state = Arc::new(TestState);
        let server = Arc::new(McpServer::new("test", "0.1.0", registry, state));
        mcp_router(server)
    }

    struct AdminTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for AdminTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "admin_op".to_owned(),
                description: "Admin-only".to_owned(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        fn capabilities(&self) -> ToolCapabilities {
            ToolCapabilities::ADMIN_ONLY
        }

        async fn execute(
            &self,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("admin ok".to_owned())
        }
    }

    /// Accepts `Bearer admin` (admin context) and `Bearer user` (non-admin);
    /// rejects anything else with 401 + a `WWW-Authenticate` challenge.
    struct TestAuthHook;

    #[async_trait::async_trait]
    impl AuthHook<TestState> for TestAuthHook {
        async fn authenticate(
            &self,
            request: &JsonRpcRequest,
            _state: &Arc<TestState>,
        ) -> Result<ToolContext, AuthError> {
            match request.auth_token.as_deref() {
                Some("admin") => Ok(ToolContext::new().with_user("u1").as_admin(true)),
                Some("user") => Ok(ToolContext::new().with_user("u2")),
                _ => Err(AuthError::Unauthorized {
                    www_authenticate: "Bearer resource_metadata=\"https://example.test/.well-known/oauth-protected-resource\"".to_owned(),
                }),
            }
        }
    }

    fn make_authed_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        registry.register(Box::new(AdminTool));
        let state = Arc::new(TestState);
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, state)
                .with_auth_hook(Arc::new(TestAuthHook))
                .with_allowed_origins(vec!["https://app.example.test".to_owned()]),
        );
        mcp_router(server)
    }

    #[tokio::test]
    async fn mcp_post_ping_returns_json() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 200);

        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["jsonrpc"], "2.0");
        assert!(json.get("result").is_some());
    }

    #[tokio::test]
    async fn mcp_post_tools_call() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "hello world");
    }

    #[tokio::test]
    async fn mcp_post_invalid_json_returns_parse_error() {
        let app = make_app();
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body("not json".to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["error"]["code"], PARSE_ERROR);
    }

    /// Streamable HTTP §Sending Messages to the Server: an accepted
    /// notification "MUST return HTTP status code 202 Accepted with no body".
    /// Older Python SDK clients fail on the 204 this used to return.
    #[tokio::test]
    async fn mcp_post_notification_returns_202_with_no_body() {
        let (status, body) = post(
            make_app(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty(), "a 202 carries no body; got {body:?}");
    }

    /// POST `body` to `/mcp` with the extra `headers`, returning the status and
    /// the raw response body.
    async fn post(app: Router, body: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(body.to_owned()).expect("request"); // Safe: test assertion
        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A body that is JSON but not a Request is one the server cannot accept:
    /// a 4xx carrying -32600, not a 200 carrying -32700.
    #[tokio::test]
    async fn json_that_is_not_a_request_is_refused_with_400_invalid_request() {
        for body in [
            // A client's response: this transport never sends the client a
            // request, so there is nothing for it to answer.
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            // A batch: MCP carries none.
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            // A null id: not a notification, and not answerable either.
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            // An id that is neither a string nor an integer.
            r#"{"jsonrpc":"2.0","id":{"n":1},"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        ] {
            let (status, response) = post(make_app(), body, &[]).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let json: Value = serde_json::from_str(&response).expect("json"); // Safe: test assertion
            assert_eq!(json["error"]["code"], INVALID_REQUEST, "{body}");
            assert_eq!(json["id"], Value::Null, "{body}");
        }
    }

    /// DNS-rebinding protection is on by default: with no allowlist a browser
    /// origin that is not loopback is refused with 403.
    #[tokio::test]
    async fn empty_allowlist_refuses_a_non_loopback_origin() {
        for origin in [
            "https://evil.test",
            "http://192.168.1.10:3000",
            "http://localhost.evil.test",
            "http://127.0.0.1.nip.io",
            "http://evil.test@localhost",
            "http://localhost/path",
            "http://localhost:99999",
            "http://::1",
            "ftp://localhost",
            "null",
        ] {
            let (status, _) = post(
                make_app(),
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                &[("origin", origin)],
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "origin {origin}");
        }
    }

    /// An `Origin` header that is present but not readable text is invalid,
    /// not absent: reading it as absent would admit it as a non-browser client.
    #[tokio::test]
    async fn an_unreadable_origin_is_refused_not_read_as_absent() {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(
                "origin",
                header::HeaderValue::from_bytes(b"http://localhost\xff")
                    .expect("obs-text is a legal header byte"), // Safe: test fixture
            )
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_owned())
            .expect("request"); // Safe: test fixture
        let response = make_app().oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Local development keeps working without configuration: a page served
    /// from a loopback origin, on any port, reaches an unconfigured server.
    #[tokio::test]
    async fn empty_allowlist_admits_a_loopback_origin() {
        for origin in [
            "http://localhost:3000",
            "https://localhost",
            "http://LOCALHOST:8082",
            "http://127.0.0.1:8081",
            "http://127.1.2.3",
            "http://[::1]:5173",
            "http://[::1]",
        ] {
            let (status, _) = post(
                make_app(),
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                &[("origin", origin)],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "origin {origin}");
        }
    }

    /// `"*"` is the explicit opt-out: a host that lists it accepts any origin.
    #[tokio::test]
    async fn wildcard_allowlist_admits_any_origin() {
        let server = Arc::new(
            McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
                .with_allowed_origins(vec!["*".to_owned()]),
        );
        let (status, _) = post(
            mcp_router(server),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            &[("origin", "https://evil.test")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// A configured allowlist is exact: it does not also admit loopback, and a
    /// request with no `Origin` (a non-browser client) is still accepted.
    #[test]
    fn a_configured_allowlist_is_matched_exactly() {
        let allowed = vec!["https://app.example.test".to_owned()];
        assert!(is_origin_allowed(
            Some("https://app.example.test"),
            &allowed
        ));
        assert!(!is_origin_allowed(Some("http://localhost:3000"), &allowed));
        assert!(!is_origin_allowed(Some("https://evil.test"), &allowed));
        assert!(is_origin_allowed(None, &allowed));
        assert!(is_origin_allowed(None, &[]));
    }

    /// The token comes from the `Authorization` header only. A body field
    /// named `auth` used to survive whenever no header was sent, so any caller
    /// could write its own credential into the JSON and be served as admin.
    #[tokio::test]
    async fn a_token_in_the_body_does_not_authenticate() {
        let (status, _) = post(
            make_authed_app(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"admin_op"},"auth":"admin"}"#,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// RFC 7235 §2.1: the auth-scheme is case-insensitive.
    #[tokio::test]
    async fn the_bearer_scheme_is_matched_case_insensitively() {
        for authorization in ["bearer user", "BEARER user"] {
            let (status, body) = post(
                make_authed_app(),
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#,
                &[("authorization", authorization)],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{authorization}");
            let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
            assert_eq!(
                json["result"]["content"][0]["text"], "hello world",
                "{authorization}"
            );
        }
    }

    #[tokio::test]
    async fn mcp_post_sse_accept_returns_event_stream() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let content_type = response
            .headers()
            .get("content-type")
            .expect("content-type") // Safe: test assertion
            .to_str()
            .expect("str"); // Safe: test assertion
        assert!(content_type.contains("text/event-stream"));
    }

    #[tokio::test]
    async fn mcp_post_tools_list() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        let tools = json["result"]["tools"].as_array().expect("tools"); // Safe: test assertion
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "hello");
    }

    #[tokio::test]
    async fn mcp_post_initialize() {
        let app = make_app();
        let body = r#"{
            "jsonrpc":"2.0",
            "id":5,
            "method":"initialize",
            "params":{
                "protocolVersion":"2024-11-05",
                "capabilities":{},
                "clientInfo":{"name":"test"}
            }
        }"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["serverInfo"]["name"], "test");
    }

    #[tokio::test]
    async fn mcp_post_disallowed_origin_returns_403() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("origin", "https://evil.test")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 403);
    }

    #[tokio::test]
    async fn mcp_post_missing_token_returns_401_with_challenge() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 401);
        let challenge = response
            .headers()
            .get("www-authenticate")
            .expect("www-authenticate header") // Safe: test assertion
            .to_str()
            .expect("str"); // Safe: test assertion
        assert!(challenge.contains("Bearer"));
        assert!(challenge.contains("resource_metadata"));
    }

    #[tokio::test]
    async fn mcp_post_authenticated_user_runs_allowed_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("origin", "https://app.example.test")
            .header("authorization", "Bearer user")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "hello world");
    }

    #[tokio::test]
    async fn mcp_post_admin_token_runs_admin_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"admin_op"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer admin")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "admin ok");
        assert_eq!(json["result"]["isError"], false);
    }

    #[tokio::test]
    async fn mcp_post_non_admin_blocked_from_admin_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"admin_op"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer user")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
                                                                      // The request is authenticated (200), but the registry's ADMIN_ONLY gate
                                                                      // turns it into a tool-level error result.
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["isError"], true);
        assert!(json["result"]["content"][0]["text"]
            .as_str()
            .expect("text") // Safe: test assertion
            .contains("admin"));
    }

    /// A scope refusal is a 403 that still carries the challenge.
    ///
    /// RFC 6750 §3.1: the point of `insufficient_scope` is that the client
    /// learns which grant it needs. A bare 403 tells it only that it lost, so
    /// the header is the whole value of the variant — asserted here because a
    /// renderer that dropped it would still return the "right" status.
    #[tokio::test]
    async fn insufficient_scope_returns_403_with_the_challenge() {
        struct ScopeHook;

        #[async_trait::async_trait]
        impl AuthHook<TestState> for ScopeHook {
            async fn authenticate(
                &self,
                _request: &JsonRpcRequest,
                _state: &Arc<TestState>,
            ) -> Result<ToolContext, AuthError> {
                Err(AuthError::InsufficientScope {
                    www_authenticate:
                        "Bearer error=\"insufficient_scope\", scope=\"fitness:write\"".to_owned(),
                    reason: "the grant does not cover this tool".to_owned(),
                })
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, Arc::new(TestState))
                .with_auth_hook(Arc::new(ScopeHook)),
        );
        let app = mcp_router(server);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .body(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                               "params":{"name":"hello","arguments":{}}})
                        .to_string(),
                    )
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .expect("an insufficient-scope refusal must carry the challenge") // Safe: test assertion
            .to_str()
            .expect("ascii"); // Safe: test assertion
        assert!(
            challenge.contains("insufficient_scope"),
            "challenge names the error: {challenge}"
        );
        assert!(
            challenge.contains("fitness:write"),
            "challenge names the scope the client must ask for: {challenge}"
        );
    }

    /// An `MCP-Protocol-Version` the server does not speak is refused.
    ///
    /// The header used to be read into request metadata that nothing read
    /// back, which reads as wired and is not. On a stateless server this is the
    /// only place a per-request revision assertion can be judged.
    #[tokio::test]
    async fn an_unsupported_protocol_version_header_is_refused() {
        let response = make_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header(MCP_PROTOCOL_VERSION_HEADER, "1999-01-01")
                    .body(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string())
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(
            &response
                .into_body()
                .collect()
                .await
                .expect("body") // Safe: test fixture
                .to_bytes(),
        )
        .expect("json"); // Safe: test fixture
        assert_eq!(body["error"]["code"], -32_022);
        assert_eq!(body["error"]["data"]["requested"], "1999-01-01");
        assert!(
            body["error"]["data"]["supported"].is_array(),
            "the refusal tells the client what we DO speak: {body}"
        );
    }

    /// A supported revision on the header is served normally — the gate must
    /// refuse the wrong one without refusing the right one.
    #[tokio::test]
    async fn a_supported_protocol_version_header_is_served() {
        let supported = McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
            .advertised_protocol_versions()
            .first()
            .cloned()
            .expect("a server advertises at least one revision"); // Safe: test assertion

        let response = make_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header(MCP_PROTOCOL_VERSION_HEADER, supported.as_str())
                    .body(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string())
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::OK);
    }
}
