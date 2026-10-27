// ABOUTME: The SSRF-guarded fetch against a real local server, names answered by a scripted resolver
// ABOUTME: Pins IP pinning against rebinding, per-hop redirect checks, the hop cap and the byte/idle/total caps
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{HOST, LOCATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use dravr_tronc::http_client::guard::{
    FetchLimits, FetchPolicy, GuardError, GuardedClient, HostAllowlist, HostResolver,
};
use futures::future::BoxFuture;
use futures::stream;
use futures::StreamExt as _;
use tokio::net::TcpListener;
use tokio::time::sleep;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const METADATA: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));

/// A resolver that answers each lookup from a script, then repeats its last
/// answer, and counts the lookups per name.
struct ScriptedResolver {
    answers: Mutex<VecDeque<Vec<IpAddr>>>,
    last: Mutex<Vec<IpAddr>>,
    lookups: Mutex<Vec<String>>,
}

impl ScriptedResolver {
    fn new(script: Vec<Vec<IpAddr>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(script.into()),
            last: Mutex::new(Vec::new()),
            lookups: Mutex::new(Vec::new()),
        })
    }

    fn always(answer: IpAddr) -> Arc<Self> {
        Self::new(vec![vec![answer]])
    }

    fn lookups(&self) -> Vec<String> {
        self.lookups.lock().unwrap().clone()
    }
}

impl HostResolver for ScriptedResolver {
    fn lookup(&self, host: &str) -> BoxFuture<'static, io::Result<Vec<IpAddr>>> {
        self.lookups.lock().unwrap().push(host.to_owned());
        let answer = {
            let next = self.answers.lock().unwrap().pop_front();
            let mut last = self.last.lock().unwrap();
            if let Some(next) = next {
                *last = next;
            }
            last.clone()
        };
        Box::pin(async move { Ok(answer) })
    }
}

#[derive(Clone)]
struct ServerState {
    port: u16,
    requests: Arc<AtomicUsize>,
}

/// Serve the test routes on a loopback port and return the port.
async fn serve() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let state = ServerState {
        port,
        requests: Arc::clone(&requests),
    };
    let app = Router::new()
        .route("/ok", get(ok))
        .route("/big", get(|| async { vec![b'x'; 4096] }))
        .route("/hop/{n}", get(hop))
        .route("/off-list", get(off_list))
        .route("/to-metadata", get(to_metadata))
        .route("/stall-head", get(stall_head))
        .route("/stall-body", get(stall_body))
        .route("/trickle", get(trickle))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await });
    (port, requests)
}

async fn ok(State(state): State<ServerState>, headers: HeaderMap) -> String {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    format!("ok host={host}")
}

fn redirect_to(location: &str) -> Response {
    (StatusCode::FOUND, [(LOCATION, location.to_owned())]).into_response()
}

/// Redirect `n` more times through alternating hosts, then land on `/ok`.
async fn hop(State(state): State<ServerState>, Path(n): Path<u32>) -> Response {
    let host = if n % 2 == 0 { "a.test" } else { "b.test" };
    if n == 0 {
        return redirect_to(&format!("http://{host}:{}/ok", state.port));
    }
    redirect_to(&format!("http://{host}:{}/hop/{}", state.port, n - 1))
}

async fn off_list(State(state): State<ServerState>) -> Response {
    redirect_to(&format!("http://evil.test:{}/ok", state.port))
}

async fn to_metadata() -> Response {
    redirect_to("http://169.254.169.254/latest/meta-data/")
}

async fn stall_head() -> &'static str {
    sleep(Duration::from_secs(30)).await;
    "late"
}

async fn stall_body() -> Response {
    let first = stream::once(async { Ok::<_, Infallible>(Bytes::from_static(b"first")) });
    let never = stream::once(async {
        sleep(Duration::from_secs(30)).await;
        Ok::<_, Infallible>(Bytes::from_static(b"late"))
    });
    Body::from_stream(first.chain(never)).into_response()
}

async fn trickle() -> Response {
    let chunks = stream::iter(0..1000).then(|_| async {
        sleep(Duration::from_millis(50)).await;
        Ok::<_, Infallible>(Bytes::from_static(b"."))
    });
    Body::from_stream(chunks).into_response()
}

fn listed(hosts: &[&str]) -> FetchPolicy {
    FetchPolicy::allowlist(HostAllowlist::new(hosts))
}

fn client(
    policy: FetchPolicy,
    resolver: Arc<ScriptedResolver>,
    limits: Option<FetchLimits>,
) -> GuardedClient {
    let mut builder = GuardedClient::builder(policy)
        .resolver(resolver)
        .permit_loopback_for_tests();
    if let Some(limits) = limits {
        builder = builder.limits(limits);
    }
    builder.build().unwrap()
}

