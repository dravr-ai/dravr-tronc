// ABOUTME: W3C trace context (traceparent, tracestate) read from HTTP headers and MCP `_meta`, written on outbound calls
// ABOUTME: Reading and validating needs no dependency; joining and propagating an OpenTelemetry trace needs `otel`
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! W3C Trace Context across every hop a dravr request takes.
//!
//! A trace crosses a dravr service three ways, and each one is read or
//! written here:
//!
//! - **An HTTP request.** [`guard_requests`](crate::server::request_guard::guard_requests)
//!   reads the `traceparent` and `tracestate` headers and makes its `request`
//!   span a child of the caller's span.
//! - **An MCP request.** The MCP trace-context convention carries the same two
//!   values in `params._meta`, so a trace survives transports that have no
//!   headers (stdio) and hops that drop them. [`TraceContext::from_meta`]
//!   reads them.
//! - **An outbound call.** `ServiceClient` (feature `service-client`) writes the current span's context on every attempt with
//!   [`inject_current_context`], so the called service's spans land in the
//!   caller's trace.
//!
//! Without the `otel` feature there is no trace to join: the values are still
//! read and validated, and nothing is exported or propagated.

use axum::http::HeaderMap;
#[cfg(feature = "otel")]
use axum::http::{HeaderName, HeaderValue};
#[cfg(feature = "otel")]
use opentelemetry::global;
#[cfg(feature = "otel")]
use opentelemetry::propagation::{Extractor, Injector};
#[cfg(feature = "otel")]
use opentelemetry::trace::TraceContextExt as _;
#[cfg(feature = "otel")]
use opentelemetry::Context;
use serde_json::Value;
#[cfg(feature = "otel")]
use tracing::Span;
#[cfg(feature = "otel")]
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

/// Header and `_meta` key of the W3C `traceparent` value.
pub const TRACEPARENT: &str = "traceparent";

/// Header and `_meta` key of the W3C `tracestate` value.
pub const TRACESTATE: &str = "tracestate";

/// Longest `tracestate` kept. W3C Trace Context lets a receiver drop a longer
/// one, and a value that size has no business in every span.
pub const MAX_TRACESTATE_LEN: usize = 512;

/// Length of a version-`00` `traceparent`: `00-<32 hex>-<16 hex>-<2 hex>`.
const TRACEPARENT_LEN: usize = 55;

/// The trace an inbound request belongs to: a validated `traceparent` and the
/// `tracestate` that came with it.
///
/// Built only from a `traceparent` that W3C Trace Context accepts, so a value a
/// caller controls is never copied into a span as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    traceparent: String,
    tracestate: Option<String>,
}

impl TraceContext {
    /// A trace context from its two values, or `None` when `traceparent` is
    /// not one W3C Trace Context accepts.
    ///
    /// A `tracestate` that is longer than [`MAX_TRACESTATE_LEN`] or holds a
    /// byte outside printable ASCII is dropped and the `traceparent` kept,
    /// which is what the specification asks of a receiver.
    #[must_use]
    pub fn parse(traceparent: &str, tracestate: Option<&str>) -> Option<Self> {
        let traceparent = traceparent.trim();
        if !is_valid_traceparent(traceparent) {
            return None;
        }
        let tracestate = tracestate
            .map(str::trim)
            .filter(|state| is_usable_tracestate(state))
            .map(str::to_owned);
        Some(Self {
            traceparent: traceparent.to_owned(),
            tracestate,
        })
    }

    /// The trace context an HTTP request's `traceparent` and `tracestate`
    /// headers carry.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let traceparent = headers.get(TRACEPARENT)?.to_str().ok()?;
        let tracestate = headers
            .get(TRACESTATE)
            .and_then(|value| value.to_str().ok());
        Self::parse(traceparent, tracestate)
    }

    /// The trace context an MCP request carries in `params._meta`.
    #[must_use]
    pub fn from_meta(params: Option<&Value>) -> Option<Self> {
        let meta = params?.get("_meta")?;
        let traceparent = meta.get(TRACEPARENT)?.as_str()?;
        let tracestate = meta.get(TRACESTATE).and_then(Value::as_str);
        Self::parse(traceparent, tracestate)
    }

    /// The validated `traceparent`.
    #[must_use]
    pub fn traceparent(&self) -> &str {
        &self.traceparent
    }

    /// The `tracestate` that came with it, when it was usable.
    #[must_use]
    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }

    /// The 32-hex-digit trace id.
    #[must_use]
    pub fn trace_id(&self) -> &str {
        self.traceparent.get(3..35).unwrap_or_default()
    }

    /// The `OpenTelemetry` context this trace context names, read through the
    /// global propagator [`tracing_init`](crate::server::tracing_init) installs.
    #[cfg(feature = "otel")]
    #[must_use]
    pub fn otel_context(&self) -> Context {
        global::get_text_map_propagator(|propagator| propagator.extract(self))
    }
}

#[cfg(feature = "otel")]
impl Extractor for TraceContext {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case(TRACEPARENT) {
            Some(self.traceparent())
        } else if key.eq_ignore_ascii_case(TRACESTATE) {
            self.tracestate()
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        if self.tracestate.is_some() {
            vec![TRACEPARENT, TRACESTATE]
        } else {
            vec![TRACEPARENT]
        }
    }
}

