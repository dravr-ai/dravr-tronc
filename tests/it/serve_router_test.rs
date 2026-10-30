// ABOUTME: Tests http::serve_router / serve_router_with_shutdown serving a host's own router over a socket
// ABOUTME: Request guard applied, loopback Host enforced, typed bind errors, graceful stop on the trigger
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::io;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use dravr_tronc::mcp::transport::http::serve_router_with_shutdown;
use dravr_tronc::mcp::transport::TransportError;
use dravr_tronc::server::request_guard::{HANDLER_PANIC, REQUEST_ID_HEADER};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::{self, JoinHandle};
use tokio::time::{sleep, timeout};

/// A host's own router: a health route beside one that panics.
fn app() -> Router {
    async fn panics() -> &'static str {
        panic!("the handler blew up");
    }
    Router::new()
        .route("/health", get(|| async { "healthy" }))
        .route("/boom", get(panics))
}

fn free_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

type Serving = JoinHandle<Result<(), TransportError>>;

/// Serve [`app`] on a free loopback port until the returned trigger fires.
async fn serve_app() -> (u16, oneshot::Sender<()>, Serving) {
    serve_app_on("127.0.0.1", None).await
}

/// Serve [`app`] bound to `bind` on a free port, with `allowed_hosts`, until
/// the returned trigger fires; the port is reachable through 127.0.0.1.
///
/// The probed port is free only until another test in this binary is handed
/// it. The serve task then fails to bind, and a connect would reach that
/// test's server instead. So the task is let run its bind before the port is
/// judged, and again after a connect succeeds: a serve that ended on a bind
/// error lost the race, and a fresh port is tried.
async fn serve_app_on(
    bind: &'static str,
    allowed_hosts: Option<Vec<String>>,
) -> (u16, oneshot::Sender<()>, Serving) {
    for _ in 0..10 {
        let port = free_port();
        let (trigger, fired) = oneshot::channel::<()>();
        let hosts = allowed_hosts.clone();
        let serving = tokio::spawn(async move {
            serve_router_with_shutdown(app(), bind, port, hosts.as_deref(), async {
                fired.await.ok();
            })
            .await
        });
        for _ in 0..100 {
            // Let the serve task run its bind before judging the port.
            task::yield_now().await;
            if serving.is_finished() {
                break;
            }
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                task::yield_now().await;
                if !serving.is_finished() {
                    return (port, trigger, serving);
                }
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(serving.is_finished(), "the router never started listening");
        match serving.await.expect("the serving task did not panic") {
            Err(TransportError::Bind { .. }) => {}
            other => panic!("the router stopped: {other:?}"),
        }
    }
    panic!("another test took every port probed for the router");
}

/// `GET path` on 127.0.0.1:`port`, naming `host` in `Host`, as raw HTTP.
async fn get_raw(port: u16, host: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the server accepts a connection");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nX-Request-Id: req-serve-router-1\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    response
}

#[tokio::test]
async fn serves_the_hosts_router_under_the_request_guard() {
    let (port, trigger, serving) = serve_app().await;
    let host = format!("127.0.0.1:{port}");

    let health = get_raw(port, &host, "/health").await;
    let panicked = get_raw(port, &host, "/boom").await;
    trigger.send(()).expect("the server is still waiting");
    timeout(Duration::from_secs(5), serving)
        .await
        .expect("it stops on the trigger")
        .expect("the serving task did not panic")
        .expect("it stops cleanly");

    assert!(health.starts_with("HTTP/1.1 200"), "{health:?}");
    assert!(health.ends_with("healthy"), "the route's body: {health:?}");
    assert!(
        health
            .to_ascii_lowercase()
            .contains(&format!("{REQUEST_ID_HEADER}: req-serve-router-1")),
        "the guard echoes the caller's request id: {health:?}"
    );
    assert!(
        panicked.starts_with("HTTP/1.1 500"),
        "the guard answers a panic with a 500, not a dropped connection: {panicked:?}"
    );
    assert!(
        panicked.contains(HANDLER_PANIC),
        "the 500 carries the panic error type: {panicked:?}"
    );
}

/// A page that rebound its own name to 127.0.0.1 reaches a loopback server
/// with that name in `Host`; the router must not answer it.
#[tokio::test]
async fn a_loopback_bind_refuses_a_foreign_host() {
    let (port, trigger, serving) = serve_app().await;

    let rebound = get_raw(port, &format!("attacker.example:{port}"), "/health").await;
    let localhost = get_raw(port, &format!("localhost:{port}"), "/health").await;
    trigger.send(()).expect("the server is still waiting");
    serving.await.expect("no panic").expect("clean stop");

    assert!(
        rebound.starts_with("HTTP/1.1 403"),
        "a foreign Host is refused on loopback: {rebound:?}"
    );
    assert!(rebound.contains("Host not allowed"), "{rebound:?}");
    assert!(
        localhost.starts_with("HTTP/1.1 200"),
        "a loopback name is served: {localhost:?}"
    );
}

/// A router reached through a proxy that forwards another name binds a
/// non-loopback address: no `Host` guard is applied there.
#[tokio::test]
async fn a_non_loopback_bind_serves_any_host() {
    let (port, trigger, serving) = serve_app_on("0.0.0.0", None).await;

    let foreign = get_raw(port, &format!("attacker.example:{port}"), "/health").await;
    trigger.send(()).expect("the server is still waiting");
    serving.await.expect("no panic").expect("clean stop");

    assert!(
        foreign.starts_with("HTTP/1.1 200"),
        "a non-loopback bind is not Host-guarded: {foreign:?}"
    );
    assert!(
        foreign.ends_with("healthy"),
        "the route's body: {foreign:?}"
    );
}

/// A loopback router behind a tunnel names the public host it forwards, the
/// opt-out `McpServer::with_allowed_hosts` gives `serve`.
#[tokio::test]
async fn a_loopback_bind_serves_the_hosts_it_is_given() {
    let (port, trigger, serving) =
        serve_app_on("127.0.0.1", Some(vec!["tunnel.example".to_owned()])).await;

    let tunnelled = get_raw(port, "tunnel.example", "/health").await;
    let localhost = get_raw(port, &format!("localhost:{port}"), "/health").await;
    let other = get_raw(port, "attacker.example", "/health").await;
    trigger.send(()).expect("the server is still waiting");
    serving.await.expect("no panic").expect("clean stop");

    assert!(
        tunnelled.starts_with("HTTP/1.1 200"),
        "a listed Host is served on loopback: {tunnelled:?}"
    );
    assert!(
        localhost.starts_with("HTTP/1.1 200"),
        "a loopback name is still served: {localhost:?}"
    );
    assert!(
        other.starts_with("HTTP/1.1 403"),
        "a Host the list does not name is refused: {other:?}"
    );
}

#[tokio::test]
async fn it_stops_on_the_trigger_and_accepts_nothing_after() {
    let (port, trigger, serving) = serve_app().await;

    trigger.send(()).expect("the server is still waiting");
    let outcome = timeout(Duration::from_secs(5), serving)
        .await
        .expect("serve_router_with_shutdown returns once triggered")
        .expect("the serving task did not panic");

    assert!(
        matches!(outcome, Ok(())),
        "a triggered stop is Ok: {outcome:?}"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "a stopped server accepts no connection"
    );
}

#[tokio::test]
async fn a_port_in_use_is_a_typed_bind_error() {
    let held = StdTcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
    let port = held.local_addr().expect("local addr").port();

    let outcome = timeout(
        Duration::from_secs(5),
        serve_router_with_shutdown(app(), "127.0.0.1", port, None, async {}),
    )
    .await
    .expect("a failed bind returns at once");
    drop(held);

    match outcome {
        Err(TransportError::Bind { addr, source }) => {
            assert_eq!(addr, format!("127.0.0.1:{port}"));
            assert_eq!(source.kind(), io::ErrorKind::AddrInUse);
        }
        other => panic!("expected TransportError::Bind, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unresolvable_host_is_a_typed_resolve_error() {
    let outcome = timeout(
        Duration::from_secs(10),
        serve_router_with_shutdown(app(), "no-such-host.invalid", 0, None, async {}),
    )
    .await
    .expect("a failed lookup returns");

    match outcome {
        Err(TransportError::Resolve { addr, .. }) => {
            assert_eq!(addr, "no-such-host.invalid:0");
        }
        other => panic!("expected TransportError::Resolve, got {other:?}"),
    }
}
