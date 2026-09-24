// ABOUTME: Request guard for dravr HTTP servers: request ids, panic containment, deadlines, completion logs
// ABOUTME: A handler that panics or outlives its deadline answers a JSON 500/504, never a dropped connection
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Two `axum::middleware::from_fn` middlewares a dravr HTTP server layers over
//! its router.
//!
//! - [`guard_requests`] goes **outermost**, over the whole router, once. It
//!   gives every request an id — the caller's [`REQUEST_ID_HEADER`] when it
//!   sends a usable one, a minted one otherwise — and echoes it on the
//!   response. It logs one INFO line per request when the response is ready
//!   (method, route template, status, latency, id), and it turns a handler
//!   panic into a `500` whose JSON body carries [`HANDLER_PANIC`], logging the
//!   panic payload at ERROR under the request id. A request whose future is
//!   dropped before it answered is logged at WARN with how long it ran: hyper
//!   drops a handler when the client closes the connection mid-request, which
//!   the client reports as "connection closed before message completed", and
//!   without this line the server keeps no trace of it at all.
//! - [`enforce_deadline`] goes on the routes that answer with **one**
//!   response. A handler still running at the deadline is dropped and the
//!   caller gets a `504` whose JSON body carries [`REQUEST_TIMEOUT`].
//!
//! Both bodies are the crate's [`ErrorResponse`] shape,
//! `{"error":{"type":…,"message":…}}`, so a caller classifies a failed request
//! by `error.type` instead of by a connection that simply went away.
//!
//! ```rust,ignore
//! use std::time::Duration;
//!
//! use axum::middleware::from_fn;
//! use dravr_tronc::mcp::transport::http::mcp_router;
//! use dravr_tronc::server::request_guard::{enforce_deadline, guard_requests};
//!
//! let deadline = Duration::from_secs(300);
//! let app = rest_routes
//!     .layer(from_fn(move |req, next| enforce_deadline(deadline, req, next)))
//!     .merge(mcp_router(server))
//!     .layer(from_fn(guard_requests));
//! ```
//!
//! # Where a deadline does not belong
//!
//! Not on `/mcp`: the MCP transport dispatches a whole tool call before it
//! answers, so a request deadline there cuts a legitimate long tool call. Not
//! on a WebSocket or a streamed body either — `enforce_deadline` bounds the
//! time to a response, and a stream's lifetime is not a request's. Layer it on
//! the router that holds the one-shot REST routes and merge the rest after.
//!
//! # A contained panic needs `panic = "unwind"`
//!
//! [`guard_requests`] contains a panic by catching the unwind. A binary built
//! with `panic = "abort"` terminates the whole process on the first panic
//! instead, dropping every in-flight request with it, and no middleware can
//! answer anything there. A server that layers the guard builds with unwinding.

use std::any::Any;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::FutureExt;
use tokio::time;
use tracing::{error, info, info_span, warn, Instrument};

use crate::error::ErrorResponse;

/// Header carrying the id a request is logged under, in both directions.
///
/// A caller that sends one gets its own id logged and echoed back, so one id
/// finds the request in the caller's logs and in the server's.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// `error.type` of the `500` [`guard_requests`] answers when a handler panics.
pub const HANDLER_PANIC: &str = "handler_panic";

/// `error.type` of the `504` [`enforce_deadline`] answers when a handler is
/// still running at its deadline.
pub const REQUEST_TIMEOUT: &str = "request_timeout";

/// Longest caller-supplied request id the guard adopts.
///
/// Long enough for a UUID, a W3C trace id, or a prefixed composite of them; a
/// longer value is replaced rather than copied into every log line.
pub const MAX_REQUEST_ID_LEN: usize = 128;

/// Route logged for a request no route matched (the router's fallback).
///
/// The concrete path is never logged: it and its query string carry athlete
/// and session ids.
const UNMATCHED_ROUTE: &str = "<unmatched>";

/// The id one request is logged under, shared with the caller through
/// [`REQUEST_ID_HEADER`].
///
/// [`guard_requests`] puts it in the request's extensions, so an inner
/// middleware or a handler reads it with `Extension<RequestId>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    /// The caller's id when it sent a usable one, a minted one otherwise.
    ///
    /// Usable means 1 to [`MAX_REQUEST_ID_LEN`] characters of ASCII letters,
    /// digits, `-`, `_`, `.` or `:`. Anything else is replaced, never copied:
    /// the id is written into every log line for the request, and a value the
    /// caller controls must not be able to forge or break one.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|id| is_usable_request_id(id))
            .map_or_else(Self::mint, |id| Self(id.to_owned()))
    }

    /// A fresh id: this process's random instance prefix and a sequence number.
    ///
    /// Unique within the process by the sequence, and across processes by the
    /// 64-bit prefix, which comes from the operating system's randomness
    /// through [`RandomState`]. Every id a process mints shares the prefix, so
    /// it also names which instance served a request.
    #[must_use]
    pub fn mint() -> Self {
        static INSTANCE: OnceLock<u64> = OnceLock::new();
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let instance = *INSTANCE.get_or_init(|| RandomState::new().build_hasher().finish());
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(format!("{instance:016x}-{sequence:x}"))
    }

    /// The id as written on the wire and in the logs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a caller-supplied id may be adopted as-is.
