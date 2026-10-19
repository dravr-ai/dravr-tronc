// ABOUTME: reqwest-middleware layer putting every outbound request in the caller's trace
// ABOUTME: Opens an OpenTelemetry CLIENT span per request, injects its W3C traceparent, records the status
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use axum::http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result};
use tracing::field::Empty;
use tracing::{info_span, Instrument as _};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::server::trace_context::inject_context;

/// Host recorded for a request whose URL names none.
const UNKNOWN_HOST: &str = "unknown";

/// A `reqwest-middleware` layer that opens a CLIENT span per outbound request
/// and sends the span's trace context with it.
///
/// The span, `http.client.request`, follows the OpenTelemetry HTTP client
/// semantic conventions: `otel.kind = "client"`, `otel.name = "<METHOD>
/// <host>"`, `http.request.method`, `server.address`, then
/// `http.response.status_code` once a response arrives, or
/// `otel.status_code = "ERROR"` when none does. Only the method and the host
/// are recorded — never the URL, whose path or query can carry a signed
/// token or a credential that must not reach a log or a trace export.
///
/// The span's context is written as `traceparent`/`tracestate` through the
/// global propagator ([`inject_context`]), so the called service's spans land
/// under it. Without an exporting `OpenTelemetry` layer the context names no
/// valid span and nothing is written: the layer is then an ordinary tracing
/// span around the request.
///
/// ```ignore
/// let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
///     .with(TracePropagationMiddleware)
///     .build();
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct TracePropagationMiddleware;

#[async_trait]
impl Middleware for TracePropagationMiddleware {
    async fn handle(
        &self,
        mut request: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let method = request.method().as_str().to_owned();
        let host = request.url().host_str().unwrap_or(UNKNOWN_HOST).to_owned();

        let span = info_span!(
            "http.client.request",
            otel.kind = "client",
            otel.name = format!("{method} {host}"),
            otel.status_code = Empty,
            http.request.method = method,
            server.address = host,
            http.response.status_code = Empty,
        );
        inject_context(&span.context(), request.headers_mut());

        let response = next.run(request, extensions).instrument(span.clone()).await;
        match &response {
            Ok(response) => {
                // As an i64: tracing-opentelemetry exports a u64 field as a
                // string, and the convention's status code is an int.
                span.record(
                    "http.response.status_code",
                    i64::from(response.status().as_u16()),
                );
            }
            Err(_) => {
                span.record("otel.status_code", "ERROR");
            }
        }
        response
    }
}
