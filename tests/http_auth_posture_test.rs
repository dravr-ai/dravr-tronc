// ABOUTME: Tests that http::serve refuses a reachable bind with no auth hook, and ApiKeyAuthHook
// ABOUTME: Real sockets for serve's posture; the router for the hook's 401 challenge and admission
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::env;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::auth::{ApiKeyAuthHook, AuthHook, API_KEY_AUTH_METHOD};
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::mcp::transport::http::{mcp_router, serve};
use dravr_tronc::server::auth::InsecureBindError;
use http::Request;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tower::ServiceExt;

struct State;

/// Answers with how the caller was authenticated, so a test can read what the
/// hook resolved.
struct WhoAmI;

#[async_trait]
impl McpTool<State> for WhoAmI {
    fn definition(&self) -> Tool {
        Tool {
            name: "whoami".to_owned(),
            description: "Reports the resolved caller".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<State>, ctx: &ToolContext, _args: Value) -> ToolResponse {
        ToolResponse::text(format!(
            "method={} admin={} user={}",
            ctx.auth_method.as_deref().unwrap_or("none"),
            ctx.is_admin,
            ctx.user_id.as_deref().unwrap_or("none"),
        ))
    }
}

fn server() -> McpServer<State> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WhoAmI));
    McpServer::new("posture-test", "0.1.0", registry, Arc::new(State))
}

fn gated_server(env_var: &str) -> McpServer<State> {
    server().with_auth_hook(Arc::new(ApiKeyAuthHook::new(env_var, "posture-test")))
}

