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
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::stream;
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use crate::error::{PARSE_ERROR, UNAUTHORIZED, UNSUPPORTED_PROTOCOL_VERSION};
use crate::mcp::auth::AuthError;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;

/// The `MCP-Protocol-Version` HTTP header (revision 2026-07-28). The transport
/// forwards its value into the request metadata for the dispatch layer.
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Build an Axum router with the `/mcp` POST endpoint
///
/// Returns a `Router` that can be merged into a larger application router
/// or served standalone. The router is parameterized over the MCP server's
/// state type.
pub fn mcp_router<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp_post::<S>))
        .with_state(server)
}

/// Start a standalone HTTP server serving only the `/mcp` endpoint
///
/// Binds to the given host and port, serves until shutdown.
pub async fn serve<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let app = mcp_router(server);

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
/// Enforces the `Origin` allowlist (403), authenticates via the server's hook
/// (401 + `WWW-Authenticate` on rejection, per RFC 9728), then dispatches under
/// the resolved per-call context and renders the response as JSON or SSE.
pub async fn handle_mcp_post<S: Send + Sync + ?Sized + 'static>(
    State(server): State<Arc<McpServer<S>>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // 1. Origin allowlist (DNS-rebinding protection).
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !is_origin_allowed(origin, server.allowed_origins()) {
        debug!(?origin, "Rejected MCP request: origin not allowed");
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }

    // 2. Parse the JSON-RPC envelope.
    let mut request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => {
            return Json(JsonRpcResponse::error(
                None,
                PARSE_ERROR,
                format!("Parse error: {e}"),
            ))
            .into_response();
        }
    };

    // 3. Populate transport-derived fields for the auth hook.
    if let Some(token) = bearer_token(&headers) {
        request.auth_token = Some(token);
    }
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
        // Notification — no response needed
        return StatusCode::NO_CONTENT.into_response();
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

/// Whether the given `Origin` is permitted. An absent origin (non-browser
/// client) is allowed; an empty allowlist or one containing `"*"` allows any
/// origin; otherwise the origin must be listed exactly.
fn is_origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    origin
        .is_none_or(|origin| allowed.is_empty() || allowed.iter().any(|a| a == "*" || a == origin))
}

/// Extract a bearer token from the `Authorization` header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
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
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["error"]["code"], PARSE_ERROR);
    }

    #[tokio::test]
    async fn mcp_post_notification_returns_204() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 204);
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
