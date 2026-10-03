// ABOUTME: Initialize-era session state: what a client declared at initialize, and its log level
// ABOUTME: A stdio connection is one session; Streamable HTTP keys opt-in sessions by Mcp-Session-Id
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Sessions of the `initialize` era (revision 2025-11-25 and before).
//!
//! An `initialize`-era client declares its capabilities once, in
//! `initialize`, and sets its log level once, with `logging/setLevel`; every
//! later request relies on both. That state has to outlive the request that
//! set it, which is what a session is. A stdio connection is one session by
//! construction. Over Streamable HTTP the server mints an `Mcp-Session-Id` at
//! `initialize` and the client sends it back on every request — when the host
//! turns sessions on with
//! [`McpServer::with_http_sessions`](crate::mcp::server::McpServer::with_http_sessions).
//!
//! Revision 2026-07-28 has no sessions: a modern request declares everything
//! it relies on in its own `_meta`, and never reaches this module.
//!
//! A session belongs to the caller identity the auth hook resolved when it
//! was created, so its id alone does not let another caller use it. One that
//! sits idle — no request in flight — for longer than its time-to-live ends,
//! as one the client ends with `DELETE` does: its id stops resolving, and the
//! calls still running in it are cancelled.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::mcp::logging::LogLevel;
use crate::mcp::random_id::random_hex_id;
use crate::mcp::tool::ToolContext;

/// What a session remembers between requests.
#[derive(Debug, Default)]
struct SessionState {
    /// The `capabilities` the client declared in `initialize`, verbatim.
    capabilities: Option<Value>,
    /// The minimum level the client asked for with `logging/setLevel`.
    log_level: Option<LogLevel>,
}

/// One `initialize`-era session.
#[derive(Debug)]
pub(crate) struct Session {
    /// The `Mcp-Session-Id`; `None` for a stdio connection, which needs none.
    id: Option<String>,
    /// The caller it belongs to.
    user: Option<String>,
    tenant: Option<String>,
    state: Mutex<SessionState>,
    /// When a request last finished in it (or it was created).
    last_seen: Mutex<Instant>,
    /// Requests being served in it right now; a session with any is not idle.
    active: AtomicUsize,
    /// Fired when the session ends; every request served in it derives its
    /// cancellation from this.
    cancellation: CancellationToken,
}

impl Session {
    fn new(id: Option<String>, ctx: &ToolContext) -> Self {
        Self {
            id,
            user: ctx.user_id.clone(),
            tenant: ctx.tenant_id.clone(),
            state: Mutex::new(SessionState::default()),
            last_seen: Mutex::new(Instant::now()),
            active: AtomicUsize::new(0),
            cancellation: ctx.cancellation.child_token(),
        }
    }

    /// A new HTTP session for the caller `ctx`, with a fresh id, not yet
    /// live: it is [`SessionStore::insert`]ed once its `initialize` succeeds,
    /// so a refused handshake leaves nothing behind.
    ///
    /// # Errors
    ///
    /// The operating system could not supply randomness for the id.
    pub(crate) fn mint(ctx: &ToolContext) -> Result<Arc<Self>, getrandom::Error> {
        Ok(Arc::new(Self::new(Some(random_hex_id()?), ctx)))
    }

    /// The session of a connection-scoped transport (stdio): no id, and the
    /// anonymous caller every request on the connection is.
    pub(crate) fn connection() -> Arc<Self> {
        Arc::new(Self::new(None, &ToolContext::default()))
    }

    /// The `Mcp-Session-Id`, when the session has one.
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Whether `ctx` is the caller the session belongs to.
    pub(crate) fn is_owned_by(&self, ctx: &ToolContext) -> bool {
        self.user == ctx.user_id && self.tenant == ctx.tenant_id
    }

    /// The token every request served in the session derives from.
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
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

    /// Count a request as served in the session until the returned guard
    /// drops.
    pub(crate) fn enter(self: &Arc<Self>) -> SessionUse {
        self.active.fetch_add(1, Ordering::AcqRel);
        SessionUse {
            session: Arc::clone(self),
        }
    }

    fn last_seen(&self) -> MutexGuard<'_, Instant> {
        self.last_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the session has sat idle for at least `ttl` by `now`.
    fn is_expired(&self, ttl: Duration, now: Instant) -> bool {
        self.active.load(Ordering::Acquire) == 0
            && now.saturating_duration_since(*self.last_seen()) >= ttl
    }