/// A port nothing is listening on right now, on `host`.
fn free_port(host: &str) -> u16 {
    StdTcpListener::bind((host, 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

const PING: &str = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
const WHOAMI: &str =
    r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#;

/// POST `body` to `/mcp` on 127.0.0.1:`port` over a real socket, retrying the
/// connect while the server comes up, and return the raw HTTP response.
async fn post_mcp(port: u16, bearer: Option<&str>, body: &str) -> String {
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            stream = Some(s);
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let mut stream = stream.expect("the server never accepted a connection");
    let auth = bearer.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    response
}

/// `serve` must answer a refused bind at once; a hang here would mean it bound
/// and is serving.
async fn refused(server: McpServer<State>, host: &str) -> InsecureBindError {
    let outcome = timeout(Duration::from_secs(5), serve(Arc::new(server), host, 0))
        .await
        .unwrap_or_else(|_| panic!("serve({host:?}) with no hook started serving"));
    let err = outcome.expect_err("a reachable bind with no hook must be refused");
    *err.downcast::<InsecureBindError>()
        .unwrap_or_else(|other| panic!("expected InsecureBindError, got {other}"))
}

// ---- http::serve: no hook, no reachable bind ----

#[tokio::test]
async fn serve_refuses_the_ipv4_wildcard_without_an_auth_hook() {
    let err = refused(server(), "0.0.0.0").await;
    assert_eq!(err.host, "0.0.0.0");
    assert_eq!(err.gate, None);
    assert!(err.to_string().contains("refusing to start"));
}

#[tokio::test]
async fn serve_refuses_the_ipv6_wildcard_without_an_auth_hook() {
    let err = refused(server(), "[::]").await;
    assert_eq!(err.host, "[::]");
}

#[tokio::test]
async fn serve_serves_loopback_without_an_auth_hook() {
    let port = free_port("127.0.0.1");
    let task = tokio::spawn(serve(Arc::new(server()), "127.0.0.1", port));

    let response = post_mcp(port, None, WHOAMI).await;
    task.abort();

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "response was {response:?}"
    );
    assert!(
        response.contains("method=none admin=false user=none"),
        "an unauthenticated loopback call runs as the anonymous context; response was {response:?}"
    );
}

#[tokio::test]
async fn serve_accepts_localhost_because_it_resolves_to_loopback() {
    let port = free_port("127.0.0.1");
    // Still running after the window means it bound and is serving; a refusal
    // returns at once.
    let outcome = timeout(
        Duration::from_millis(500),
        serve(Arc::new(server()), "localhost", port),
    )
    .await;
    assert!(
        outcome.is_err(),
        "serve(\"localhost\") must serve, not return; it returned {outcome:?}"
    );
}

#[tokio::test]
async fn serve_serves_a_reachable_bind_behind_the_api_key_hook() {
    const ENV: &str = "TRONC_POSTURE_TEST_SERVE_KEY";
    env::set_var(ENV, "wire-key-42");
    let port = free_port("0.0.0.0");
    let task = tokio::spawn(serve(Arc::new(gated_server(ENV)), "0.0.0.0", port));

    let without = post_mcp(port, None, PING).await;
    let wrong = post_mcp(port, Some("wire-key-43"), PING).await;
    let with = post_mcp(port, Some("wire-key-42"), WHOAMI).await;
    task.abort();
    env::remove_var(ENV);

    assert!(
        without.starts_with("HTTP/1.1 401"),
        "response was {without:?}"
    );
    assert!(
        without
            .to_ascii_lowercase()
            .contains("www-authenticate: bearer realm=\"posture-test\""),
        "a 401 must carry the bearer challenge; response was {without:?}"
    );
    assert!(wrong.starts_with("HTTP/1.1 401"), "response was {wrong:?}");
    assert!(with.starts_with("HTTP/1.1 200"), "response was {with:?}");
    assert!(
        with.contains(&format!(
            "method={API_KEY_AUTH_METHOD} admin=false user=none"
        )),
        "response was {with:?}"
    );
}

// ---- ApiKeyAuthHook through the router ----

async fn call(env_var: &str, bearer: Option<&str>) -> (u16, Option<String>, Value) {
    let app = mcp_router(Arc::new(gated_server(env_var)));
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let resp = app
        .oneshot(builder.body(WHOAMI.to_owned()).expect("request"))
        .await
        .expect("response");
    let status = resp.status().as_u16();
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .map(|v| v.to_str().expect("ascii").to_owned());
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        challenge,
        serde_json::from_slice(&bytes).expect("json"),
    )
}

#[tokio::test]
async fn the_hook_answers_401_with_a_bearer_challenge_without_the_key() {
    const ENV: &str = "TRONC_POSTURE_TEST_HOOK_MISSING";
    env::set_var(ENV, "correct-horse");

    let (status, challenge, body) = call(ENV, None).await;
    env::remove_var(ENV);

    assert_eq!(status, 401);
    assert_eq!(challenge.as_deref(), Some("Bearer realm=\"posture-test\""));
    assert_eq!(body["error"]["message"], "Unauthorized");
}

#[tokio::test]
async fn the_hook_refuses_a_wrong_key_of_the_same_length() {
    const ENV: &str = "TRONC_POSTURE_TEST_HOOK_WRONG";
    env::set_var(ENV, "correct-horse");

    let (status, challenge, _) = call(ENV, Some("correct-mouse")).await;
    env::remove_var(ENV);

    assert_eq!(status, 401);
    assert_eq!(challenge.as_deref(), Some("Bearer realm=\"posture-test\""));
}

#[tokio::test]
async fn the_hook_admits_the_key_as_a_key_holder_never_an_admin() {
    const ENV: &str = "TRONC_POSTURE_TEST_HOOK_RIGHT";
    env::set_var(ENV, "correct-horse");

    let (status, challenge, body) = call(ENV, Some("correct-horse")).await;
    env::remove_var(ENV);

    assert_eq!(status, 200);
    assert_eq!(challenge, None);
    assert_eq!(
        body["result"]["content"][0]["text"],
        "method=api_key admin=false user=none"
    );
}

#[tokio::test]
async fn the_hook_fails_closed_when_its_key_is_unset_or_empty() {
    const ENV: &str = "TRONC_POSTURE_TEST_HOOK_UNSET";
    env::remove_var(ENV);
    for bearer in [None, Some("anything")] {
        let (status, _, _) = call(ENV, bearer).await;
        assert_eq!(status, 401, "unset key, bearer {bearer:?}");
    }

    env::set_var(ENV, "");
    // An empty presented token against an empty key must not match.
    let hook = ApiKeyAuthHook::new(ENV, "posture-test");
    let mut request = JsonRpcRequest::new("tools/list", None);
    request.auth_token = Some(String::new());
    let refused = AuthHook::<State>::authenticate(&hook, &request, &Arc::new(State)).await;
    env::remove_var(ENV);

    assert!(refused.is_err(), "an empty key admits nobody");
}

#[test]
fn the_realm_is_escaped_into_a_quoted_string() {
    let hook = ApiKeyAuthHook::new("UNUSED", r#"a "quoted" \ realm"#);
    assert_eq!(hook.challenge(), r#"Bearer realm="a \"quoted\" \\ realm""#);
}
