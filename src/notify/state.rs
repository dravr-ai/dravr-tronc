// ABOUTME: Internal state for NotifyLayer — dedup map, batch buffers, and the SpanFields stash
// ABOUTME: All types are crate-internal; the layer threads them through Arc<Mutex<...>> so it can stay Clone
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-span stash placed into `tracing_subscriber::registry::SpanRef::extensions_mut`
/// at span creation time. Holds the fields declared on `#[instrument(fields(...))]`
/// — typically `tenant_id` and `user_id` — so `on_event` can fold them into
/// every emitted notify event without the call site repeating them.
#[derive(Debug, Default)]
pub(crate) struct SpanFields {
    pub fields: HashMap<String, String>,
}

impl SpanFields {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Dedup key: `(event_name, joined_values)`. We pre-join the per-key values
/// with a NUL byte separator so two distinct field tuples can't collide via
/// boundary ambiguity (e.g. `["ab", "c"]` vs `["a", "bc"]`).
pub(crate) type DedupKey = (String, String);

/// Shared dedup state. Maps a `DedupKey` to the last time it fired so the
/// layer can drop repeats inside the rule's window.
pub(crate) type DedupMap = Arc<Mutex<HashMap<DedupKey, Instant>>>;

/// One buffered, formatted line waiting for the next batch flush.
#[derive(Debug, Clone)]
pub(crate) struct BatchedLine {
    pub queued_at: Instant,
    pub text: String,
}

/// Shared batch state. Maps an event name to its pending lines plus the
/// rule's flush interval (carried alongside the buffer so the flusher
/// doesn't have to re-resolve it on every tick).
#[derive(Debug, Default)]
pub(crate) struct BatchBuffers {
    pub by_event: HashMap<String, BatchBuffer>,
}

#[derive(Debug, Clone)]
pub(crate) struct BatchBuffer {
    pub channel: String,
    pub interval: Duration,
    pub lines: Vec<BatchedLine>,
}

impl BatchBuffer {
    pub(crate) fn new(channel: String, interval: Duration) -> Self {
        Self {
            channel,
            interval,
            lines: Vec::new(),
        }
    }
}

/// Shared batch buffers wrapped for cross-task access.
pub(crate) type SharedBatchBuffers = Arc<Mutex<BatchBuffers>>;

/// Build a dedup key from the rule's key field names and the merged event
/// fields. Missing fields render as empty so the key shape stays stable.
pub(crate) fn build_dedup_key(
    event_name: &str,
    keys: &[String],
    fields: &HashMap<String, String>,
) -> DedupKey {
    let mut joined = String::with_capacity(keys.len() * 16);
    for (idx, key) in keys.iter().enumerate() {
        if idx > 0 {
            joined.push('\0');
        }
        if let Some(value) = fields.get(key) {
            joined.push_str(value);
        }
    }
    (event_name.to_owned(), joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dedup_key_concatenates_with_separator() {
        let mut fields = HashMap::new();
        fields.insert("user_id".to_owned(), "u1".to_owned());
        fields.insert("tenant_id".to_owned(), "t1".to_owned());

        let key = build_dedup_key(
            "user.login",
            &["user_id".to_owned(), "tenant_id".to_owned()],
            &fields,
        );
        assert_eq!(key.0, "user.login");
        assert_eq!(key.1, "u1\0t1");
    }

    #[test]
    fn build_dedup_key_missing_field_renders_empty() {
        let fields = HashMap::new();
        let key = build_dedup_key("e", &["missing".to_owned()], &fields);
        assert_eq!(key.1, "");
    }

    #[test]
    fn build_dedup_key_distinguishes_boundary_ambiguity() {
        let mut fields_a = HashMap::new();
        fields_a.insert("a".to_owned(), "ab".to_owned());
        fields_a.insert("b".to_owned(), "c".to_owned());

        let mut fields_b = HashMap::new();
        fields_b.insert("a".to_owned(), "a".to_owned());
        fields_b.insert("b".to_owned(), "bc".to_owned());

        let key_a = build_dedup_key("e", &["a".to_owned(), "b".to_owned()], &fields_a);
        let key_b = build_dedup_key("e", &["a".to_owned(), "b".to_owned()], &fields_b);
        assert_ne!(key_a, key_b);
    }
}
