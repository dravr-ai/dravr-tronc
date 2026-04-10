// ABOUTME: HTTP transport implementing MCP Streamable HTTP with JSON and SSE responses
// ABOUTME: Serves a POST /mcp endpoint that accepts JSON-RPC and responds via JSON or event stream
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::convert::Infallible;
use std::error::Error;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::stream;
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use crate::mcp::protocol::{JsonRpcResponse, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;

/// Build an Axum router with the `/mcp` POST endpoint
///
/// Returns a `Router` that can be merged into a larger application router
/// or served standalone. The router is parameterized over the MCP server's
/// state type.
pub fn mcp_router<S: Send + Sync + 'static>(server: Arc<McpServer<S>>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp_post::<S>))
        .with_state(server)
}

/// Start a standalone HTTP server serving only the `/mcp` endpoint
///
/// Binds to the given host and port, serves until shutdown.
pub async fn serve<S: Send + Sync + 'static>(
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
/// Parses the body as JSON-RPC, dispatches to the MCP server, and returns
/// the response as JSON or SSE depending on the Accept header.
pub async fn handle_mcp_post<S: Send + Sync + 'static>(
    State(server): State<Arc<McpServer<S>>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(response) = server.handle_raw(&body).await else {
        // Notification — no response needed
        return StatusCode::NO_CONTENT.into_response();
    };

    debug!(method = "mcp", "Handled HTTP MCP request");

    let wants_sse = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/event-stream"));

    if wants_sse {
        respond_sse(&response)
    } else {
        Json(response).into_response()
    }
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
    use crate::error::PARSE_ERROR;
    use crate::mcp::protocol::{CallToolResult, ToolDefinition};
    use crate::mcp::tool::{McpTool, ToolRegistry};
    use http::Request;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    struct TestState;

    struct HelloTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for HelloTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "hello".to_owned(),
                description: "Says hello".to_owned(),
                input_schema: json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _state: &Arc<RwLock<TestState>>,
            _arguments: Value,
        ) -> CallToolResult {
            CallToolResult::text("hello world".to_owned())
        }
    }

    fn make_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let state = Arc::new(RwLock::new(TestState));
        let server = Arc::new(McpServer::new("test", "0.1.0", registry, state));
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
}
