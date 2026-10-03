// ABOUTME: Form-mode elicitation wire types: elicitation/create params, its restricted schema, the answer
// ABOUTME: Primitive schemas carry SEP-1034 defaults and the five SEP-1330 enum shapes
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Elicitation (`client/elicitation`, revision 2025-11-25), form mode.
//!
//! A server asks its client for structured input with `elicitation/create`,
//! naming what it wants in a restricted JSON Schema: a flat object whose
//! properties are each one [`PrimitiveSchema`] — a string, a number or
//! integer, a boolean, or an enum. The client shows a form, and answers with
//! an [`ElicitResult`]: the person accepted (with `content` matching the
//! schema), declined, or dismissed it.
//!
//! Every primitive may carry a `default` the form starts from (SEP-1034), and
//! an enum takes one of five shapes (SEP-1330): single-select with or without
//! titles, the legacy `enumNames` form, and multi-select with or without
//! titles. Each shape is its own type here, so a schema that would confuse a
//! client — titles in two places, a multi-select without `items` — cannot be
//! written.
//!
//! Revision 2026-07-28 carries no server-to-client request inside a call: a
//! tool there asks through
//! [`CallToolOutcome::InputRequired`](crate::mcp::host::CallToolOutcome::InputRequired)
//! instead.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// `elicitation/create`: the server asks the client for input.
pub const ELICITATION_CREATE: &str = "elicitation/create";

/// The `params` of a form-mode `elicitation/create`.
///
/// `mode` is left off the wire: form is what a request without one means in
/// revision 2025-11-25, and what every earlier client understands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElicitRequest {
    /// What the client shows the person, saying what is asked and why.
    pub message: String,
    /// The shape of the answer.
    #[serde(rename = "requestedSchema")]
    pub requested_schema: ElicitationSchema,
}

/// The `requestedSchema` of a form elicitation: a flat object of primitives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElicitationSchema {
    /// Always `"object"`.
    #[serde(rename = "type")]
    pub kind: ObjectType,
    /// The fields of the form, by name.
    pub properties: BTreeMap<String, PrimitiveSchema>,
    /// The names of the fields an accepted answer must fill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

/// The `type` of an [`ElicitationSchema`]: `"object"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectType {
    /// `"object"`.
    #[default]
    #[serde(rename = "object")]
    Object,
}

/// The `type` of a string-valued field: `"string"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StringType {
    /// `"string"`.
    #[default]
    #[serde(rename = "string")]
    String,
}

/// The `type` of a numeric field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumberType {
    /// `"number"`: any number.
    #[default]
    #[serde(rename = "number")]
    Number,
    /// `"integer"`: a whole number.
    #[serde(rename = "integer")]
    Integer,
}

/// The `type` of a boolean field: `"boolean"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BooleanType {
    /// `"boolean"`.
    #[default]
    #[serde(rename = "boolean")]
    Boolean,
}

/// The `type` of a multi-select field: `"array"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrayType {
    /// `"array"`.
    #[default]
    #[serde(rename = "array")]
    Array,
}

/// One field of an [`ElicitationSchema`].
///
/// Untagged on the wire, as the specification writes it; reading one tries
/// the shapes that need a distinguishing member (`oneOf`, `enumNames`,
/// `enum`, `items`) before the plain ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrimitiveSchema {
    /// A single-select enum whose options carry titles (SEP-1330).
    TitledSingleSelect(TitledSingleSelectSchema),
    /// A single-select enum titled through `enumNames`, the shape SEP-1330
    /// keeps for clients that predate `oneOf`.
    LegacyTitledEnum(LegacyTitledEnumSchema),
    /// A single-select enum of bare values (SEP-1330).
    UntitledSingleSelect(UntitledSingleSelectSchema),
    /// A multi-select enum whose options carry titles (SEP-1330).
    TitledMultiSelect(TitledMultiSelectSchema),
    /// A multi-select enum of bare values (SEP-1330).
    UntitledMultiSelect(UntitledMultiSelectSchema),
    /// Free text.
    String(StringSchema),
    /// A number or an integer.
    Number(NumberSchema),
    /// A yes/no choice.
    Boolean(BooleanSchema),
}

/// A free-text field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringSchema {
    /// Always `"string"`.
    #[serde(rename = "type")]
    pub kind: StringType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The fewest characters an answer may have.
    #[serde(rename = "minLength", default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    /// The most characters an answer may have.
    #[serde(rename = "maxLength", default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    /// A format the answer must have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<StringFormat>,
    /// The value the form starts with (SEP-1034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// The formats a [`StringSchema`] may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StringFormat {
    /// An e-mail address.
    #[serde(rename = "email")]
    Email,
    /// A URI.
    #[serde(rename = "uri")]
    Uri,
    /// A calendar date.
    #[serde(rename = "date")]
    Date,
    /// A date and time.
    #[serde(rename = "date-time")]
    DateTime,
}

