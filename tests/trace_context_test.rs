// ABOUTME: Tests W3C trace propagation end to end: the request guard joins a caller's trace, ServiceClient forwards it
// ABOUTME: Spans are exported to memory through the same tracing-opentelemetry layer tracing_init installs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "otel")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn;
use axum::routing::get;
use axum::Router;
use dravr_tronc::server::request_guard::guard_requests;
use dravr_tronc::server::trace_context::TRACEPARENT;
use http_body_util::BodyExt;
use opentelemetry::global;
use opentelemetry::trace::{SpanId, TraceContextExt as _, TraceId, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::ServiceExt;
use tracing::subscriber::{set_default, DefaultGuard};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt;

const CALLER_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const CALLER_SPAN: &str = "00f067aa0ba902b7";

fn caller_traceparent() -> String {
    format!("00-{CALLER_TRACE}-{CALLER_SPAN}-01")
}

/// Route this thread's spans into memory through a tracing-opentelemetry
/// layer, with the W3C propagator `tracing_init` installs.
fn export_spans() -> (InMemorySpanExporter, SdkTracerProvider, DefaultGuard) {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    (exporter, provider, set_default(subscriber))
}

/// The trace id of the span the handler runs in.
async fn current_trace_id() -> String {
    Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
        .to_string()
}

#[tokio::test]
async fn the_request_span_continues_the_callers_trace() {
    let (exporter, _provider, _subscriber) = export_spans();
    let app = Router::new()
        .route("/api/plan", get(current_trace_id))
        .layer(from_fn(guard_requests));

    let response = app
        .oneshot(
            Request::get("/api/plan")
                .header(TRACEPARENT, caller_traceparent())
                .header("tracestate", "dravr=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], CALLER_TRACE.as_bytes(), "the handler runs in it");

    let spans = exporter.get_finished_spans().unwrap();
    let request = spans
        .iter()
        .find(|span| span.name == "request")
        .unwrap_or_else(|| panic!("the request span is exported: {spans:?}"));
    assert_eq!(
        request.span_context.trace_id(),
        TraceId::from_hex(CALLER_TRACE).unwrap()
    );
    assert_eq!(
        request.parent_span_id,
        SpanId::from_hex(CALLER_SPAN).unwrap()
    );
    assert!(request.parent_span_is_remote);
}

#[tokio::test]
async fn a_request_without_a_usable_traceparent_starts_its_own_trace() {
    let (_exporter, _provider, _subscriber) = export_spans();
    let app = Router::new()
        .route("/api/plan", get(current_trace_id))
        .layer(from_fn(guard_requests));

    for header in [None, Some("00-not-a-trace-01")] {
        let mut request = Request::get("/api/plan");
        if let Some(value) = header {
            request = request.header(TRACEPARENT, value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let trace_id = String::from_utf8(body.to_vec()).unwrap();
        assert_ne!(trace_id, CALLER_TRACE, "{header:?}");
        assert_ne!(trace_id, TraceId::INVALID.to_string(), "{header:?}");
    }
}

#[cfg(feature = "service-client")]
mod service_client {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dravr_tronc::server::trace_context::{join_trace, TraceContext};
    use dravr_tronc::service_client::ServiceClient;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tracing::{info_span, Instrument};

    use super::*;

    /// A loopback service answering `200 {}`, recording each request head.
    async fn spawn_recording() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).to_ascii_lowercase());
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                    )
                    .await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn traceparent_of(head: &str) -> Option<String> {
        head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name == TRACEPARENT).then(|| value.trim().to_owned())
        })
    }

    #[tokio::test]
    async fn an_outbound_call_carries_the_current_trace() {
        let (_exporter, _provider, _subscriber) = export_spans();
        let (base_url, seen) = spawn_recording().await;
        let client = ServiceClient::new("svc", &base_url, None, reqwest::Client::new()).unwrap();

        let span = info_span!("handler");
        let caller = TraceContext::parse(&caller_traceparent(), None).unwrap();
        assert!(join_trace(&span, &caller));
        let own_span_id = span.context().span().span_context().span_id();
        client
            .get("list", "/api/items")
            .timeout(Duration::from_secs(5))
            .send()
            .instrument(span)
            .await
            .unwrap();

        let heads = seen.lock().unwrap().clone();
        let sent = traceparent_of(&heads[0])
            .unwrap_or_else(|| panic!("the call carries a traceparent: {}", heads[0]));
        let sent = TraceContext::parse(&sent, None).unwrap();
        assert_eq!(sent.trace_id(), CALLER_TRACE, "the same trace");
        assert!(
            sent.traceparent().contains(&own_span_id.to_string()),
            "parented on the calling span, not on the caller's caller: {}",
            sent.traceparent()
        );
    }

    #[tokio::test]
    async fn a_call_outside_any_trace_carries_none() {
        let (_exporter, _provider, _subscriber) = export_spans();
        let (base_url, seen) = spawn_recording().await;
        let client = ServiceClient::new("svc", &base_url, None, reqwest::Client::new()).unwrap();

        client.get("list", "/api/items").send().await.unwrap();

        let heads = seen.lock().unwrap().clone();
        assert_eq!(traceparent_of(&heads[0]), None, "{}", heads[0]);
    }
}
