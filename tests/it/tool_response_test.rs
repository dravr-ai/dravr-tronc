// ABOUTME: ToolResponse::structured — the structured and text forms of one typed result
// ABOUTME: Pins the f32 precision of both forms and the refusal of a value that is not an object
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dravr_tronc::mcp::schema::{StructuredContentError, ToolResponse};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
struct Pace {
    km_per_hour: f32,
    laps: u32,
}

#[test]
fn a_structured_result_carries_the_same_json_in_both_forms() {
    let response = ToolResponse::structured(&Pace {
        km_per_hour: 12.8,
        laps: 4,
    })
    .expect("an object");

    assert!(!response.is_error);
    assert_eq!(
        response.structured_content,
        Some(json!({ "km_per_hour": 12.8, "laps": 4 })),
        "the f32 reaches the structured reader at its own precision"
    );
    assert_eq!(
        response.content[0].as_text(),
        Some(r#"{"km_per_hour":12.8,"laps":4}"#)
    );
}

#[test]
fn a_value_that_is_not_an_object_is_refused() {
    let refused = ToolResponse::structured(&[1, 2, 3]).expect_err("an array");
    assert!(matches!(
        refused,
        StructuredContentError::NotAnObject("an array")
    ));
    assert_eq!(
        refused.to_string(),
        "structured content must be a JSON object, but the result is an array"
    );
}

#[test]
fn the_wire_form_names_structured_content() {
    let response = ToolResponse::structured(&json!({ "ok": true })).expect("an object");
    let wire = serde_json::to_value(&response).expect("serializes");
    assert_eq!(wire["structuredContent"], json!({ "ok": true }));
    assert_eq!(wire["isError"], false);
}
