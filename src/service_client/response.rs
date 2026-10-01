// ABOUTME: What a call to another service came back with: the Exchange it ran under and the service's answer
// ABOUTME: classify sorts a response into the service's own answer, a shed, or a request that was not finished
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::error::Category;
use serde_json::Value;

use super::client::DEFAULT_SHED_RETRY_AFTER_SECS;
use super::error::{ServiceError, Unfinished};
use crate::error::ErrorResponse;
use crate::server::request_guard::{RequestId, HANDLER_PANIC, REQUEST_TIMEOUT};
use crate::server::shed::{RETRY_AFTER_SECS_FIELD, SHED_REASON_FIELD};

/// One attempt, as both sides logged it.
#[derive(Debug, Clone)]
pub struct Exchange {
    /// The service that was called.
    pub service: String,
    /// The caller's name for what it asked.
    pub operation: String,
    /// The id this attempt was sent under; the guard logs and echoes it.
    pub request_id: RequestId,
    /// From just before the send until the outcome was known. Excludes token
    /// minting.
    pub elapsed: Duration,
    /// This attempt re-sent a GET whose first connection closed before any
    /// response.
    pub resent: bool,
}

/// An answer the service's own handler produced, with any status.
#[derive(Debug)]
pub struct ServiceResponse {
    exchange: Exchange,
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl ServiceResponse {
    /// The response status. Not necessarily a success: a service's own
    /// refusals come back here.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The response headers.
    #[must_use]
    pub const fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The attempt that produced this response.
    #[must_use]
    pub const fn exchange(&self) -> &Exchange {
        &self.exchange
    }

    /// The body as it arrived.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// The body as JSON; `Value::Null` when it is not JSON.
    ///
    /// For reading a refusal, where the consumer branches on a marker and an
    /// empty or HTML body simply carries none.
    #[must_use]
    pub fn json_value(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }

    /// The body decoded as `T`.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Decode`], naming the request id, when the body is not
    /// a `T`. Its detail says what kind of mismatch and where, and never
    /// quotes the body: a response between services can carry a secret, and
    /// this error's text is what a consumer logs.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, ServiceError> {
        serde_json::from_slice(&self.body).map_err(|error| ServiceError::Decode {
            exchange: Box::new(self.exchange.clone()),
            status: self.status,
            detail: decode_detail(&error),
        })
    }
}

/// What kind of mismatch a decode failed on and where, with none of the body.
///
/// `serde_json`'s own message quotes the value it did not expect.
fn decode_detail(error: &serde_json::Error) -> String {
    let what = match error.classify() {
        Category::Data => "a value of another type, or a missing field,",
        Category::Syntax => "invalid JSON",
        Category::Eof => "JSON that ends early",
        Category::Io => "an unreadable body",
    };
    format!("{what} at line {} column {}", error.line(), error.column())
}

/// Sort a response into the service's own answer, a shed, or a request that
/// was not finished.
///
/// A shed is checked first; the two status sets do not overlap.
pub(super) fn classify(
    exchange: Exchange,
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
) -> Result<ServiceResponse, ServiceError> {
    if !status.is_success() {
        let json = serde_json::from_slice::<Value>(&body).ok();
        let named_wait = json
            .as_ref()
            .and_then(|json| json.get(RETRY_AFTER_SECS_FIELD)?.as_u64());

        if status == StatusCode::SERVICE_UNAVAILABLE || named_wait.is_some() {
            let retry_after_secs = named_wait
                .or_else(|| header_wait(&headers))
                .unwrap_or(DEFAULT_SHED_RETRY_AFTER_SECS);
            let reason = json
                .as_ref()
                .and_then(|json| json.get(SHED_REASON_FIELD)?.as_str())
                .map(str::to_owned);
            return Err(ServiceError::Shed {
                exchange: Box::new(exchange),
                status,
                retry_after_secs,
                reason,
            });
        }

        let guard = json.and_then(|json| serde_json::from_value::<ErrorResponse>(json).ok());
        if let Some(kind) = unfinished_kind(status, guard.as_ref()) {
            return Err(ServiceError::Unfinished {
                exchange: Box::new(exchange),
                status,
                kind,
                detail: guard.map(|guard| guard.error.message),
            });
        }
    }

    Ok(ServiceResponse {
        exchange,
        status,
        headers,
        body,
    })
}

