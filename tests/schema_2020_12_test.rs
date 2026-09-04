// ABOUTME: Pins the JSON Schema 2020-12 vocabulary on tool input/output schemas
// ABOUTME: Guards the wire names and the "empty type is omitted" rule for $ref/composition
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::collections::BTreeMap;

use dravr_tronc::mcp::schema::{JsonSchema, PropertySchema, ToolSchema};
use serde_json::json;

#[test]
fn a_simple_object_schema_is_unchanged_on_the_wire() {
    // The 2020-12 vocabulary is additive: a plain object schema must serialize
    // exactly as it did before, with no empty keys leaking onto the wire.
    let mut properties = BTreeMap::new();
    properties.insert(
        "name".to_owned(),
        PropertySchema {
            property_type: "string".to_owned(),
            description: Some("Person to greet".to_owned()),
            ..Default::default()
        },
    );
    let schema = JsonSchema {
        schema_type: "object".to_owned(),
        properties: Some(properties),
        required: Some(vec!["name".to_owned()]),
        ..Default::default()
    };

    let value = serde_json::to_value(&schema).expect("serializes");
    assert_eq!(
        value,
        json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "Person to greet" } },
            "required": ["name"]
        }),
        "additive fields must not appear when unset"
    );
}

#[test]
fn validation_keywords_use_their_json_schema_names() {
    let property = PropertySchema {
        property_type: "string".to_owned(),
        enum_values: Some(vec![json!("run"), json!("ride")]),
        format: Some("date-time".to_owned()),
        pattern: Some("^[a-z]+$".to_owned()),
        min_length: Some(1),
        max_length: Some(64),
        default_value: Some(json!("run")),
        ..Default::default()
    };

    let value = serde_json::to_value(&property).expect("serializes");
    assert_eq!(value["enum"], json!(["run", "ride"]));
    assert_eq!(value["format"], "date-time");
    assert_eq!(value["pattern"], "^[a-z]+$");
    assert_eq!(value["minLength"], 1);
    assert_eq!(value["maxLength"], 64);
    assert_eq!(value["default"], "run");
}

#[test]
fn numeric_and_array_bounds_serialize() {
    let property = PropertySchema {
        property_type: "array".to_owned(),
        min_items: Some(1),
        max_items: Some(10),
        items: Some(Box::new(PropertySchema {
            property_type: "number".to_owned(),
            minimum: Some(0.0),
            maximum: Some(100.0),
            ..Default::default()
        })),
        ..Default::default()
    };

    let value = serde_json::to_value(&property).expect("serializes");
    assert_eq!(value["minItems"], 1);
    assert_eq!(value["maxItems"], 10);
    assert_eq!(value["items"]["minimum"], 0.0);
    assert_eq!(value["items"]["maximum"], 100.0);
}

#[test]
fn a_ref_only_subschema_omits_type() {
    // A `$ref` subschema carries no `type`. An empty `property_type` is the
    // signal to omit it; emitting `"type": ""` would be schema-invalid.
    let property = PropertySchema {
        ref_path: Some("#/$defs/Activity".to_owned()),
        ..Default::default()
    };

    let value = serde_json::to_value(&property).expect("serializes");
    assert_eq!(value["$ref"], "#/$defs/Activity");
    assert!(
        value.get("type").is_none(),
        "a $ref-only subschema must not carry a type: {value}"
    );
}