/// A numeric field. Bounds and default are JSON numbers as written, so an
/// integer field's default stays an integer on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberSchema {
    /// `"number"` or `"integer"`.
    #[serde(rename = "type")]
    pub kind: NumberType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The smallest value allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<Number>,
    /// The largest value allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<Number>,
    /// The value the form starts with (SEP-1034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Number>,
}

/// A yes/no field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooleanSchema {
    /// Always `"boolean"`.
    #[serde(rename = "type")]
    pub kind: BooleanType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The value the form starts with (SEP-1034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
}

/// One titled option of an enum: the value sent back, and its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumOption {
    /// The value an answer carries when this option is picked.
    #[serde(rename = "const")]
    pub value: String,
    /// What the person sees.
    pub title: String,
}

/// A single-select enum of bare values: `{"type":"string","enum":[…]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntitledSingleSelectSchema {
    /// Always `"string"`.
    #[serde(rename = "type")]
    pub kind: StringType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The values to choose from.
    #[serde(rename = "enum")]
    pub values: Vec<String>,
    /// The value the form starts with (SEP-1034); one of [`Self::values`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// A single-select enum with a title per option:
/// `{"type":"string","oneOf":[{"const":…,"title":…}]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitledSingleSelectSchema {
    /// Always `"string"`.
    #[serde(rename = "type")]
    pub kind: StringType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The options to choose from.
    #[serde(rename = "oneOf")]
    pub options: Vec<EnumOption>,
    /// The value the form starts with (SEP-1034); one option's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// A single-select enum titled through a parallel `enumNames` list — the
/// shape before `oneOf`, kept so older clients can still show titles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyTitledEnumSchema {
    /// Always `"string"`.
    #[serde(rename = "type")]
    pub kind: StringType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The values to choose from.
    #[serde(rename = "enum")]
    pub values: Vec<String>,
    /// The title of each value, in the same order and of the same length.
    #[serde(rename = "enumNames")]
    pub names: Vec<String>,
    /// The value the form starts with (SEP-1034); one of [`Self::values`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// The `items` of an [`UntitledMultiSelectSchema`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntitledEnumItems {
    /// Always `"string"`.
    #[serde(rename = "type")]
    pub kind: StringType,
    /// The values to choose from.
    #[serde(rename = "enum")]
    pub values: Vec<String>,
}

/// The `items` of a [`TitledMultiSelectSchema`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitledEnumItems {
    /// The options to choose from.
    #[serde(rename = "anyOf")]
    pub options: Vec<EnumOption>,
}

/// A multi-select enum of bare values:
/// `{"type":"array","items":{"type":"string","enum":[…]}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntitledMultiSelectSchema {
    /// Always `"array"`.
    #[serde(rename = "type")]
    pub kind: ArrayType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The fewest values an answer may pick.
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    /// The most values an answer may pick.
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// The values to choose from.
    pub items: UntitledEnumItems,
    /// The values the form starts with picked (SEP-1034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Vec<String>>,
}

/// A multi-select enum with a title per option:
/// `{"type":"array","items":{"anyOf":[{"const":…,"title":…}]}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitledMultiSelectSchema {
    /// Always `"array"`.
    #[serde(rename = "type")]
    pub kind: ArrayType,
    /// The field's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the field is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The fewest values an answer may pick.
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    /// The most values an answer may pick.
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// The options to choose from.
    pub items: TitledEnumItems,
    /// The values the form starts with picked (SEP-1034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Vec<String>>,
}

/// What the person did with the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitAction {
    /// Submitted it: `content` carries the answer.
    Accept,
    /// Explicitly refused to answer.
    Decline,
    /// Dismissed it without choosing.
    Cancel,
}

