// ABOUTME: schemars 1.x JSON schemas for the date types: the schemas schemars' chrono04 support emits
// ABOUTME: Inline strings with the same `format` values, so a generated tool or SDK schema does not change
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::borrow::Cow;

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};

use super::{CivilDate, CivilDateTime, CivilTime, Instant, Weekday};

macro_rules! formatted_string_schema {
    ($type:ident, $format:literal) => {
        impl JsonSchema for $type {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                stringify!($type).into()
            }

            fn schema_id() -> Cow<'static, str> {
                concat!("dravr_tronc::time::", stringify!($type)).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "format": $format
                })
            }
        }
    };
}

formatted_string_schema!(Instant, "date-time");
formatted_string_schema!(CivilDate, "date");
formatted_string_schema!(CivilDateTime, "partial-date-time");
formatted_string_schema!(CivilTime, "partial-time");

impl JsonSchema for Weekday {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Weekday".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "dravr_tronc::time::Weekday".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        })
    }
}
