// ABOUTME: Initialize-era session state: what a client declared at initialize, and its log level
// ABOUTME: A stdio connection is one session, holding that state for every later request on it
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Sessions of the `initialize` era (revision 2025-11-25 and before).
//!
//! An `initialize`-era client declares its capabilities once, in
//! `initialize`, and sets its log level once, with `logging/setLevel`; every
//! later request relies on both. That state has to outlive the request that
//! set it, which is what a session is. A stdio connection is one session by
//! construction.
//!
//! Revision 2026-07-28 has no sessions: a modern request declares everything
//! it relies on in its own `_meta`, and never reaches this module.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::mcp::logging::LogLevel;

/// What a session remembers between requests.
#[derive(Debug, Default)]
struct SessionState {
    /// The `capabilities` the client declared in `initialize`, verbatim.
    capabilities: Option<Value>,
    /// The minimum level the client asked for with `logging/setLevel`.
    log_level: Option<LogLevel>,
}

/// One `initialize`-era session.
#[derive(Debug, Default)]
pub(crate) struct Session {
    state: Mutex<SessionState>,
}

impl Session {
    /// The session of a connection-scoped transport (stdio).
    pub(crate) fn connection() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        // Every critical section is one assignment or one read, so a panic
        // elsewhere cannot leave the state half-written.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record what the client declared in `initialize`.
    pub(crate) fn record_capabilities(&self, capabilities: Option<Value>) {
        self.state().capabilities = capabilities;
    }

    /// The capabilities the client declared in `initialize`, if it has.
    pub(crate) fn capabilities(&self) -> Option<Value> {
        self.state().capabilities.clone()
    }

    /// Record the minimum level the client asked for.
    pub(crate) fn set_log_level(&self, level: LogLevel) {
        self.state().log_level = Some(level);
    }

    /// The minimum level the client asked for, if it has.
    pub(crate) fn log_level(&self) -> Option<LogLevel> {
        self.state().log_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_remembers_capabilities_and_log_level() {
        let session = Session::connection();
        assert_eq!(session.capabilities(), None);
        assert_eq!(session.log_level(), None);
        session.record_capabilities(Some(serde_json::json!({ "sampling": {} })));
        session.set_log_level(LogLevel::Warning);
        assert_eq!(
            session.capabilities(),
            Some(serde_json::json!({ "sampling": {} }))
        );
        assert_eq!(session.log_level(), Some(LogLevel::Warning));
    }
}