/// The `Retry-After` header as whole seconds. Its other form, an HTTP date, is
/// not a wait this client can state without a clock it trusts.
fn header_wait(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Whether a status, with the guard's answer when the body was one, says the
/// request started and was not finished.
fn unfinished_kind(status: StatusCode, guard: Option<&ErrorResponse>) -> Option<Unfinished> {
    let guard_type = guard.map(|guard| guard.error.error_type.as_str());
    match status {
        StatusCode::GATEWAY_TIMEOUT if guard_type == Some(REQUEST_TIMEOUT) => {
            Some(Unfinished::ServiceDeadline)
        }
        StatusCode::GATEWAY_TIMEOUT => Some(Unfinished::GatewayDeadline),
        StatusCode::BAD_GATEWAY => Some(Unfinished::BadGateway),
        StatusCode::INTERNAL_SERVER_ERROR if guard_type == Some(HANDLER_PANIC) => {
            Some(Unfinished::HandlerPanic)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn exchange() -> Exchange {
        Exchange {
            service: "svc".to_owned(),
            operation: "list".to_owned(),
            request_id: RequestId::mint(),
            elapsed: Duration::from_millis(3),
            resent: false,
        }
    }

    fn sorted(status: u16, body: &Value) -> Result<ServiceResponse, ServiceError> {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("serialize") // Safe: test assertion
        };
        classify(
            exchange(),
            StatusCode::from_u16(status).expect("status"), // Safe: test assertion
            HeaderMap::new(),
            bytes,
        )
    }

    /// The kind, detail and message of a response sorted as unfinished, or
    /// `None` when it was sorted as anything else.
    fn unfinished(status: u16, body: &Value) -> Option<(Unfinished, Option<String>, String)> {
        let error = sorted(status, body).err()?;
        let text = error.to_string();
        match error {
            ServiceError::Unfinished { kind, detail, .. } => Some((kind, detail, text)),
            ServiceError::Identity { .. }
            | ServiceError::Transport { .. }
            | ServiceError::Shed { .. }
            | ServiceError::Body { .. }
            | ServiceError::Decode { .. } => None,
        }
    }

    #[test]
    fn classify_names_only_unfinished_requests() {
        let (kind, detail, text) = unfinished(
            504,
            &json!({"error": {"type": "request_timeout",
                              "message": "The request did not complete within 320s."}}),
        )
        .expect("the guard's 504 is unfinished"); // Safe: test assertion
        assert_eq!(kind, Unfinished::ServiceDeadline);
        assert_eq!(
            detail.as_deref(),
            Some("The request did not complete within 320s.")
        );
        assert!(text.contains("service's request deadline"), "{text}");
        assert!(text.contains("320s"), "{text}");

        let (kind, detail, text) = unfinished(504, &Value::Null).expect("a bare 504 is unfinished"); // Safe: test assertion
        assert_eq!(kind, Unfinished::GatewayDeadline);
        assert_eq!(detail, None);
        assert!(text.contains("gateway's deadline"), "{text}");

        let (kind, detail, text) = unfinished(502, &Value::Null).expect("a 502 is unfinished"); // Safe: test assertion
        assert_eq!(kind, Unfinished::BadGateway);
        assert_eq!(detail, None);
        assert!(text.contains("no usable answer"), "{text}");

        let (kind, detail, text) = unfinished(
            500,
            &json!({"error": {"type": "handler_panic", "message": "logged under x-1"}}),
        )
        .expect("the guard's panic 500 is unfinished"); // Safe: test assertion
        assert_eq!(kind, Unfinished::HandlerPanic);
        assert_eq!(detail.as_deref(), Some("logged under x-1"));
        assert!(text.contains("panicked"), "{text}");

        // The service's own answers: a fault it reported, a guard-shaped body
        // of another type, a refusal. None is this crate's to name.
        for (status, body) in [
            (500, json!({"error": "browser error: failed to launch"})),
            (
                500,
                json!({"error": {"type": "internal_error", "message": "x"}}),
            ),
            (401, json!({"error": "session_expired"})),
        ] {
            let response = sorted(status, &body).expect("the service's own answer"); // Safe: test assertion
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.json_value(), body);
        }

        // A success is the service's answer whatever its body names.
        let response = sorted(200, &json!({"retry_after_secs": 5})).expect("a success"); // Safe: test assertion
        assert_eq!(response.json_value()["retry_after_secs"], 5);
    }
}
