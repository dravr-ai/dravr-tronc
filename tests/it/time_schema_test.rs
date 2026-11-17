// ABOUTME: JSON schema goldens for the date types: literal expected schemas, and a chrono twin struct through schemars
// ABOUTME: A tool or SDK schema generated from a migrated struct must not change by one byte
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use dravr_tronc::time::{CivilDate, CivilDateTime, CivilTime, Instant, Weekday};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap()
}

#[test]
fn each_type_has_the_recorded_schema() {
    let draft = "https://json-schema.org/draft/2020-12/schema";
    assert_eq!(
        schema::<Instant>(),
        json!({"$schema": draft, "title": "Instant", "type": "string", "format": "date-time"})
    );
    assert_eq!(
        schema::<CivilDate>(),
        json!({"$schema": draft, "title": "CivilDate", "type": "string", "format": "date"})
    );
    assert_eq!(
        schema::<CivilDateTime>(),
        json!({"$schema": draft, "title": "CivilDateTime", "type": "string", "format": "partial-date-time"})
    );
    assert_eq!(
        schema::<CivilTime>(),
        json!({"$schema": draft, "title": "CivilTime", "type": "string", "format": "partial-time"})
    );
    assert_eq!(
        schema::<Weekday>(),
        json!({
            "$schema": draft,
            "title": "Weekday",
            "type": "string",
            "enum": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        })
    );
}

/// A tool input as a satellite declares one.
#[derive(JsonSchema, Serialize, Deserialize)]
struct Session {
    /// When the session started.
    started_at: Instant,
    ended_at: Option<Instant>,
    day: CivilDate,
    #[serde(default)]
    wake: Option<CivilTime>,
    local_start: CivilDateTime,
    rest_day: Weekday,
    laps: Vec<Instant>,
}

/// The same struct on chrono, under the same name and the same doc comments
/// so the titles and descriptions agree.
mod twin {
    use super::{
        DateTime, Deserialize, JsonSchema, NaiveDate, NaiveDateTime, NaiveTime, Serialize, Utc,
    };

    /// A tool input as a satellite declares one.
    #[derive(JsonSchema, Serialize, Deserialize)]
    pub struct Session {
        /// When the session started.
        pub started_at: DateTime<Utc>,
        pub ended_at: Option<DateTime<Utc>>,
        pub day: NaiveDate,
        #[serde(default)]
        pub wake: Option<NaiveTime>,
        pub local_start: NaiveDateTime,
        pub rest_day: chrono::Weekday,
        pub laps: Vec<DateTime<Utc>>,
    }
}

#[test]
fn a_struct_of_date_fields_has_the_schema_of_its_chrono_twin() {
    let ours = schema::<Session>();
    assert_eq!(ours, schema::<twin::Session>());
    // Byte for byte, not only as a JSON value: property order included.
    assert_eq!(
        serde_json::to_string(&schema_for!(Session)).unwrap(),
        serde_json::to_string(&schema_for!(twin::Session)).unwrap()
    );
    assert_eq!(
        ours,
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Session",
            "description": "A tool input as a satellite declares one.",
            "type": "object",
            "properties": {
                "started_at": {
                    "description": "When the session started.",
                    "type": "string",
                    "format": "date-time"
                },
                "ended_at": {"type": ["string", "null"], "format": "date-time"},
                "day": {"type": "string", "format": "date"},
                "wake": {"type": ["string", "null"], "format": "partial-time", "default": null},
                "local_start": {"type": "string", "format": "partial-date-time"},
                "rest_day": {
                    "type": "string",
                    "enum": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                },
                "laps": {
                    "type": "array",
                    "items": {"type": "string", "format": "date-time"}
                }
            },
            "required": ["started_at", "day", "local_start", "rest_day", "laps"]
        })
    );
    // Inline, as chrono's schemas are: nothing is moved to `$defs`.
    assert_eq!(ours.get("$defs"), None);

    // The schema describes what serde writes: one document, both structs.
    let document = r#"{"started_at":"2026-11-17T10:00:00.100Z","ended_at":null,"day":"2026-11-17","wake":"06:30:00","local_start":"2026-11-17T05:00:00","rest_day":"Sun","laps":["2026-11-17T10:05:00Z"]}"#;
    let mine: Session = serde_json::from_str(document).unwrap();
    let theirs: twin::Session = serde_json::from_str(document).unwrap();
    assert_eq!(serde_json::to_string(&mine).unwrap(), document);
    assert_eq!(serde_json::to_string(&theirs).unwrap(), document);
}