#[tokio::test]
async fn a_fetch_connects_to_the_checked_address_and_keeps_the_host_name() {
    let (port, _) = serve().await;
    let resolver = ScriptedResolver::always(LOOPBACK);
    let client = client(FetchPolicy::webhook(), Arc::clone(&resolver), None);

    let body = client
        .get(&format!("http://hooks.test:{port}/ok"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    assert_eq!(
        String::from_utf8(body).unwrap(),
        format!("ok host=hooks.test:{port}")
    );
    assert_eq!(
        resolver.lookups(),
        vec!["hooks.test".to_owned()],
        "one lookup, the one checked"
    );
}

#[tokio::test]
async fn a_name_rebound_after_validation_is_refused_at_connect() {
    let (port, requests) = serve().await;
    // The first lookup (validation) sees loopback; the second (the fetch's
    // own) sees the metadata address the name was rebound to.
    let resolver = ScriptedResolver::new(vec![vec![LOOPBACK], vec![METADATA]]);
    let client = client(FetchPolicy::webhook(), Arc::clone(&resolver), None);
    let url = format!("http://rebind.test:{port}/ok");

    client.validate(&url).await.unwrap();
    let refusal = client.post(&url).unwrap().send().await.err().unwrap();

    assert_eq!(refusal, GuardError::DeniedAddress);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "nothing reached the server"
    );
    assert_eq!(resolver.lookups().len(), 2);
}

#[tokio::test]
async fn an_answer_mixing_a_public_and_a_denied_address_is_refused() {
    let resolver = ScriptedResolver::new(vec![vec![
        IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
    ]]);
    let client = client(FetchPolicy::webhook(), resolver, None);

    assert_eq!(
        client.validate("https://mixed.test/hook").await,
        Err(GuardError::DeniedAddress)
    );
    assert_eq!(
        client
            .get("https://mixed.test/hook")
            .unwrap()
            .send()
            .await
            .err(),
        Some(GuardError::DeniedAddress)
    );
}

#[tokio::test]
async fn a_name_that_does_not_resolve_is_refused() {
    let resolver = ScriptedResolver::new(vec![vec![]]);
    let client = client(FetchPolicy::webhook(), resolver, None);
    assert_eq!(
        client.validate("https://nowhere.test/").await,
        Err(GuardError::Unresolvable)
    );
}

#[tokio::test]
async fn webhook_returns_a_redirect_instead_of_following_it() {
    let (port, requests) = serve().await;
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        None,
    );

    let response = client
        .get(&format!("http://a.test:{port}/off-list"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "the redirect target was never fetched"
    );
}

#[tokio::test]
async fn every_redirect_hop_is_resolved_and_followed_up_to_the_cap() {
    let (port, requests) = serve().await;
    let resolver = ScriptedResolver::always(LOOPBACK);
    let client = client(listed(&["a.test", "b.test"]), Arc::clone(&resolver), None);

    // /hop/4 → … → /hop/0 → /ok: five redirects, the cap, alternating hosts.
    let response = client
        .get(&format!("http://a.test:{port}/hop/4"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let lookups = resolver.lookups();
    assert!(lookups.contains(&"a.test".to_owned()) && lookups.contains(&"b.test".to_owned()));
}

#[tokio::test]
async fn a_sixth_redirect_is_refused() {
    let (port, requests) = serve().await;
    let client = client(
        listed(&["a.test", "b.test"]),
        ScriptedResolver::always(LOOPBACK),
        None,
    );

    // /hop/5 needs six redirects to reach /ok.
    let refusal = client
        .get(&format!("http://b.test:{port}/hop/5"))
        .unwrap()
        .send()
        .await
        .err();

    assert_eq!(refusal, Some(GuardError::TooManyRedirects));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_redirect_off_the_allowlist_is_refused() {
    let (port, requests) = serve().await;
    let resolver = ScriptedResolver::always(LOOPBACK);
    let client = client(listed(&["a.test"]), Arc::clone(&resolver), None);

    let refusal = client
        .get(&format!("http://a.test:{port}/off-list"))
        .unwrap()
        .send()
        .await
        .err();

    assert_eq!(refusal, Some(GuardError::HostNotAllowed));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(
        !resolver.lookups().contains(&"evil.test".to_owned()),
        "never resolved"
    );
}

#[tokio::test]
async fn a_redirect_to_a_denied_literal_is_refused_under_archive_rules() {
    // The archive policy needs https on the default port, which a test
    // listener cannot offer, so the refusal is checked on the URL the hop
    // would follow — the same check the redirect policy runs.
    let client = client(
        FetchPolicy::archive_fetch(HostAllowlist::new(["export.test", "169.254.169.254"])),
        ScriptedResolver::always(LOOPBACK),
        None,
    );
    assert_eq!(
        client.check_url("https://169.254.169.254/latest").err(),
        Some(GuardError::DeniedAddress)
    );
    assert_eq!(
        client.check_url("http://export.test/a.zip").err(),
        Some(GuardError::SchemeNotAllowed)
    );
    assert!(client.check_url("https://export.test/a.zip").is_ok());
}

#[tokio::test]
async fn the_byte_cap_counts_the_bytes_received() {
    let (port, _) = serve().await;
    let policy = FetchPolicy::webhook();
    let limits = FetchLimits {
        max_body_bytes: 1024,
        ..policy.default_limits()
    };
    let client = client(policy, ScriptedResolver::always(LOOPBACK), Some(limits));

    let refusal = client
        .get(&format!("http://a.test:{port}/big"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .err();

    assert_eq!(refusal, Some(GuardError::BodyTooLarge { limit: 1024 }));
}

#[tokio::test]
async fn a_stalled_body_hits_the_idle_cap() {
    let (port, _) = serve().await;
    let limits = FetchLimits {
        max_body_bytes: 1024,
        connect_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(20),
    };
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        Some(limits),
    );

    let mut response = client
        .get(&format!("http://a.test:{port}/stall-body"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.chunk().await.unwrap().as_deref(),
        Some(&b"first"[..])
    );

    assert_eq!(response.chunk().await.err(), Some(GuardError::IdleTimeout));
}

#[tokio::test]
async fn a_stalled_response_head_hits_the_idle_cap() {
    let (port, _) = serve().await;
    let limits = FetchLimits {
        max_body_bytes: 1024,
        connect_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(20),
    };
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        Some(limits),
    );

    let refusal = client
        .get(&format!("http://a.test:{port}/stall-head"))
        .unwrap()
        .send()
        .await
        .err();

    assert_eq!(refusal, Some(GuardError::IdleTimeout));
}

#[tokio::test]
async fn a_body_that_keeps_trickling_hits_the_total_cap() {
    let (port, _) = serve().await;
    let limits = FetchLimits {
        max_body_bytes: 1024 * 1024,
        connect_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(5),
        total_timeout: Duration::from_millis(600),
    };
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        Some(limits),
    );

    let refusal = client
        .get(&format!("http://a.test:{port}/trickle"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .err();

    assert_eq!(refusal, Some(GuardError::TotalTimeout));
}

#[tokio::test]
async fn a_transport_failure_is_described_without_the_url() {
    let port = StdTcpListener::bind((LOOPBACK, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        None,
    );

    let refusal = client
        .get(&format!(
            "http://closed.test:{port}/botSECRET-TOKEN/hook?key=SECRET-KEY"
        ))
        .unwrap()
        .send()
        .await
        .err()
        .unwrap();

    let text = refusal.to_string();
    assert!(matches!(refusal, GuardError::Transport(_)), "{refusal:?}");
    assert!(
        !text.contains("SECRET") && !text.contains("closed.test"),
        "{text}"
    );
}

#[tokio::test]
async fn without_the_test_permission_loopback_is_denied() {
    let (port, requests) = serve().await;
    let client = GuardedClient::builder(FetchPolicy::webhook())
        .resolver(ScriptedResolver::always(LOOPBACK))
        .build()
        .unwrap();

    assert_eq!(
        client
            .get(&format!("http://a.test:{port}/ok"))
            .unwrap()
            .send()
            .await
            .err(),
        Some(GuardError::DeniedAddress)
    );
    assert_eq!(
        client
            .check_url(&format!("http://127.0.0.1:{port}/ok"))
            .err(),
        Some(GuardError::DeniedAddress)
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_redirect_to_the_metadata_address_is_refused() {
    // Followed under the allowlist policy, which lists the metadata address
    // but applies no deny list — so this pins that the hop is checked against
    // the allowlist the operator wrote, and that a deny-list policy refuses
    // the same literal before any connection.
    let (port, _) = serve().await;
    let client = client(
        listed(&["a.test"]),
        ScriptedResolver::always(LOOPBACK),
        None,
    );
    let refusal = client
        .get(&format!("http://a.test:{port}/to-metadata"))
        .unwrap()
        .send()
        .await
        .err();
    assert_eq!(refusal, Some(GuardError::HostNotAllowed));

    let webhook = GuardedClient::new(FetchPolicy::webhook()).unwrap();
    assert_eq!(
        webhook
            .check_url("http://169.254.169.254/latest/meta-data/")
            .err(),
        Some(GuardError::DeniedAddress)
    );
}

#[tokio::test]
async fn limits_of_duration_max_mean_no_cap_rather_than_a_panic() {
    let (port, _) = serve().await;
    let limits = FetchLimits {
        max_body_bytes: u64::MAX,
        connect_timeout: Duration::MAX,
        idle_timeout: Duration::MAX,
        total_timeout: Duration::MAX,
    };
    let client = client(
        FetchPolicy::webhook(),
        ScriptedResolver::always(LOOPBACK),
        Some(limits),
    );

    let body = client
        .get(&format!("http://a.test:{port}/ok"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    assert!(body.starts_with(b"ok host="));
}
