// ABOUTME: Tests the request guard: a panic answers a JSON 500 over a real socket, a deadline a JSON 504
// ABOUTME: Pins the request-id echo, the completion and dropped-request log lines, and the MCP exemption
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

// Same allowances tests/integration_test.rs carries: an integration test is not
// covered by the lib's `cfg_attr(test, ...)`, and the panicking handlers here
// are the behaviour under test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn;
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::mcp::transport::http::{mcp_router, serve};
use dravr_tronc::server::request_guard::{
    enforce_deadline, guard_requests, RequestId, HANDLER_PANIC, MAX_REQUEST_ID_LEN,
    REQUEST_ID_HEADER, REQUEST_TIMEOUT,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time;
use tower::ServiceExt;
use tracing::subscriber::{set_default, DefaultGuard};
use tracing::Level;

// ============================================================================
// Fixtures
// ============================================================================

/// Log output written by the subscriber [`capture_logs`] installs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Route this thread's logs into a buffer for the life of the guard.
///
/// `#[tokio::test]` runs a current-thread runtime, so the server tasks these
/// tests spawn log on the same thread and land in the buffer too.
fn capture_logs() -> (Captured, DefaultGuard) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(Level::INFO)
        .finish();
    (captured, set_default(subscriber))
}

/// Sets its flag when dropped: proof that a handler's future was cancelled.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A handler that outlives every deadline in this file, flagging its cancellation.
async fn slow_handler(flag: Arc<AtomicBool>) -> &'static str {
    let armed = DropFlag(flag);
    time::sleep(Duration::from_secs(30)).await;
    drop(armed);
    "late"
}

/// A panic with a formatted message: its payload is a `String`.
async fn panicking_handler() -> &'static str {
    let row = 7;
    panic!("scrape blew up at row {row}")
}

/// A panic with a literal message: its payload is a `&str`.
async fn literal_panicking_handler() -> &'static str {
    panic!("literal panic payload")
}

/// Serve `app` on an ephemeral loopback port.
async fn serve_on_loopback(app: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
}

/// One HTTP/1.1 exchange over a raw socket, returning everything the server
/// wrote before closing. A server that drops the connection returns an empty
/// string here — which is exactly what the guard must never produce.
async fn raw_exchange(addr: SocketAddr, head: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "{head}\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

/// Split a raw response into its lower-cased head and its body.
fn split_response(raw: &str) -> (String, String) {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no complete HTTP response in {raw:?}"));
    (head.to_ascii_lowercase(), body.to_owned())
}

async fn body_json(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn echoed_id(response: &Response) -> String {
    response
        .headers()
        .get(REQUEST_ID_HEADER)
        .expect("the guard echoes a request id on every response")
        .to_str()
        .unwrap()
        .to_owned()
}

fn deadline_layered(router: Router, deadline: Duration) -> Router {
    router.layer(from_fn(move |req, next| {
        enforce_deadline(deadline, req, next)
    }))
}

// ============================================================================
// Panic containment
// ============================================================================

/// The scenario behind carnet#546: a handler failure must reach the client as
/// a classifiable answer. Asserted over a real socket, because a panic that
/// escaped the guard would reset the connection — `oneshot` cannot show that.
#[tokio::test]
async fn a_panicking_handler_answers_a_json_500_over_a_real_socket() {
    let (logs, _subscriber) = capture_logs();
    let app = Router::new()
        .route("/api/boom", get(panicking_handler))
        .layer(from_fn(guard_requests));
    let addr = serve_on_loopback(app).await;

    let raw = raw_exchange(
        addr,
        &format!("GET /api/boom HTTP/1.1\r\n{REQUEST_ID_HEADER}: caller-id-1"),
        "",
    )
    .await;

    assert!(
        raw.starts_with("HTTP/1.1 500"),
        "a panic answers 500, not a dropped connection: {raw:?}"
    );
    let (head, body) = split_response(&raw);
    assert!(
        head.contains("x-request-id: caller-id-1"),
        "the caller's id is echoed: {head}"
    );
    assert!(head.contains("content-type: application/json"), "{head}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"]["type"], HANDLER_PANIC);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("caller-id-1"),
        "the body names the id the failure was logged under: {json}"
    );

    let logs = logs.text();
    assert!(logs.contains("request handler panicked"), "{logs}");
    assert!(
        logs.contains("scrape blew up at row 7"),
        "the panic payload is logged: {logs}"
    );
    assert!(logs.contains("request_id=caller-id-1"), "{logs}");
    assert!(
        logs.contains("status=500"),
        "the completion line records the 500: {logs}"
    );
}