fn is_usable_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// The route template a request matched (`/api/activities/{id}`), or
/// [`UNMATCHED_ROUTE`].
///
/// Read from axum's [`MatchedPath`], which the router sets before a
/// `Router::layer` middleware runs. Never the concrete path or its query.
fn route_template(request: &Request) -> String {
    request.extensions().get::<MatchedPath>().map_or_else(
        || UNMATCHED_ROUTE.to_owned(),
        |path| path.as_str().to_owned(),
    )
}

/// Milliseconds since `started`, saturating.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A failure answer in the crate's [`ErrorResponse`] shape.
fn failure_response(status: StatusCode, error_type: &str, message: String) -> Response {
    (status, Json(ErrorResponse::new(error_type, message))).into_response()
}

/// The text of a panic payload: `panic!` with a literal carries a `&str`, with
/// a format string a `String`; anything else is named, not guessed at.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic payload is not a string")
}

/// One request between its arrival and its response.
///
/// Dropped without [`finish`](Self::finish) means no response was produced:
/// hyper drops a handler's future when the client closes the connection
/// mid-request, and a server going down drops every one in flight. That drop
/// is the only trace such a request leaves, so it is logged.
struct InFlight {
    request_id: RequestId,
    method: Method,
    path: String,
    started: Instant,
    finished: bool,
}

impl InFlight {
    fn start(request_id: RequestId, method: Method, path: String) -> Self {
        Self {
            request_id,
            method,
            path,
            started: Instant::now(),
            finished: false,
        }
    }

    /// Log the completed request at INFO.
    fn finish(mut self, status: StatusCode) {
        self.finished = true;
        info!(
            request_id = %self.request_id,
            method = %self.method,
            path = %self.path,
            status = status.as_u16(),
            latency_ms = elapsed_ms(self.started),
            "request completed"
        );
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.finished {
            warn!(
                request_id = %self.request_id,
                method = %self.method,
                path = %self.path,
                elapsed_ms = elapsed_ms(self.started),
                "request dropped before a response: the connection closed or the server \
                 stopped while the handler was running"
            );
        }
    }
}

/// Outermost middleware: request id, panic containment, completion log.
///
/// Layer it once, last, over the whole router —
/// `router.layer(axum::middleware::from_fn(guard_requests))` — so it wraps
/// every route, every other middleware, and the fallback. See the
/// [module docs](self) for what it logs and why a contained panic needs
/// `panic = "unwind"`.
pub async fn guard_requests(mut request: Request, next: Next) -> Response {
    let request_id = RequestId::from_headers(request.headers());
    request.extensions_mut().insert(request_id.clone());
    let in_flight = InFlight::start(
        request_id.clone(),
        request.method().clone(),
        route_template(&request),
    );

    // Every event the handler logs carries the id through this span.
    let span = info_span!("request", request_id = %request_id);
    let outcome = AssertUnwindSafe(next.run(request))
        .catch_unwind()
        .instrument(span)
        .await;

    let mut response = match outcome {
        Ok(response) => response,
        Err(payload) => {
            error!(
                request_id = %request_id,
                method = %in_flight.method,
                path = %in_flight.path,
                panic = panic_message(payload.as_ref()),
                "request handler panicked; answering 500"
            );
            failure_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                HANDLER_PANIC,
                format!(
                    "The request handler failed. The service logged the failure under \
                     request id {request_id}."
                ),
            )
        }
    };
    in_flight.finish(response.status());

    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Per-router deadline: a handler still running at `deadline` is dropped and
/// the caller gets a `504` with a [`REQUEST_TIMEOUT`] body.
///
/// Layer it on the router holding one-shot routes, before merging anything
/// that must not be cut:
/// `rest.layer(from_fn(move |req, next| enforce_deadline(deadline, req, next)))`.
/// Size `deadline` above the slowest legitimate request the routes serve and
/// below the caller's own client timeout, so the caller receives this answer
/// rather than giving up on the connection first. See the
/// [module docs](self) for the routes it does not belong on.
pub async fn enforce_deadline(deadline: Duration, request: Request, next: Next) -> Response {
    let request_id = request.extensions().get::<RequestId>().cloned();
    let method = request.method().clone();
    let path = route_template(&request);

    if let Ok(response) = time::timeout(deadline, next.run(request)).await {
        return response;
    }

    warn!(
        request_id = request_id.as_ref().map_or("unassigned", RequestId::as_str),
        method = %method,
        path = %path,
        deadline_ms = u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        "request outlived its deadline; answering 504"
    );
    failure_response(
        StatusCode::GATEWAY_TIMEOUT,
        REQUEST_TIMEOUT,
        format!("The request did not complete within {deadline:?}."),
    )
}
