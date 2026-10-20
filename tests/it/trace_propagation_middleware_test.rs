// ABOUTME: Tests TracePropagationMiddleware: a CLIENT span per outbound request, its traceparent sent, its status recorded
// ABOUTME: Spans are exported to memory through the tracing-opentelemetry layer tracing_init installs
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
use std::sync::{Arc, Mutex};

use dravr_tronc::http_client::TracePropagationMiddleware;
use dravr_tronc::server::trace_context::{TraceContext, TRACEPARENT};
use opentelemetry::global;
use opentelemetry::trace::{SpanKind, Status, TracerProvider as _};
use opentelemetry::Value;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use serial_test::serial;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::subscriber::{set_default, DefaultGuard};
use tracing::{info_span, Instrument as _};
use tracing_subscriber::layer::SubscriberExt;

/// Route this thread's spans into memory, with the W3C propagator
/// `tracing_init` installs.
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

/// A loopback service answering `status`, recording each request head.
async fn spawn_recording(status: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
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
            let reply =
                format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}");
            let _ = socket.write_all(reply.as_bytes()).await;
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

fn traced_client() -> ClientWithMiddleware {
    ClientBuilder::new(reqwest::Client::new())
        .with(TracePropagationMiddleware)
        .build()
}

fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

fn client_span(exporter: &InMemorySpanExporter) -> SpanData {
    let spans = exporter.get_finished_spans().unwrap();
    spans
        .iter()
        .find(|span| span.span_kind == SpanKind::Client)
        .cloned()
        .unwrap_or_else(|| panic!("a CLIENT span is exported: {spans:?}"))
}

#[tokio::test]
#[serial]
async fn each_request_opens_a_client_span_and_sends_its_traceparent() {
    let (exporter, _provider, _subscriber) = export_spans();
    let (base_url, seen) = spawn_recording("200 OK").await;

    let handler = info_span!("handler");
    traced_client()
        .get(format!("{base_url}/api/items?token=SECRET"))
        .send()
        .instrument(handler)
        .await
        .unwrap();

    let span = client_span(&exporter);
    assert_eq!(span.name, "GET 127.0.0.1");
    assert_eq!(
        attribute(&span, "http.request.method"),
        Some(&Value::from("GET"))
    );
    assert_eq!(
        attribute(&span, "server.address"),
        Some(&Value::from("127.0.0.1"))
    );
    assert_eq!(
        attribute(&span, "http.response.status_code"),
        Some(&Value::I64(200))
    );
    assert!(
        span.attributes
            .iter()
            .all(|kv| !kv.value.as_str().contains("SECRET")),
        "no attribute carries the URL: {:?}",
        span.attributes
    );

    let parent = exporter
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .find(|span| span.name == "handler")
        .unwrap();
    assert_eq!(
        span.parent_span_id,
        parent.span_context.span_id(),
        "a child of the calling span"
    );

    let heads = seen.lock().unwrap().clone();
    let sent = traceparent_of(&heads[0])
        .unwrap_or_else(|| panic!("the request carries a traceparent: {}", heads[0]));
    let sent = TraceContext::parse(&sent, None).unwrap();
    assert_eq!(
        sent.trace_id(),
        span.span_context.trace_id().to_string(),
        "the same trace"
    );
    assert!(
        sent.traceparent()
            .contains(&span.span_context.span_id().to_string()),
        "parented on the client span itself: {}",
        sent.traceparent()
    );
}

#[tokio::test]
#[serial]
async fn an_error_status_is_recorded_and_a_failed_send_marks_the_span() {
    let (exporter, _provider, _subscriber) = export_spans();
    let (base_url, _seen) = spawn_recording("503 Service Unavailable").await;
    let response = traced_client().get(&base_url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(
        attribute(&client_span(&exporter), "http.response.status_code"),
        Some(&Value::I64(503))
    );

    let (exporter, _provider, _subscriber) = export_spans();
    let port = StdTcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    traced_client()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .expect_err("nothing listens on the port");
    let span = client_span(&exporter);
    assert!(
        matches!(span.status, Status::Error { .. }),
        "a send that got no response is an error: {:?}",
        span.status
    );
    assert_eq!(attribute(&span, "http.response.status_code"), None);
}

#[tokio::test]
#[serial]
async fn without_an_exporting_layer_nothing_is_injected() {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let (base_url, seen) = spawn_recording("200 OK").await;
    traced_client().get(&base_url).send().await.unwrap();
    let heads = seen.lock().unwrap().clone();
    assert_eq!(traceparent_of(&heads[0]), None, "{}", heads[0]);
}
