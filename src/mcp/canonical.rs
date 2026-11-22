// ABOUTME: Canonical (key-ordered) JSON: one rendering of a value whatever order its maps were built in
// ABOUTME: What a tools/list definition is hashed over (Tool::def_hash), and what a host renders schemas through

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Deterministic JSON.
//!
//! `serde_json` keeps insertion order once anything in the build enables its
//! `preserve_order` feature, and a schema built from a `HashMap` is inserted
//! in a different order on every build of the map. Two facts follow: the same
//! definition can reach a client in a different byte order on every request,
//! and a hash over it changes with nothing changed. Rewriting every object
//! with its keys sorted, at every depth, fixes both. [`Tool::def_hash`]
//! hashes this rendering, and a host renders its schemas through
//! [`to_canonical_value`] so its `tools/list` bytes are stable too.
//!
//! [`Tool::def_hash`]: crate::mcp::schema::Tool::def_hash

use serde::Serialize;
use serde_json::{Map, Value};

/// `value` as JSON with every object's keys in sorted order, at every depth.
///
/// `Value::Null` when `value` cannot be serialized: a tool whose schema
/// cannot be rendered is listed without one rather than failing the listing.
pub fn to_canonical_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).map_or(Value::Null, sort_keys)
}

/// `value` rewritten with every object's keys sorted, recursively.
fn sort_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_keys(value)))
                    .collect::<Map<String, Value>>(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_at_every_depth() {
        let value = json!({"b": {"y": 1, "x": [ {"d": 0, "c": 1} ]}, "a": 2});
        assert_eq!(
            to_canonical_value(&value).to_string(),
            r#"{"a":2,"b":{"x":[{"c":1,"d":0}],"y":1}}"#
        );
    }

    #[test]
    fn arrays_keep_their_order() {
        let value = json!(["b", "a"]);
        assert_eq!(to_canonical_value(&value), json!(["b", "a"]));
    }
}
