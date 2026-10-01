// ABOUTME: Tests ServiceClient against a raw-socket stand-in and a real request_guard server
// ABOUTME: Pins the one GET re-send, transport classification, auth, request ids, and shed / unfinished decoding
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "service-client")]
// Same allowances tests/integration_test.rs carries: an integration test is not
// covered by the lib's `cfg_attr(test, ...)`, and the panicking handler here is
// the behaviour under test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::collections::VecDeque;
use std::env;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap as AxumHeaderMap, HeaderValue};
use axum::middleware::from_fn;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use dravr_tronc::iam::{IamError, METADATA_HOST_ENV};
use dravr_tronc::server::request_guard::{
    enforce_deadline, guard_requests, RequestId, REQUEST_ID_HEADER,
};
use dravr_tronc::server::shed::shed_response;
use dravr_tronc::service_client::{
    ConfigError, ServiceClient, ServiceError, ServiceResponse, StatusCode, TransportFailure,
    Unfinished, DEFAULT_SHED_RETRY_AFTER_SECS,
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

/// What the stand-in service does with the next connection, once it has read
/// the whole request.
enum Step {
    /// Close the connection without a response.
    Drop,
    /// Answer, then close.
    Answer {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    },
    /// Keep the connection open and silent for this long, then close it.
    Hold(Duration),
    /// Answer this status announcing 100 body bytes, send five, and close.
    Truncated(u16),
}

impl Step {
    fn json(status: u16, body: &Value) -> Self {
        Self::Answer {
            status,
            headers: vec![("content-type", "application/json".to_owned())],
            body: body.to_string(),
        }
    }

    /// A gateway's own page: no JSON, nothing the service wrote.
    fn html(status: u16) -> Self {
        Self::Answer {
            status,
            headers: vec![("content-type", "text/html".to_owned())],
            body: format!("<html><body><h1>Error: {status}</h1></body></html>"),
        }
    }
}

/// Read one whole HTTP request: the head, then as much body as its
/// `content-length` announces.
async fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&bytes);
        if let Some(head_end) = text.find("\r\n\r\n") {
            let announced = text[..head_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= head_end + 4 + announced {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A stand-in service on a loopback port: it records each request raw and then
/// performs the next step. A connection past the last step is closed unanswered.
async fn spawn_stub(steps: Vec<Step>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&seen);
    let mut steps = VecDeque::from(steps);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let request = read_request(&mut stream).await;
            log.lock().unwrap().push(request);
            match steps.pop_front() {
                // Closed after the request was read in full, so the client
                // sees the connection end mid-exchange.
                None | Some(Step::Drop) => drop(stream),
                Some(Step::Answer {
                    status,
                    headers,
                    body,
                }) => {
                    let extra = headers
                        .iter()
                        .map(|(name, value)| format!("{name}: {value}\r\n"))
                        .collect::<Vec<_>>()
                        .concat();
                    let response = format!(
                        "HTTP/1.1 {status} Stub\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
                Some(Step::Hold(how_long)) => {
                    tokio::spawn(async move {
                        sleep(how_long).await;
                        drop(stream);
                    });
                }
                Some(Step::Truncated(status)) => {
                    let response = format!(
                        "HTTP/1.1 {status} Stub\r\ncontent-length: 100\r\nconnection: close\r\n\r\nhello"
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
        }
    });
    (addr, seen)
}

/// The requests the stand-in has read so far.
fn requests(seen: &Mutex<Vec<String>>) -> Vec<String> {
    seen.lock().unwrap().clone()
}

/// Every value of header `name` in a raw request.
fn headers_of(raw: &str, name: &str) -> Vec<String> {
    let head = raw.split("\r\n\r\n").next().unwrap_or_default();
    head.lines()
        .skip(1)
        .filter_map(|line| {
            let (header, value) = line.split_once(':')?;
            header
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
        .collect()
}

/// The value of header `name` in a raw request, or an empty string.
fn header_of(raw: &str, name: &str) -> String {
    headers_of(raw, name).into_iter().next().unwrap_or_default()
}

fn request_line(raw: &str) -> &str {
    raw.lines().next().unwrap_or_default()
}

fn body_of(raw: &str) -> &str {
    raw.split_once("\r\n\r\n").map_or("", |(_, body)| body)
}

/// A client for a service on loopback: no audience, no token.
fn loopback_client(addr: SocketAddr) -> ServiceClient {
    ServiceClient::new("svc", &format!("http://{addr}"), None, Client::new()).unwrap()
}

/// The error of a call that must not have succeeded.
fn failed(outcome: Result<ServiceResponse, ServiceError>) -> ServiceError {
    match outcome {
        Err(error) => error,
        Ok(response) => panic!("expected an error, got a {} response", response.status()),
    }
}

/// The transport failure of a call that must have produced no response.
fn transport_failure(error: &ServiceError) -> TransportFailure {
    match error {
        ServiceError::Transport { failure, .. } => *failure,
        other => panic!("expected a transport failure, got: {other}"),
    }
}

/// A loopback address nothing listens on.
fn closed_port() -> SocketAddr {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr
}

// ---------------------------------------------------------------------------
// Re-send
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_get_dropped_once_is_resent_and_answered() {
    let (addr, seen) = spawn_stub(vec![Step::Drop, Step::json(200, &json!({"ok": true}))]).await;

    let response = loopback_client(addr)
        .get("list", "/api/items")
        .send()
        .await
        .expect("the re-sent GET is answered");

    let seen = requests(&seen);
    assert_eq!(seen.len(), 2, "one send and exactly one re-send");
    assert!(seen
        .iter()
        .all(|raw| request_line(raw) == "GET /api/items HTTP/1.1"));
    let first_id = header_of(&seen[0], REQUEST_ID_HEADER);
    let second_id = header_of(&seen[1], REQUEST_ID_HEADER);
    assert!(!first_id.is_empty() && !second_id.is_empty());
    assert_ne!(
        first_id, second_id,
        "each attempt goes out under its own id"
    );

    assert!(response.exchange().resent);
    assert_eq!(response.exchange().request_id.as_str(), second_id);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json_value()["ok"], true);
}

#[tokio::test]
async fn a_get_dropped_twice_is_resent_exactly_once() {
    // A third step that would answer: it must never be reached.
    let (addr, seen) = spawn_stub(vec![
        Step::Drop,
        Step::Drop,
        Step::json(200, &json!({"ok": true})),
    ])
    .await;

    let error = failed(
        loopback_client(addr)
            .get("list", "/api/items")
            .query(&[("limit", "5")])
            .send()
            .await,
    );

    let seen = requests(&seen);
    assert_eq!(seen.len(), 2, "a second close is not re-sent again");
    assert_eq!(
        transport_failure(&error),
        TransportFailure::ClosedBeforeResponse
    );
    let exchange = error.exchange().expect("a sent attempt has an exchange");
    assert!(exchange.resent);
    let second_id = header_of(&seen[1], REQUEST_ID_HEADER);
    assert_eq!(exchange.request_id.as_str(), second_id);
    assert_ne!(second_id, header_of(&seen[0], REQUEST_ID_HEADER));

    let text = error.to_string();
    assert!(
        text.contains("saw the connection close before any response"),
        "{text}"
    );
    assert!(text.contains(&second_id), "{text}");
    assert!(text.starts_with("svc list request ("), "{text}");
    assert!(
        !text.contains("limit=5") && !text.contains("/api/items"),
        "the message must carry no part of the URL: {text}"
    );
}

#[tokio::test]
async fn a_post_dropped_is_never_resent() {
    let (addr, seen) = spawn_stub(vec![Step::Drop, Step::json(200, &json!({"ok": true}))]).await;

    let error = failed(
        loopback_client(addr)
            .post_json("create", "/api/items", &json!({"k": 1}))
            .send()
            .await,
    );

    let seen = requests(&seen);
    assert_eq!(seen.len(), 1, "a POST may have run; it is never re-sent");
    assert_eq!(
        transport_failure(&error),
        TransportFailure::ClosedBeforeResponse
    );
    let exchange = error.exchange().unwrap();
    assert!(!exchange.resent);
    assert_eq!(
        exchange.request_id.as_str(),
        header_of(&seen[0], REQUEST_ID_HEADER)
    );
}

#[tokio::test]
async fn a_delete_dropped_is_never_resent() {
    let (addr, seen) = spawn_stub(vec![Step::Drop, Step::json(200, &json!({"ok": true}))]).await;

    let error = failed(
        loopback_client(addr)
            .delete("drop", "/api/items/7")
            .send()
            .await,
    );

    let seen = requests(&seen);
    assert_eq!(seen.len(), 1);
    assert_eq!(request_line(&seen[0]), "DELETE /api/items/7 HTTP/1.1");
    assert_eq!(
        transport_failure(&error),
        TransportFailure::ClosedBeforeResponse
    );
}

#[tokio::test]
async fn an_answered_get_is_never_resent() {
    let (addr, seen) = spawn_stub(vec![
        Step::json(500, &json!({"error": "x"})),
        Step::json(200, &json!({"ok": true})),
    ])
    .await;

    let response = loopback_client(addr)
        .get("list", "/api/items")
        .send()
        .await
        .expect("a service's own 500 is a response");

    assert_eq!(requests(&seen).len(), 1);
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!response.exchange().resent);
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_timeout_is_classified_and_not_resent() {
    let (addr, seen) = spawn_stub(vec![
        Step::Hold(Duration::from_secs(2)),
        Step::json(200, &json!({"ok": true})),
    ])
    .await;

    let error = failed(
        loopback_client(addr)
            .get("list", "/api/items")
            .timeout(Duration::from_millis(100))
            .send()
            .await,
    );

    assert_eq!(transport_failure(&error), TransportFailure::TimedOut);
    assert_eq!(requests(&seen).len(), 1, "a timeout is never re-sent");
    let exchange = error.exchange().unwrap();
    assert!(!exchange.resent);
    assert!(
        exchange.elapsed >= Duration::from_millis(100),
        "elapsed was {:?}",
        exchange.elapsed
    );
    let text = error.to_string();
    assert!(text.contains("timed out waiting for a response"), "{text}");
}

#[tokio::test]
async fn an_unreachable_service_is_classified() {
    let error = failed(
        loopback_client(closed_port())
            .get("list", "/api/items")
            .send()
            .await,
    );

    assert_eq!(transport_failure(&error), TransportFailure::Unreachable);
    let text = error.to_string();
    assert!(text.contains("could not connect"), "{text}");
}

#[tokio::test]
async fn a_request_that_cannot_be_built_is_other() {
    let (addr, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;

    let error = failed(
        loopback_client(addr)
            .get("list", "/api/items")
            .header("x-session-id", "a\nb")
            .send()
            .await,
    );

    assert_eq!(transport_failure(&error), TransportFailure::Other);
    let text = error.to_string();
    assert!(text.contains("could not be sent"), "{text}");
    assert!(requests(&seen).is_empty(), "nothing reaches the network");
}

#[tokio::test]
async fn a_body_cut_short_is_a_body_error() {
    let (addr, seen) = spawn_stub(vec![
        Step::Truncated(200),
        Step::json(200, &json!({"ok": true})),
    ])
    .await;

    let error = failed(loopback_client(addr).get("list", "/api/items").send().await);

    match &error {
        ServiceError::Body { status, .. } => assert_eq!(*status, StatusCode::OK),
        other => panic!("expected a body error, got: {other}"),
    }
    assert!(error.exchange().is_some());
    assert_eq!(
        requests(&seen).len(),
        1,
        "an answered request is never re-sent, even when its body is cut"
    );
    let text = error.to_string();
    assert!(
        text.contains("was answered 200 OK but its body could not be read"),
        "{text}"
    );
}

#[tokio::test]
async fn the_send_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}

    let client = loopback_client(closed_port());
    let pending = client.get("list", "/api/items").send();
    assert_send(&pending);
    drop(pending);
}

#[tokio::test]
async fn a_cut_body_keeps_the_name_its_status_gives() {
    // The body never arrives; the status alone still says shed or unfinished.
    let (addr, seen) = spawn_stub(vec![
        Step::Truncated(503),
        Step::Truncated(504),
        Step::Truncated(502),
        Step::Truncated(500),
    ])
    .await;
    let client = loopback_client(addr);
    let call = || async { failed(client.get("list", "/api/items").send().await) };

    match call().await {
        ServiceError::Shed {
            status,
            retry_after_secs,
            reason,
            ..
        } => {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(retry_after_secs, DEFAULT_SHED_RETRY_AFTER_SECS);
            assert_eq!(reason, None);
        }
        other => panic!("a cut 503 is still a shed, got: {other}"),
    }
    for (status, expected) in [
        (StatusCode::GATEWAY_TIMEOUT, Unfinished::GatewayDeadline),
        (StatusCode::BAD_GATEWAY, Unfinished::BadGateway),
    ] {
        match call().await {
            ServiceError::Unfinished {
                status: seen,
                kind,
                detail,
                ..
            } => {
                assert_eq!(seen, status);
                assert_eq!(kind, expected);
                assert_eq!(detail, None);
            }
            other => panic!("a cut {status} is still unfinished, got: {other}"),
        }
    }
    // A 500 is named only by its body, so without one it is a body error.
    match call().await {
        ServiceError::Body { status, .. } => {
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        }
        other => panic!("a cut 500 is a body error, got: {other}"),
    }
    assert_eq!(
        requests(&seen).len(),
        4,
        "an answered request is never re-sent"
    );
}

#[tokio::test]
async fn a_path_that_could_leave_the_service_is_never_sent() {
    let (addr, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;
    let client = loopback_client(addr);

    // Appended to the base URL, each of these names another host or port.
    for path in ["@evil.test/x", ".evil.test/x", "0/x", "api/items", ""] {
        for outcome in [
            client.get("list", path).send().await,
            client.post_json("create", path, &json!({})).send().await,
            client.delete("drop", path).send().await,
        ] {
            let error = failed(outcome);
            assert_eq!(transport_failure(&error), TransportFailure::Other);
            let text = error.to_string();
            assert!(text.contains("does not start with '/'"), "{text}");
            assert!(
                !text.contains("evil.test"),
                "the path is not echoed: {text}"
            );
        }
    }
    assert!(requests(&seen).is_empty(), "nothing reached the network");
}

// ---------------------------------------------------------------------------
// Request shape and auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_request_carries_exactly_one_minted_request_id() {
    let (addr, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;

    let response = loopback_client(addr)
        .get("list", "/api/items")
        .header(REQUEST_ID_HEADER, "mine")
        .send()
        .await
        .unwrap();

    let seen = requests(&seen);
    let ids = headers_of(&seen[0], REQUEST_ID_HEADER);
    assert_eq!(ids.len(), 1, "exactly one id on the wire, got {ids:?}");
    assert_ne!(ids[0], "mine", "the caller's value is replaced, not sent");
    assert_eq!(ids[0], response.exchange().request_id.as_str());

    // The guard adopts the id as it is, so one id names the request on both sides.
    let mut headers = AxumHeaderMap::new();
    headers.insert(REQUEST_ID_HEADER, HeaderValue::from_str(&ids[0]).unwrap());
    assert_eq!(RequestId::from_headers(&headers).as_str(), ids[0]);
}

#[tokio::test]
async fn query_body_and_headers_reach_the_service() {
    let (addr, seen) = spawn_stub(vec![
        Step::json(200, &json!({"ok": true})),
        Step::json(200, &json!({"ok": true})),
    ])
    .await;
    let client = loopback_client(addr);

    client
        .get("list", "/api/items")
        .query(&[("limit", "5"), ("athlete", "a b")])
        .header("x-session-id", "s1")
        .send()
        .await
        .unwrap();
    client
        .post_json("create", "/api/items", &json!({"k": 1}))
        .send()
        .await
        .unwrap();

    let seen = requests(&seen);
    assert_eq!(
        request_line(&seen[0]),
        "GET /api/items?limit=5&athlete=a+b HTTP/1.1"
    );
    assert_eq!(header_of(&seen[0], "x-session-id"), "s1");

    assert_eq!(request_line(&seen[1]), "POST /api/items HTTP/1.1");
    assert_eq!(header_of(&seen[1], "content-type"), "application/json");
    let body: Value = serde_json::from_str(body_of(&seen[1])).unwrap();
    assert_eq!(body, json!({"k": 1}));
}

#[tokio::test]
async fn a_loopback_service_is_called_without_a_token() {
    let (addr, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;

    // An audience is given and ignored: loopback decides.
    let client =
        ServiceClient::new("svc", &format!("http://{addr}"), Some("aud"), Client::new()).unwrap();
    // Not even the caller's own: `Authorization` is the client's to set.
    client
        .get("list", "/api/items")
        .header("authorization", "Bearer mine")
        .send()
        .await
        .unwrap();

    let seen = requests(&seen);
    assert_eq!(seen.len(), 1);
    assert!(
        headers_of(&seen[0], "authorization").is_empty(),
        "a loopback service must be called with no Authorization header: {}",
        seen[0]
    );
}

/// A client for a service that is not on loopback by name, resolved to the
/// stand-in at `service`. The stand-in speaks no TLS, so nothing sent through
/// this client is ever answered: it is for what happens before the send. The
/// bearer header itself is pinned beside the client, in `client.rs`.
fn remote_client(service: SocketAddr) -> ServiceClient {
    let http = Client::builder()
        .no_proxy()
        .resolve("service.test", service)
        .build()
        .unwrap();
    ServiceClient::new(
        "svc",
        &format!("https://service.test:{}", service.port()),
        Some("test-audience"),
        http,
    )
    .unwrap()
}

#[tokio::test]
#[serial]
async fn a_token_that_cannot_be_minted_sends_nothing() {
    let (service, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;
    let client = remote_client(service);

    env::set_var(METADATA_HOST_ENV, closed_port().to_string());
    let outcome = client.get("list", "/api/items").send().await;
    env::remove_var(METADATA_HOST_ENV);
    let error = failed(outcome);

    match &error {
        ServiceError::Identity {
            service,
            operation,
            source: IamError::MetadataUnavailable(_),
        } => {
            assert_eq!(service, "svc");
            assert_eq!(operation, "list");
        }
        other => panic!("expected an identity error, got: {other}"),
    }
    assert!(error.exchange().is_none(), "nothing was sent");
    let text = error.to_string();
    assert!(
        text.starts_with("could not mint an identity token for svc"),
        "{text}"
    );
    assert!(requests(&seen).is_empty(), "no request without a token");
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn loopback_hosts_need_no_audience() {
    for url in [
        "http://127.0.0.1:1",
        "http://localhost:1/",
        "http://[::1]:8080",
        "http://127.0.0.2",
    ] {
        let client = ServiceClient::new("svc", url, None, Client::new())
            .unwrap_or_else(|error| panic!("{url} is loopback and needs no audience: {error}"));
        assert_eq!(client.service(), "svc");
        assert_eq!(client.base_url(), url.trim_end_matches('/'));
    }
}

#[test]
fn a_remote_url_without_an_audience_is_refused() {
    for audience in [None, Some("")] {
        match ServiceClient::new("svc", "https://svc.example", audience, Client::new()) {
            Err(ConfigError::AudienceRequired { service }) => assert_eq!(service, "svc"),
            Err(other) => panic!("expected AudienceRequired, got: {other}"),
            Ok(_) => panic!("a reachable service must not be callable unauthenticated"),
        }
    }
}

#[test]
fn an_unusable_base_url_is_refused() {
    for url in [
        "not a url",
        "localhost:8080",
        "ftp://x",
        // Every path would be appended to the query or the fragment.
        "https://svc.example?x=1",
        "https://svc.example/#top",
        "https://user:secret@svc.example",
        // An identity token is never sent in clear.
        "http://svc.example",
        "http://svc.example:8080/",
    ] {
        match ServiceClient::new("svc", url, Some("aud"), Client::new()) {
            Err(error @ ConfigError::InvalidBaseUrl { .. }) => {
                let text = error.to_string();
                assert!(text.starts_with("svc base URL is not usable: "), "{text}");
                assert!(!text.contains("secret"), "{text}");
            }
            Err(other) => panic!("expected InvalidBaseUrl for {url:?}, got: {other}"),
            Ok(_) => panic!("{url:?} must be refused"),
        }
    }
}

#[test]
fn a_remote_service_in_clear_is_refused_for_its_scheme() {
    let Err(error) = ServiceClient::new("svc", "http://svc.example", Some("aud"), Client::new())
    else {
        panic!("a token must never be sent over plain http");
    };
    assert!(matches!(error, ConfigError::InvalidBaseUrl { .. }));
    let text = error.to_string();
    assert!(text.contains("must be https"), "{text}");
}

#[tokio::test]
#[serial]
async fn from_env_reads_the_named_variables() {
    const URL_VAR: &str = "TRONC_TEST_SVC_URL";
    const AUDIENCE_VAR: &str = "TRONC_TEST_SVC_AUDIENCE";
    let from_env = || ServiceClient::from_env("svc", URL_VAR, AUDIENCE_VAR, Duration::from_secs(5));

    // Not configured: unset, empty, or only a slash.
    env::remove_var(URL_VAR);
    env::remove_var(AUDIENCE_VAR);
    assert!(from_env().unwrap().is_none());
    for unset in ["", "/"] {
        env::set_var(URL_VAR, unset);
        assert!(from_env().unwrap().is_none(), "{unset:?} counts as unset");
    }

    // A reachable service with no audience is a misconfiguration, not "off".
    env::set_var(URL_VAR, "https://svc.example");
    for audience in [None, Some("")] {
        match audience {
            Some(value) => env::set_var(AUDIENCE_VAR, value),
            None => env::remove_var(AUDIENCE_VAR),
        }
        match from_env() {
            Err(error @ ConfigError::AudienceNotSet { .. }) => {
                let text = error.to_string();
                assert!(
                    text.contains(URL_VAR) && text.contains(AUDIENCE_VAR),
                    "{text}"
                );
            }
            Err(other) => panic!("expected AudienceNotSet, got: {other}"),
            Ok(_) => panic!("a reachable URL without an audience must be an error"),
        }
    }

    env::set_var(AUDIENCE_VAR, "https://svc.example");
    let remote = from_env().unwrap().expect("URL and audience are both set");
    assert_eq!(remote.base_url(), "https://svc.example");

    // Loopback with a trailing slash: built with no audience, and the slash
    // does not double in the request path.
    let (addr, seen) = spawn_stub(vec![Step::json(200, &json!({"ok": true}))]).await;
    env::set_var(URL_VAR, format!("http://{addr}/"));
    env::remove_var(AUDIENCE_VAR);
    let local = from_env();
    env::remove_var(URL_VAR);
    let local = local.unwrap().expect("a loopback URL needs no audience");

    local.get("health", "/health").send().await.unwrap();
    let seen = requests(&seen);
    assert_eq!(request_line(&seen[0]), "GET /health HTTP/1.1");
}

// ---------------------------------------------------------------------------
// Guard and shed, against the real server half
// ---------------------------------------------------------------------------

async fn slow_handler() -> &'static str {
    sleep(Duration::from_secs(2)).await;
    "late"
}

async fn panicking_handler() -> &'static str {
    let row = 7;
    panic!("handler blew up at row {row}")
}

async fn busy_handler() -> Response {
    shed_response("busy", "queue full", 12)
}

/// A router under the real request guard, with a 50 ms deadline, served on an
/// ephemeral loopback port.
async fn guarded_app() -> SocketAddr {
    let deadline = Duration::from_millis(50);
    let app = Router::new()
        .route("/slow", get(slow_handler))
        .route("/panic", get(panicking_handler))
        .route("/busy", get(busy_handler))
        .layer(from_fn(move |request, next| {
            enforce_deadline(deadline, request, next)
        }))
        .layer(from_fn(guard_requests));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
}

#[tokio::test]
async fn the_guards_deadline_is_a_service_deadline() {
    let addr = guarded_app().await;

    let error = failed(loopback_client(addr).get("slow", "/slow").send().await);

    let ServiceError::Unfinished {
        exchange,
        status,
        kind,
        detail,
    } = &error
    else {
        panic!("expected an unfinished request, got: {error}");
    };
    assert_eq!(*kind, Unfinished::ServiceDeadline);
    assert_eq!(*status, StatusCode::GATEWAY_TIMEOUT);
    assert!(
        detail.as_deref().is_some_and(|d| d.contains("50ms")),
        "the guard's own message must come through: {detail:?}"
    );
    let text = error.to_string();
    assert!(text.contains("request deadline"), "{text}");
    assert!(text.contains(exchange.request_id.as_str()), "{text}");
}

#[tokio::test]
async fn a_handler_panic_is_named() {
    let addr = guarded_app().await;

    let error = failed(loopback_client(addr).get("panic", "/panic").send().await);

    let ServiceError::Unfinished {
        exchange,
        status,
        kind,
        detail,
    } = &error
    else {
        panic!("expected an unfinished request, got: {error}");
    };
    assert_eq!(*kind, Unfinished::HandlerPanic);
    assert_eq!(*status, StatusCode::INTERNAL_SERVER_ERROR);
    // The guard names the id it logged the panic under: the one the client sent.
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.contains(exchange.request_id.as_str())),
        "the guard must have adopted the client's id {}: {detail:?}",
        exchange.request_id
    );
    assert!(!exchange.resent, "an answered request is never re-sent");
    let text = error.to_string();
    assert!(text.contains("panicked"), "{text}");
}

#[tokio::test]
async fn a_shed_built_by_the_server_helper_is_decoded() {
    let addr = guarded_app().await;

    let error = failed(loopback_client(addr).get("busy", "/busy").send().await);

    let ServiceError::Shed {
        status,
        retry_after_secs,
        reason,
        ..
    } = &error
    else {
        panic!("expected a shed, got: {error}");
    };
    assert_eq!(*status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(*retry_after_secs, 12);
    assert_eq!(reason.as_deref(), Some("queue full"));
    let text = error.to_string();
    assert!(text.contains("retry after 12s"), "{text}");
    assert!(text.ends_with(": queue full"), "{text}");
}

#[tokio::test]
async fn a_gateways_504_and_502_are_not_the_services() {
    let expected = [
        (504, Unfinished::GatewayDeadline, "gateway's deadline"),
        (502, Unfinished::BadGateway, "no usable answer"),
    ];
    for (status, expected_kind, phrase) in expected {
        let (addr, seen) = spawn_stub(vec![Step::html(status), Step::html(status)]).await;

        let error = failed(loopback_client(addr).get("list", "/api/items").send().await);

        let ServiceError::Unfinished { kind, detail, .. } = &error else {
            panic!("expected an unfinished request, got: {error}");
        };
        assert_eq!(*kind, expected_kind);
        assert_eq!(*detail, None, "a gateway's page carries no guard message");
        let text = error.to_string();
        assert!(text.contains(phrase), "{text}");
        assert!(text.ends_with(": no detail"), "{text}");
        assert_eq!(requests(&seen).len(), 1, "an answered GET is not re-sent");
    }
}

/// The shed a call to a stand-in answering `step` fails with.
async fn shed_from(step: Step) -> (StatusCode, u64, Option<String>) {
    let (addr, _seen) = spawn_stub(vec![step]).await;
    match failed(loopback_client(addr).get("list", "/api/items").send().await) {
        ServiceError::Shed {
            status,
            retry_after_secs,
            reason,
            ..
        } => (status, retry_after_secs, reason),
        other => panic!("expected a shed, got: {other}"),
    }
}

#[tokio::test]
async fn a_wait_on_another_status_is_still_a_shed() {
    let shed = shed_from(Step::json(429, &json!({"retry_after_secs": 7}))).await;
    assert_eq!(shed, (StatusCode::TOO_MANY_REQUESTS, 7, None));
}

#[tokio::test]
async fn a_shed_without_a_usable_wait_falls_back() {
    assert_eq!(DEFAULT_SHED_RETRY_AFTER_SECS, 30);
    for unusable in [json!(-1), json!("soon"), json!(1.5)] {
        let body = json!({"error": "busy", "retry_after_secs": unusable});
        let (status, wait, _) = shed_from(Step::json(503, &body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(wait, DEFAULT_SHED_RETRY_AFTER_SECS, "for {body}");
    }

    // A gateway's bare page names no wait at all.
    let (_, wait, reason) = shed_from(Step::html(503)).await;
    assert_eq!(wait, DEFAULT_SHED_RETRY_AFTER_SECS);
    assert_eq!(reason, None);

    // The standard header is read when the body names none.
    let (_, wait, _) = shed_from(Step::Answer {
        status: 503,
        headers: vec![
            ("content-type", "text/html".to_owned()),
            ("retry-after", "9".to_owned()),
        ],
        body: "<html></html>".to_owned(),
    })
    .await;
    assert_eq!(wait, 9);
}

#[tokio::test]
async fn a_services_own_failures_come_back_as_responses() {
    let own = [
        (500, json!({"error": "chrome crashed"})),
        (
            500,
            json!({"error": {"type": "internal_error", "message": "x"}}),
        ),
        (401, json!({"error": "session_expired"})),
    ];
    for (status, body) in own {
        let (addr, _seen) = spawn_stub(vec![Step::json(status, &body)]).await;
        let response = loopback_client(addr)
            .get("list", "/api/items")
            .send()
            .await
            .unwrap_or_else(|error| panic!("{status} {body} is the service's answer: {error}"));
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.json_value()["error"], body["error"]);
        assert_eq!(response.bytes(), body.to_string().as_bytes());
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    // An empty body is not JSON, and reads as null rather than failing.
    let (addr, _seen) = spawn_stub(vec![Step::Answer {
        status: 404,
        headers: Vec::new(),
        body: String::new(),
    }])
    .await;
    let response = loopback_client(addr)
        .delete("drop", "/api/items/7")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.json_value(), Value::Null);
}

#[derive(Debug, Deserialize)]
struct Counted {
    count: u32,
}

#[tokio::test]
async fn a_typed_decode_failure_names_the_request() {
    let (addr, _seen) = spawn_stub(vec![
        Step::json(200, &json!({"count": "s3cr3t-cookie"})),
        Step::json(200, &json!({"count": 3})),
    ])
    .await;
    let client = loopback_client(addr);

    let response = client.get("count", "/api/count").send().await.unwrap();
    let error = match response.json::<Counted>() {
        Err(error) => error,
        Ok(counted) => panic!("a string is not a count: {counted:?}"),
    };
    let ServiceError::Decode {
        exchange, status, ..
    } = &error
    else {
        panic!("expected a decode error, got: {error}");
    };
    assert_eq!(*status, StatusCode::OK);
    assert_eq!(exchange.request_id, response.exchange().request_id);
    let text = error.to_string();
    assert!(text.starts_with("svc count response ("), "{text}");
    assert!(text.contains(exchange.request_id.as_str()), "{text}");
    assert!(text.contains("is not the expected shape"), "{text}");
    // Where and what kind, never the value: a response can carry a secret.
    assert!(text.contains("at line 1 column"), "{text}");
    assert!(!text.contains("s3cr3t-cookie"), "{text}");

    let response = client.get("count", "/api/count").send().await.unwrap();
    assert_eq!(response.json::<Counted>().unwrap().count, 3);
}