/// `panic!` with a literal carries a `&str` payload rather than a `String`;
/// both must reach the log.
#[tokio::test]
async fn a_literal_panic_payload_is_logged_too() {
    let (logs, _subscriber) = capture_logs();
    let app = Router::new()
        .route("/api/boom", get(literal_panicking_handler))
        .layer(from_fn(guard_requests));

    let response = app
        .oneshot(Request::get("/api/boom").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body_json(response).await["error"]["type"], HANDLER_PANIC);
    assert!(
        logs.text().contains("literal panic payload"),
        "{}",
        logs.text()
    );
}

// ============================================================================
// Deadline
// ============================================================================

/// A handler still running at the deadline is cancelled and the caller gets a
/// 504 whose body says so, carrying the request id like any other answer.
#[tokio::test]
async fn a_handler_past_its_deadline_answers_a_json_504_and_is_cancelled() {
    let (logs, _subscriber) = capture_logs();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let app = deadline_layered(
        Router::new().route("/api/slow", get(move || slow_handler(Arc::clone(&flag)))),
        Duration::from_millis(50),
    )
    .layer(from_fn(guard_requests));

    let response = app
        .oneshot(
            Request::get("/api/slow")
                .header(REQUEST_ID_HEADER, "deadline-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(echoed_id(&response), "deadline-1");
    let json = body_json(response).await;
    assert_eq!(json["error"]["type"], REQUEST_TIMEOUT);
    assert!(
        json["error"]["message"].as_str().unwrap().contains("50ms"),
        "the body names the deadline: {json}"
    );
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the handler's future is dropped at the deadline, not left running"
    );

    let logs = logs.text();
    assert!(
        logs.contains("request outlived its deadline"),
        "the deadline is logged: {logs}"
    );
    assert!(logs.contains("request_id=deadline-1"), "{logs}");
}

/// The deadline covers only the router it is layered on. Merged after it, the
/// MCP route runs a tool call past the deadline to completion — a request
/// deadline must never cut a tool call that is legitimately long — while the
/// REST route on the same app is still cut.
#[tokio::test]
async fn a_deadline_layered_on_rest_routes_leaves_a_merged_mcp_route_uncut() {
    struct SlowTool;

    #[async_trait]
    impl McpTool<()> for SlowTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "slow".to_owned(),
                description: "Answers after the REST deadline has passed".to_owned(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(
            &self,
            _state: &Arc<()>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            time::sleep(Duration::from_millis(200)).await;
            ToolResponse::text("finished after the deadline".to_owned())
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowTool));
    let server = Arc::new(McpServer::new("test", "0.1.0", registry, Arc::new(())));
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let app = deadline_layered(
        Router::new().route("/api/slow", get(move || slow_handler(Arc::clone(&flag)))),
        Duration::from_millis(50),
    )
    .merge(mcp_router(server))
    .layer(from_fn(guard_requests));

    let tool_call = app
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                           "params": {"name": "slow", "arguments": {}}})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tool_call.status(), StatusCode::OK);
    assert_eq!(
        body_json(tool_call).await["result"]["content"][0]["text"],
        "finished after the deadline"
    );

    let rest = app
        .oneshot(Request::get("/api/slow").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        rest.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "the same app still cuts the REST route at its deadline"
    );
}

// ============================================================================
// Request id and logs
// ============================================================================

