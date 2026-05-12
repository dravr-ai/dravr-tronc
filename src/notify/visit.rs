// ABOUTME: tracing::Visit implementation that pulls the `event` field plus all other fields off an event/span
// ABOUTME: Shared by NotifyLayer's on_event (extract event name + fields) and on_new_span/on_record (stash span fields)
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use tracing::field::{Field, Visit};

/// Field name carrying the canonical event identifier in `info!(target:
/// "notify", event = "user.login", ...)`.
pub(crate) const EVENT_FIELD: &str = "event";

/// Visits a tracing event or span and collects every recorded field as a
/// `HashMap<String, String>`. Captures the `event` name into a dedicated slot
/// so the layer can route on it without scanning the map twice.
#[derive(Debug, Default)]
pub(crate) struct NotifyVisitor {
    /// The `event = "..."` value if the visitor saw it.
    pub event_name: Option<String>,
    /// All other recorded fields.
    pub fields: HashMap<String, String>,
}

impl NotifyVisitor {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl Visit for NotifyVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == EVENT_FIELD {
            self.event_name = Some(value.to_owned());
        } else {
            self.fields
                .insert(field.name().to_owned(), value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        // `Debug` on a `&str` wraps the value in quotes — strip them so a span
        // field declared `tenant_id = %tid` lands as `tenant_id=abc123` not
        // `tenant_id="abc123"` in the Slack line.
        let cleaned = strip_debug_quotes(&rendered);
        if field.name() == EVENT_FIELD {
            self.event_name = Some(cleaned.into_owned());
        } else {
            self.fields
                .insert(field.name().to_owned(), cleaned.into_owned());
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
}

/// Strip a single pair of surrounding double quotes if both ends carry one.
/// `Debug` formatting of `&str` yields `"hello"` — Slack lines read better
/// without the quotes for IDs.
fn strip_debug_quotes(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        Cow::Owned(s[1..s.len() - 1].to_owned())
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_debug_quotes_removes_wrapping_quotes() {
        assert_eq!(strip_debug_quotes("\"abc\""), "abc");
    }

    #[test]
    fn strip_debug_quotes_leaves_unquoted_untouched() {
        assert_eq!(strip_debug_quotes("123"), "123");
    }

    #[test]
    fn strip_debug_quotes_leaves_single_quote_alone() {
        assert_eq!(strip_debug_quotes("\"unbalanced"), "\"unbalanced");
    }
}
