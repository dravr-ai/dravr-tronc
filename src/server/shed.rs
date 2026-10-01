// ABOUTME: The answer of a service that will not start a request because it is saturated
// ABOUTME: shed_response builds the 503 + Retry-After + retry_after_secs body ServiceClient reads as a shed
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The shed response: how a saturated service tells a caller to come back.
//!
//! [`request_guard`](super::request_guard) owns "could not finish": a handler
//! that panicked or outlived its deadline. A shed is the other refusal, "would
//! not start": the service is at capacity, the request did no work, and the
//! caller is told how long to wait before sending it again. The two are kept
//! apart because a caller treats them differently — a shed is always safe to
//! retry, whatever the method, since nothing ran.
//!
//! The wire shape is a `503`, a `Retry-After` header, and a JSON body naming
//! the same wait:
//!
//! ```json
//! {"error": "<the service's marker>", "reason": "<why>", "retry_after_secs": 12}
//! ```
//!
//! `error` is a flat string here on purpose, unlike the guard's nested
//! `{"error": {"type": …, "message": …}}`: it is the service's own marker, in
//! the service's own vocabulary, and this crate defines none and reads none.
//! What a caller keys on is the status and [`RETRY_AFTER_SECS_FIELD`], which is
//! what `service_client::ServiceClient` decodes.
//!
//! ```rust,ignore
//! use dravr_tronc::server::shed::shed_response;
//!
//! async fn list(State(queue): State<Queue>) -> Response {
//!     let Some(permit) = queue.try_acquire() else {
//!         return shed_response("widgets_busy", "queue full", 12);
//!     };
//!     // …
//! }
//! ```

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Top-level body field naming how long a shed caller should wait, in seconds.
pub const RETRY_AFTER_SECS_FIELD: &str = "retry_after_secs";

/// Top-level body field saying why the service shed the request.
pub const SHED_REASON_FIELD: &str = "reason";

/// The answer of a service that will not start a request because it is saturated.
///
/// `503`, a `Retry-After: <secs>` header, and
/// `{"error": <marker>, "reason": <reason>, "retry_after_secs": <secs>}`.
/// `marker` is the service's own busy marker; tronc defines none and reads none.
///
/// The wait is written as given, in both places: the service knows its own
/// queue, and logs its own reason.
#[must_use]
pub fn shed_response(marker: &str, reason: &str, retry_after_secs: u64) -> Response {
    let body = json!({
        "error": marker,
        SHED_REASON_FIELD: reason,
        RETRY_AFTER_SECS_FIELD: retry_after_secs,
    });
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
    response
}
