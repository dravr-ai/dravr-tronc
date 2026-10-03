// ABOUTME: MCP logging utility wire types: the RFC 5424 severity levels and notifications/message
// ABOUTME: Levels order by severity, so a client's minimum level filters what a call may send
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The logging utility (`server/utilities/logging`).
//!
//! A server that declares the `logging` capability may send its client
//! `notifications/message`, each carrying a [`LogLevel`], an optional logger
//! name and arbitrary JSON `data`. The client picks the least severe level it
//! wants: with `logging/setLevel` on an `initialize`-era session, or with the
//! `io.modelcontextprotocol/logLevel` `_meta` key on each request in revision
//! 2026-07-28. A call sends a message only at or above that level, and none
//! at all when the client named no level.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `logging/setLevel`: an `initialize`-era client sets its session's minimum
/// level. Revision 2026-07-28 removed it in favour of a per-request `_meta` key.
pub const LOGGING_SET_LEVEL: &str = "logging/setLevel";

/// `notifications/message`: one log message from server to client.
pub const NOTIFICATIONS_MESSAGE: &str = "notifications/message";

/// The severity of a log message, the eight levels of RFC 5424 §6.2.1.
///
/// Declared from least to most severe, so the derived ordering is severity:
/// a message is sent when `level >= minimum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Detailed debugging information.
    Debug,
    /// General informational messages.
    Info,
    /// Normal but significant events.
    Notice,
    /// Warning conditions.
    Warning,
    /// Error conditions.
    Error,
    /// Critical conditions.
    Critical,
    /// Action must be taken immediately.
    Alert,
    /// The system is unusable.
    Emergency,
}

impl LogLevel {
    /// Every level, least severe first.
    pub const ALL: [Self; 8] = [
        Self::Debug,
        Self::Info,
        Self::Notice,
        Self::Warning,
        Self::Error,
        Self::Critical,
        Self::Alert,
        Self::Emergency,
    ];

    /// The level's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }

    /// The level a wire name names, or `None` for anything else: the names
    /// are exact and lower-case.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == name)
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `params` of a `notifications/message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingMessageParams {
    /// The message's severity.
    pub level: LogLevel,
    /// The name of the logger that produced it, when the server names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    /// The message itself: a string, or any JSON a client can display.
    pub data: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn levels_order_by_severity() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Warning < LogLevel::Error);
        assert!(LogLevel::Alert < LogLevel::Emergency);
        let mut shuffled = vec![LogLevel::Emergency, LogLevel::Debug, LogLevel::Notice];
        shuffled.sort();
        assert_eq!(
            shuffled,
            vec![LogLevel::Debug, LogLevel::Notice, LogLevel::Emergency]
        );
    }

    #[test]
    fn wire_names_round_trip_and_unknown_names_are_none() {
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::from_wire(level.as_str()), Some(level));
            assert_eq!(json!(level), json!(level.as_str()));
        }
        assert_eq!(LogLevel::from_wire("INFO"), None);
        assert_eq!(LogLevel::from_wire("verbose"), None);
    }

    #[test]
    fn a_message_omits_an_absent_logger() {
        let params = LoggingMessageParams {
            level: LogLevel::Info,
            logger: None,
            data: json!("started"),
        };
        assert_eq!(
            serde_json::to_value(&params).expect("serialize"), // Safe: test assertion
            json!({ "level": "info", "data": "started" })
        );
    }
}