/// The caller's id is adopted, echoed, handed to the handler, and logged with
/// the route template — never the concrete path or its query, which carry ids.
#[tokio::test]
async fn the_callers_request_id_is_echoed_and_logged_with_the_route_template() {
    let (logs, _subscriber) = capture_logs();
    let app = Router::new()
        .route(
            "/api/athletes/{id}/plan",
            get(|Extension(id): Extension<RequestId>| async move { id.to_string() }),
        )
        .layer(from_fn(guard_requests));

    let response = app
        .oneshot(
            Request::get("/api/athletes/athlete-42/plan?athlete=secret-99")
                .header(REQUEST_ID_HEADER, "platform-7f3a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(echoed_id(&response), "platform-7f3a");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        &body[..],
        b"platform-7f3a",
        "the handler reads the same id from the request's extensions"
    );

    let logs = logs.text();
    let completion = logs
        .lines()
        .find(|line| line.contains("request completed"))
        .unwrap_or_else(|| panic!("one completion line per request: {logs}"));
    assert!(completion.contains(" INFO "), "{completion}");
    assert!(
        completion.contains("request_id=platform-7f3a"),
        "{completion}"
    );
    assert!(completion.contains("method=GET"), "{completion}");
    assert!(
        completion.contains("path=/api/athletes/{id}/plan"),
        "{completion}"
    );
    assert!(completion.contains("status=200"), "{completion}");
    assert!(completion.contains("latency_ms="), "{completion}");
    assert!(
        !logs.contains("athlete-42") && !logs.contains("secret-99"),
        "neither the concrete path nor the query reaches the logs: {logs}"
    );
}

/// A request no route matched is still logged and still carries an id, under
/// a placeholder rather than the path it asked for.
#[tokio::test]
async fn an_unmatched_request_is_logged_without_its_path() {
    let (logs, _subscriber) = capture_logs();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(from_fn(guard_requests));

    let response = app
        .oneshot(
            Request::get("/sessions/secret-session-9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!echoed_id(&response).is_empty());
    let logs = logs.text();
    assert!(logs.contains("path=<unmatched>"), "{logs}");
    assert!(logs.contains("status=404"), "{logs}");
    assert!(!logs.contains("secret-session-9"), "{logs}");
}

/// An id the guard cannot safely copy into its logs is replaced with a minted
/// one, and minted ids never repeat.
#[tokio::test]
async fn an_unusable_request_id_is_replaced_by_a_minted_one() {
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(from_fn(guard_requests));

    let too_long = "a".repeat(MAX_REQUEST_ID_LEN + 1);
    let mut minted = Vec::new();
    for unusable in [
        "has spaces in it",
        "quote\"injection",
        too_long.as_str(),
        "",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/health")
                    .header(REQUEST_ID_HEADER, unusable)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = echoed_id(&response);
        assert_ne!(id, unusable, "an unusable id is never adopted");
        let (instance, sequence) = id
            .split_once('-')
            .unwrap_or_else(|| panic!("a minted id is <instance>-<sequence>: {id}"));
        assert_eq!(instance.len(), 16, "{id}");
        assert!(instance.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        assert!(
            !sequence.is_empty() && sequence.bytes().all(|b| b.is_ascii_hexdigit()),
            "{id}"
        );
        minted.push(id);
    }
    minted.sort();
    minted.dedup();
    assert_eq!(minted.len(), 4, "every minted id is distinct: {minted:?}");

    let at_the_limit = "b".repeat(MAX_REQUEST_ID_LEN);
    let response = app
        .oneshot(
            Request::get("/health")
                .header(REQUEST_ID_HEADER, at_the_limit.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        echoed_id(&response),
        at_the_limit,
        "the limit itself is usable"
    );
}

/// A request whose future is dropped before it answered — the client closed
/// the connection mid-request — is logged with how long it ran. That is the
/// line the next "connection closed before message completed" leaves behind.
#[tokio::test]
async fn a_request_dropped_before_its_response_is_logged() {
    let (logs, _subscriber) = capture_logs();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let app = Router::new()
        .route("/api/slow", get(move || slow_handler(Arc::clone(&flag))))
        .layer(from_fn(guard_requests));

    let abandoned = time::timeout(
        Duration::from_millis(50),
        app.oneshot(
            Request::get("/api/slow")
                .header(REQUEST_ID_HEADER, "abandoned-1")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;

    assert!(abandoned.is_err(), "the caller gave up before any response");
    assert!(cancelled.load(Ordering::SeqCst));
    let logs = logs.text();
    let dropped = logs
        .lines()
        .find(|line| line.contains("request dropped before a response"))
        .unwrap_or_else(|| panic!("the dropped request is logged: {logs}"));
    assert!(dropped.contains(" WARN "), "{dropped}");
    assert!(dropped.contains("request_id=abandoned-1"), "{dropped}");
    assert!(dropped.contains("path=/api/slow"), "{dropped}");
    assert!(dropped.contains("elapsed_ms="), "{dropped}");
    assert!(
        !logs.contains("request completed"),
        "a request that never answered is not logged as completed: {logs}"
    );
}

// ============================================================================
// Standalone MCP transport
// ============================================================================

/// `serve` — the standalone MCP transport every satellite can run — layers
/// the guard itself, so its requests carry an id without the caller wiring it.
#[tokio::test]
async fn the_standalone_mcp_transport_is_guarded() {
    let port = {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap().port()
    };
    let server = Arc::new(McpServer::new(
        "test",
        "0.1.0",
        ToolRegistry::<()>::new(),
        Arc::new(()),
    ));
    tokio::spawn(async move { serve(server, "127.0.0.1", port).await });
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let mut ready = false;
    for _ in 0..100 {
        if TcpStream::connect(addr).await.is_ok() {
            ready = true;
            break;
        }
        time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready, "the standalone transport never started listening");

    let raw = raw_exchange(
        addr,
        &format!(
            "POST /mcp HTTP/1.1\r\ncontent-type: application/json\r\n{REQUEST_ID_HEADER}: serve-check-1"
        ),
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    )
    .await;

    assert!(raw.starts_with("HTTP/1.1 200"), "{raw:?}");
    let (head, body) = split_response(&raw);
    assert!(head.contains("x-request-id: serve-check-1"), "{head}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(json.get("result").is_some(), "{json}");
}