    /// End the session: every request still served in it is cancelled.
    fn end(&self) {
        self.cancellation.cancel();
    }
}

/// A request being served in a session; dropping it marks the session seen.
#[derive(Debug)]
pub(crate) struct SessionUse {
    session: Arc<Session>,
}

impl Drop for SessionUse {
    fn drop(&mut self) {
        *self.session.last_seen() = Instant::now();
        self.session.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The live HTTP sessions of one server, by `Mcp-Session-Id`.
///
/// Held in process memory: a session minted by one instance of a service is
/// unknown to the others, which is why HTTP sessions are something a host
/// turns on only where every request of a client reaches the same instance.
#[derive(Debug)]
pub(crate) struct SessionStore {
    ttl: Duration,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// When expired sessions were last swept out.
    last_sweep: Mutex<Instant>,
}

impl SessionStore {
    /// A store whose sessions end after `ttl` idle.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            sessions: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Make `session` live.
    pub(crate) fn insert(&self, session: Arc<Session>) {
        let now = Instant::now();
        self.sweep_if_due(now);
        if let Some(id) = session.id().map(str::to_owned) {
            self.sessions().insert(id, session);
        }
    }

    /// The live session `id` names. One that expired is ended and forgotten
    /// on the way.
    pub(crate) fn find(&self, id: &str) -> Option<Arc<Session>> {
        let now = Instant::now();
        self.sweep_if_due(now);
        let mut sessions = self.sessions();
        let session = sessions.get(id)?;
        if session.is_expired(self.ttl, now) {
            if let Some(expired) = sessions.remove(id) {
                expired.end();
            }
            return None;
        }
        Some(Arc::clone(session))
    }

    /// End the session `id` names and forget it. Returns whether one did.
    pub(crate) fn end(&self, id: &str) -> bool {
        let removed = self.sessions().remove(id);
        removed.is_some_and(|session| {
            session.end();
            true
        })
    }

    /// Sweep expired sessions out, at most once per half time-to-live: a
    /// lookup finds an expired one by itself, so the sweep only bounds how
    /// long an abandoned one is held.
    fn sweep_if_due(&self, now: Instant) {
        {
            let mut last = self
                .last_sweep
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if now.saturating_duration_since(*last) < self.ttl / 2 {
                return;
            }
            *last = now;
        }
        self.sessions().retain(|_, session| {
            let expired = session.is_expired(self.ttl, now);
            if expired {
                session.end();
            }
            !expired
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_session_is_live_only_once_inserted() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let id = session
            .id()
            .map(str::to_owned)
            .expect("an HTTP session has an id"); // Safe: test assertion
        assert!(store.find(&id).is_none());
        store.insert(session);
        assert!(store.find(&id).is_some());
    }

    #[test]
    fn ending_a_session_cancels_its_requests_and_forgets_it() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let id = session.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let token = session.cancellation().child_token();
        store.insert(session);
        assert!(store.end(&id));
        assert!(token.is_cancelled());
        assert!(store.find(&id).is_none());
        assert!(!store.end(&id), "a session ends once");
    }

    #[test]
    fn an_idle_session_expires_and_a_busy_one_does_not() {
        let store = SessionStore::new(Duration::ZERO);
        let busy = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let idle = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let busy_id = busy.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let idle_id = idle.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let serving = busy.enter();
        let idle_token = idle.cancellation().child_token();
        store.insert(busy);
        store.insert(idle);
        assert!(store.find(&idle_id).is_none(), "idle past a zero TTL");
        assert!(idle_token.is_cancelled());
        assert!(store.find(&busy_id).is_some(), "a request is in flight");
        drop(serving);
        assert!(store.find(&busy_id).is_none(), "idle once it finished");
    }

    #[test]
    fn a_session_belongs_to_the_caller_that_created_it() {
        let owner = ToolContext::new().with_user("u1").with_tenant("t1");
        let session = Session::mint(&owner).expect("randomness"); // Safe: test assertion
        assert!(session.is_owned_by(&owner));
        assert!(!session.is_owned_by(&ToolContext::new().with_user("u2").with_tenant("t1")));
        assert!(!session.is_owned_by(&ToolContext::default()));
    }

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