/// Whether `value` is a `traceparent` W3C Trace Context accepts.
///
/// `version-traceid-parentid-flags`, lowercase hex, neither id all zeros, and
/// never version `ff`. A version-`00` value is exactly 55 characters; a later
/// version may append fields after a `-`, which a version-`00` reader ignores.
#[must_use]
pub fn is_valid_traceparent(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < TRACEPARENT_LEN {
        return false;
    }
    let lower_hex = |range: &[u8]| {
        range
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    };
    let not_zero = |range: &[u8]| range.iter().any(|b| *b != b'0');
    let (version, trace_id, parent_id, flags) =
        (&bytes[0..2], &bytes[3..35], &bytes[36..52], &bytes[53..55]);
    let dashes = bytes[2] == b'-' && bytes[35] == b'-' && bytes[52] == b'-';
    let tail_ok = match version {
        b"00" => bytes.len() == TRACEPARENT_LEN,
        _ => bytes.len() == TRACEPARENT_LEN || bytes[TRACEPARENT_LEN] == b'-',
    };
    dashes
        && tail_ok
        && lower_hex(version)
        && version != b"ff"
        && lower_hex(trace_id)
        && not_zero(trace_id)
        && lower_hex(parent_id)
        && not_zero(parent_id)
        && lower_hex(flags)
}

/// Whether a `tracestate` is short enough and plain enough to carry along.
fn is_usable_tracestate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TRACESTATE_LEN
        && value.bytes().all(|b| (b' '..=b'~').contains(&b))
}

/// Make `span` a child of the remote span `context` names.
///
/// Returns whether it did: not when the context names no valid span (no
/// propagator is installed, so nothing is exported either), when no
/// `OpenTelemetry` layer is installed, or when `span` has already started.
/// Call it right after creating the span.
#[cfg(feature = "otel")]
pub fn join_trace(span: &Span, context: &TraceContext) -> bool {
    let parent = context.otel_context();
    if !parent.span().span_context().is_valid() {
        return false;
    }
    span.set_parent(parent).is_ok()
}

/// Write the current span's trace context into an outbound request's headers.
///
/// Through the global propagator, so a service that installed another format
/// propagates that one. Writes nothing when the current span is not part of an
/// exported trace, and leaves any `traceparent` the caller set in that case.
#[cfg(feature = "otel")]
pub fn inject_current_context(headers: &mut HeaderMap) {
    let context = Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut HeaderInjector(headers));
    });
}

/// An outbound request's headers as the propagator writes them.
#[cfg(feature = "otel")]
struct HeaderInjector<'a>(&'a mut HeaderMap);

#[cfg(feature = "otel")]
impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        // A propagator only writes header-safe values; one that does not is
        // dropped rather than sent malformed.
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn a_w3c_traceparent_is_accepted() {
        let context = TraceContext::parse(VALID, Some("congo=t61rcWkgMzE")).unwrap(); // Safe: test assertion
        assert_eq!(context.traceparent(), VALID);
        assert_eq!(context.tracestate(), Some("congo=t61rcWkgMzE"));
        assert_eq!(context.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");
    }

    #[test]
    fn a_malformed_traceparent_is_refused() {
        for refused in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00_4bf92f3577b34da6a3ce929d0e0e4736_00f067aa0ba902b7_01",
            "00-4bf92f3577b34da6a3ce929d0e0e473g-00f067aa0ba902b7-01",
        ] {
            assert!(TraceContext::parse(refused, None).is_none(), "{refused:?}");
        }
    }

    #[test]
    fn a_later_version_may_append_fields() {
        let later = "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-future";
        assert!(is_valid_traceparent(later));
    }

    #[test]
    fn an_unusable_tracestate_is_dropped_and_the_traceparent_kept() {
        let long = "k=".to_owned() + &"v".repeat(MAX_TRACESTATE_LEN);
        for dropped in [long.as_str(), "", "k=v\u{7f}", "k=\u{e9}"] {
            let context = TraceContext::parse(VALID, Some(dropped)).unwrap(); // Safe: test assertion
            assert_eq!(context.tracestate(), None, "{dropped:?}");
        }
    }

    #[test]
    fn the_meta_carries_the_same_two_values() {
        let params = json!({
            "name": "t",
            "_meta": { "traceparent": VALID, "tracestate": "a=b" }
        });
        let context = TraceContext::from_meta(Some(&params)).unwrap(); // Safe: test assertion
        assert_eq!(context.traceparent(), VALID);
        assert_eq!(context.tracestate(), Some("a=b"));

        assert!(TraceContext::from_meta(Some(&json!({"_meta": {}}))).is_none());
        assert!(TraceContext::from_meta(Some(&json!({"_meta": {"traceparent": 7}}))).is_none());
        assert!(TraceContext::from_meta(None).is_none());
    }

    #[test]
    fn the_headers_carry_them_too() {
        let mut headers = HeaderMap::new();
        headers.insert(TRACEPARENT, VALID.parse().unwrap()); // Safe: test assertion
        let context = TraceContext::from_headers(&headers).unwrap(); // Safe: test assertion
        assert_eq!(context.traceparent(), VALID);
        assert_eq!(context.tracestate(), None);
    }
}