/// The client's answer to an `elicitation/create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElicitResult {
    /// What the person did.
    pub action: ElicitAction,
    /// The submitted fields, by name, when [`Self::action`] is
    /// [`ElicitAction::Accept`]: strings, numbers, booleans, or lists of
    /// strings for a multi-select.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Map<String, Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(pairs: &[(&str, &str)]) -> Vec<EnumOption> {
        pairs
            .iter()
            .map(|(value, title)| EnumOption {
                value: (*value).to_owned(),
                title: (*title).to_owned(),
            })
            .collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    /// The five SEP-1330 shapes, each under the name the conformance suite
    /// reads it by.
    fn sep1330_schema() -> ElicitationSchema {
        let properties = BTreeMap::from([
            (
                "untitledSingle".to_owned(),
                PrimitiveSchema::UntitledSingleSelect(UntitledSingleSelectSchema {
                    values: strings(&["option1", "option2", "option3"]),
                    ..UntitledSingleSelectSchema::default()
                }),
            ),
            (
                "titledSingle".to_owned(),
                PrimitiveSchema::TitledSingleSelect(TitledSingleSelectSchema {
                    options: options(&[("value1", "First Option"), ("value2", "Second Option")]),
                    ..TitledSingleSelectSchema::default()
                }),
            ),
            (
                "legacyEnum".to_owned(),
                PrimitiveSchema::LegacyTitledEnum(LegacyTitledEnumSchema {
                    values: strings(&["opt1", "opt2"]),
                    names: strings(&["Option One", "Option Two"]),
                    ..LegacyTitledEnumSchema::default()
                }),
            ),
            (
                "untitledMulti".to_owned(),
                PrimitiveSchema::UntitledMultiSelect(UntitledMultiSelectSchema {
                    items: UntitledEnumItems {
                        values: strings(&["option1", "option2"]),
                        ..UntitledEnumItems::default()
                    },
                    ..UntitledMultiSelectSchema::default()
                }),
            ),
            (
                "titledMulti".to_owned(),
                PrimitiveSchema::TitledMultiSelect(TitledMultiSelectSchema {
                    items: TitledEnumItems {
                        options: options(&[("value1", "First Choice")]),
                    },
                    ..TitledMultiSelectSchema::default()
                }),
            ),
        ]);
        ElicitationSchema {
            properties,
            ..ElicitationSchema::default()
        }
    }

    #[test]
    fn sep1330_enum_shapes_serialize_as_the_specification_writes_them() {
        let wire = serde_json::to_value(sep1330_schema()).expect("serialize"); // Safe: test assertion
        assert_eq!(
            wire,
            json!({
                "type": "object",
                "properties": {
                    "untitledSingle": { "type": "string", "enum": ["option1", "option2", "option3"] },
                    "titledSingle": { "type": "string", "oneOf": [
                        { "const": "value1", "title": "First Option" },
                        { "const": "value2", "title": "Second Option" }
                    ] },
                    "legacyEnum": { "type": "string", "enum": ["opt1", "opt2"],
                                    "enumNames": ["Option One", "Option Two"] },
                    "untitledMulti": { "type": "array",
                                       "items": { "type": "string", "enum": ["option1", "option2"] } },
                    "titledMulti": { "type": "array",
                                     "items": { "anyOf": [{ "const": "value1", "title": "First Choice" }] } }
                }
            })
        );
    }

    #[test]
    fn each_shape_reads_back_as_itself() {
        let schema = sep1330_schema();
        let wire = serde_json::to_value(&schema).expect("serialize"); // Safe: test assertion
        let read: ElicitationSchema = serde_json::from_value(wire).expect("deserialize"); // Safe: test assertion
        assert_eq!(read, schema);
    }

    #[test]
    fn sep1034_defaults_keep_their_json_types() {
        let schema = ElicitationSchema {
            properties: BTreeMap::from([
                (
                    "age".to_owned(),
                    PrimitiveSchema::Number(NumberSchema {
                        kind: NumberType::Integer,
                        default: Some(Number::from(30)),
                        ..NumberSchema::default()
                    }),
                ),
                (
                    "score".to_owned(),
                    PrimitiveSchema::Number(NumberSchema {
                        default: Number::from_f64(95.5),
                        ..NumberSchema::default()
                    }),
                ),
                (
                    "verified".to_owned(),
                    PrimitiveSchema::Boolean(BooleanSchema {
                        default: Some(true),
                        ..BooleanSchema::default()
                    }),
                ),
                (
                    "name".to_owned(),
                    PrimitiveSchema::String(StringSchema {
                        default: Some("John Doe".to_owned()),
                        ..StringSchema::default()
                    }),
                ),
            ]),
            required: strings(&["name"]),
            ..ElicitationSchema::default()
        };
        let wire = serde_json::to_value(&schema).expect("serialize"); // Safe: test assertion
        assert_eq!(
            wire["properties"]["age"],
            json!({ "type": "integer", "default": 30 })
        );
        assert_eq!(
            wire["properties"]["score"],
            json!({ "type": "number", "default": 95.5 })
        );
        assert_eq!(
            wire["properties"]["verified"],
            json!({ "type": "boolean", "default": true })
        );
        assert_eq!(
            wire["properties"]["name"],
            json!({ "type": "string", "default": "John Doe" })
        );
        assert_eq!(wire["required"], json!(["name"]));
        let read: ElicitationSchema = serde_json::from_value(wire).expect("deserialize"); // Safe: test assertion
        assert_eq!(read, schema);
    }

    #[test]
    fn a_result_reads_each_action() {
        let accepted: ElicitResult = serde_json::from_value(json!({
            "action": "accept", "content": { "username": "ada" }
        }))
        .expect("deserialize"); // Safe: test assertion
        assert_eq!(accepted.action, ElicitAction::Accept);
        assert_eq!(
            accepted.content.and_then(|c| c.get("username").cloned()),
            Some(json!("ada"))
        );
        for (wire, action) in [
            ("decline", ElicitAction::Decline),
            ("cancel", ElicitAction::Cancel),
        ] {
            let read: ElicitResult =
                serde_json::from_value(json!({ "action": wire })).expect("deserialize"); // Safe: test assertion
            assert_eq!(read.action, action);
            assert_eq!(read.content, None);
        }
    }
}
