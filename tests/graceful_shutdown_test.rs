// ABOUTME: Tests http::serve_with_shutdown draining a request in flight before it returns
// ABOUTME: and refusing new connections once the shutdown trigger has fired
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::mcp::transport::http::serve_with_shutdown;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

/// Answers after 300ms, long enough to still be running when shutdown fires.
struct SlowTool;

#[async_trait]
impl McpTool<()> for SlowTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "slow".to_owned(),
            description: "Answers late".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<()>, _ctx: &ToolContext, _args: Value) -> ToolResponse {
        sleep(Duration::from_millis(300)).await;
        ToolResponse::text("finished".to_owned())
    }
}

fn free_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

async fn connect(port: u16) -> TcpStream {
    for _ in 0..100 {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never accepted a connection");
}

/// A request in flight when shutdown fires is answered, and only then does
/// `serve_with_shutdown` return `Ok`; nothing new is accepted afterwards.
#[tokio::test]
async fn shutdown_drains_the_request_in_flight() {
    let port = free_port();
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowTool));
    let server = Arc::new(McpServer::new("drain", "0.1.0", registry, Arc::new(())));
    let (trigger, fired) = oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        serve_with_shutdown(server, "127.0.0.1", port, async {
            fired.await.ok();
        })
        .await
        .map_err(|e| e.to_string())
    });

    let mut stream = connect(port).await;
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"slow"}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream;q=0.5\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    // Let the call reach the tool before shutdown fires.
    sleep(Duration::from_millis(100)).await;
    trigger.send(()).expect("the server is still waiting");

    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
    assert!(response.contains("finished"), "{response:?}");

    let outcome = timeout(Duration::from_secs(5), serving)
        .await
        .expect("serve_with_shutdown returns once drained")
        .expect("the serving task did not panic");
    assert_eq!(outcome, Ok(()));
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "a stopped server accepts no connection"
    );
}
