// ABOUTME: Tests the shed response a saturated service answers with
// ABOUTME: Pins the 503, the Retry-After header and the body fields ServiceClient decodes
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

// Same allowances tests/integration_test.rs carries: an integration test is not
// covered by the lib's `cfg_attr(test, ...)`, and assertions are what these are.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use axum::http::{header, StatusCode};
use dravr_tronc::server::shed::{shed_response, RETRY_AFTER_SECS_FIELD, SHED_REASON_FIELD};
use http_body_util::BodyExt;
use serde_json::{json, Value};

#[tokio::test]
async fn a_shed_response_carries_its_wait_twice() {
    let response = shed_response("busy", "queue full", 12);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("12"),
        "the wait must be in the standard header, for a caller that reads no body"
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body,
        json!({"error": "busy", "reason": "queue full", "retry_after_secs": 12})
    );
    // The constants are the field names on the wire.
    assert_eq!(body[RETRY_AFTER_SECS_FIELD], 12);
    assert_eq!(body[SHED_REASON_FIELD], "queue full");
}
