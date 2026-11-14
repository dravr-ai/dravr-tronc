// ABOUTME: ToolResponse constructors and builders — structured content and the result's _meta
// ABOUTME: Pins f32 precision, the non-object refusal, and how _meta is written, merged and read back
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

/// The key a dravr host reads its trust labels under, as a host would name one.
const LABELS_KEY: &str = "ai.dravr.vault/labels";

#[test]
fn with_meta_writes_the_value_under_its_key_in_meta() {
    let labels = json!({ "v": 1, "tier": "T1", "untrusted": true });
    let response = ToolResponse::text("ok".to_owned()).with_meta(LABELS_KEY, labels.clone());

    let wire = serde_json::to_value(&response).expect("serializes");
    assert_eq!(
        wire["_meta"],
        json!({ LABELS_KEY: labels }),
        "the value sits under its key in the result's _meta"
    );
    assert!(
        wire.get("meta").is_none(),
        "the field is renamed to _meta on the wire, never written as meta"
    );
}

#[test]
fn a_result_without_meta_writes_no_meta_key() {
    let wire = serde_json::to_value(ToolResponse::text("ok".to_owned())).expect("serializes");
    assert!(
        wire.get("_meta").is_none(),
        "a result nobody labelled carries no _meta key at all, not an empty one"
    );
    let error = serde_json::to_value(ToolResponse::error("boom".to_owned())).expect("serializes");
    assert!(
        error.get("_meta").is_none(),
        "an error result carries none either"
    );
}

#[test]
fn a_second_key_is_kept_beside_the_first_and_a_repeated_key_is_replaced() {
    let response = ToolResponse::text("ok".to_owned())
        .with_meta(LABELS_KEY, json!("first"))
        .with_meta("com.example/other", json!(2))
        .with_meta(LABELS_KEY, json!("second"));

    let meta = response.meta.expect("meta was written");
    assert_eq!(meta.len(), 2, "two distinct keys, each once");
    assert_eq!(meta[LABELS_KEY], json!("second"), "the later value wins");
    assert_eq!(
        meta["com.example/other"],
        json!(2),
        "the other key survives"
    );
}

#[test]
fn a_wire_result_without_meta_reads_back_as_none() {
    let response: ToolResponse = serde_json::from_value(json!({
        "content": [{ "type": "text", "text": "ok" }],
        "isError": false
    }))
    .expect("a result with no _meta deserializes");
    assert!(
        response.meta.is_none(),
        "no _meta on the wire reads as None"
    );
    assert!(response.structured_content.is_none());
}

#[test]
fn meta_survives_a_round_trip_through_its_wire_form() {
    let labels = json!({ "v": 1, "tier": "T0", "source_class": ["device"] });
    let sent = ToolResponse::text("ok".to_owned()).with_meta(LABELS_KEY, labels.clone());

    let wire = serde_json::to_value(&sent).expect("serializes");
    let read: ToolResponse = serde_json::from_value(wire).expect("deserializes");

    assert_eq!(
        read.meta.expect("_meta read back")[LABELS_KEY],
        labels,
        "a host that re-reads the result keeps the labels the tool attached"
    );
    assert_eq!(read.content[0].as_text(), Some("ok"));
}

#[test]
fn a_structured_result_with_meta_carries_both() {
    let response = ToolResponse::structured(&json!({ "laps": 4 }))
        .expect("an object")
        .with_meta(LABELS_KEY, json!({ "tier": "T0" }));

    let wire = serde_json::to_value(&response).expect("serializes");
    assert_eq!(wire["structuredContent"], json!({ "laps": 4 }));
    assert_eq!(wire["content"][0]["text"], r#"{"laps":4}"#);
    assert_eq!(wire["_meta"][LABELS_KEY], json!({ "tier": "T0" }));
}

#[test]
fn with_structured_content_keeps_the_blocks_and_the_error_flag() {
    let response = ToolResponse::error("refused".to_owned())
        .with_structured_content(json!({ "reason": "quota" }));

    assert!(
        response.is_error,
        "the builder never flips an error to success"
    );
    assert_eq!(response.content[0].as_text(), Some("refused"));
    assert_eq!(
        response.structured_content,
        Some(json!({ "reason": "quota" }))
    );
}