#[test]
fn composition_and_defs_serialize_with_dollar_names() {
    let mut defs = BTreeMap::new();
    defs.insert(
        "Activity".to_owned(),
        PropertySchema {
            property_type: "object".to_owned(),
            ..Default::default()
        },
    );

    let schema = JsonSchema {
        schema_dialect: Some("https://json-schema.org/draft/2020-12/schema".to_owned()),
        schema_type: "object".to_owned(),
        defs: Some(defs),
        one_of: Some(vec![
            PropertySchema {
                required: Some(vec!["a".to_owned()]),
                ..Default::default()
            },
            PropertySchema {
                required: Some(vec!["b".to_owned()]),
                ..Default::default()
            },
        ]),
        additional_properties: Some(false),
        ..Default::default()
    };

    let value = serde_json::to_value(&schema).expect("serializes");
    assert_eq!(
        value["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(value["$defs"]["Activity"].is_object());
    assert_eq!(value["oneOf"].as_array().expect("array").len(), 2);
    assert_eq!(value["additionalProperties"], false);
    // The composition branches carry no `type`.
    assert!(value["oneOf"][0].get("type").is_none());
}

#[test]
fn tool_schema_carries_an_output_schema() {
    let schema = ToolSchema::without_annotations(
        "get_activities".to_owned(),
        "List activities".to_owned(),
        JsonSchema {
            schema_type: "object".to_owned(),
            ..Default::default()
        },
    )
    .with_output_schema(JsonSchema {
        schema_type: "object".to_owned(),
        required: Some(vec!["activities".to_owned()]),
        ..Default::default()
    });

    let value = serde_json::to_value(&schema).expect("serializes");
    assert_eq!(value["outputSchema"]["type"], "object");
    assert_eq!(value["outputSchema"]["required"][0], "activities");

    // A tool that declares no output schema must omit the key entirely.
    let bare = ToolSchema::without_annotations(
        "ping".to_owned(),
        "Ping".to_owned(),
        JsonSchema {
            schema_type: "object".to_owned(),
            ..Default::default()
        },
    );
    let value = serde_json::to_value(&bare).expect("serializes");
    assert!(value.get("outputSchema").is_none());
}

#[test]
fn a_2020_12_schema_round_trips() {
    let original = JsonSchema {
        schema_dialect: Some("https://json-schema.org/draft/2020-12/schema".to_owned()),
        schema_type: "object".to_owned(),
        description: Some("A composed schema".to_owned()),
        any_of: Some(vec![PropertySchema {
            const_value: Some(json!("fixed")),
            ..Default::default()
        }]),
        ..Default::default()
    };

    let encoded = serde_json::to_string(&original).expect("serializes");
    let decoded: JsonSchema = serde_json::from_str(&encoded).expect("round-trips");

    assert_eq!(decoded.schema_type, "object");
    assert_eq!(decoded.description.as_deref(), Some("A composed schema"));
    assert_eq!(
        decoded
            .any_of
            .expect("anyOf survives")
            .first()
            .expect("one branch")
            .const_value,
        Some(json!("fixed"))
    );
}

/// A schema serializes with ordered keys at EVERY level, nested ones included.
///
/// This is what a consumer relies on when it hashes a tool schema, diffs it, or
/// checks a generated SDK type into git: two processes over the same schema must
/// emit the same bytes, or identical schemas read as drift.
///
/// Nested levels are the point. Moving only the top-level map to an ordered one
/// leaves a schema whose outer keys are stable and whose `properties.x.properties`
/// still reshuffles per process — which looks fixed on a flat schema and is not
/// fixed at all on a real one.
///
/// Asserted against the serialized STRING, not `serde_json::to_value`. A `Value`
/// map is a `BTreeMap` unless `preserve_order` is on, so it sorts the keys
/// itself and would report success over an unordered source — and whether that
/// feature is on depends on which consumer's build unified it in. The string is
/// what actually goes on the wire.
#[test]
fn nested_schema_keys_serialize_in_order() {
    fn many(prefix: &str) -> BTreeMap<String, PropertySchema> {
        (0..12)
            .map(|i| {
                let key = format!("{prefix}_{i:02}");
                (
                    key.clone(),
                    PropertySchema {
                        property_type: "string".to_owned(),
                        description: Some(format!("field {key}")),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    let mut outer = many("outer");
    outer.insert(
        "nested".to_owned(),
        PropertySchema {
            property_type: "object".to_owned(),
            properties: Some(many("inner")),
            ..Default::default()
        },
    );

    let encoded = serde_json::to_string(&JsonSchema {
        schema_type: "object".to_owned(),
        properties: Some(outer),
        ..Default::default()
    })
    .expect("serializes");

    // The inner keys appear in the string in the order the map yielded them.
    let positions: Vec<usize> = (0..12)
        .map(|i| {
            let key = format!("\"inner_{i:02}\"");
            encoded
                .find(&key)
                .unwrap_or_else(|| panic!("{key} is present in the encoded schema"))
        })
        .collect();

    let mut ascending = positions.clone();
    ascending.sort_unstable();
    assert_eq!(
        positions, ascending,
        "nested property keys must serialize in order — the half 0.10.0 missed"
    );
}
