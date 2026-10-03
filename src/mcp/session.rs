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
//!
//! Anyone who can reach `initialize` can start a session, so what one holds
//! and how many a store holds are both bounded: a session keeps what the
//! client declared as flags, never the `capabilities` object it sent, and a
//! store holds at most [`SessionLimits::total`] sessions, at most
//! [`SessionLimits::per_caller`] of them for one caller. A caller at its own
//! limit gives up its least recently used idle session to start another — the
//! client holding that one is told it is gone, and initializes again — so a
//! client that loops on `initialize` churns through its own sessions instead
//! of growing the store.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::mcp::logging::LogLevel;
use crate::mcp::random_id::random_hex_id;
use crate::mcp::tool::ToolContext;

/// What a client declared in `initialize` that a later call relies on: the
/// capabilities a server request needs. Read once into flags, so a session
/// holds a few bytes however large an object the client sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeclaredCapabilities {
    /// `sampling`, declared as an object.
    pub(crate) sampling: bool,
    /// Form elicitation, declared as `elicitation: {}` — what every client
    /// before URL mode sends — or as an `elicitation` object with a `form`
    /// member.
    pub(crate) form_elicitation: bool,
}

impl DeclaredCapabilities {
    /// Read the `capabilities` object of an `initialize` request; none
    /// declares nothing.
    pub(crate) fn from_initialize(capabilities: Option<&Value>) -> Self {
        let Some(capabilities) = capabilities else {
            return Self::default();
        };
        Self {
            sampling: capabilities.get("sampling").is_some_and(Value::is_object),
            form_elicitation: capabilities
                .get("elicitation")
                .and_then(Value::as_object)
                .is_some_and(|elicitation| {
                    elicitation.is_empty() || elicitation.contains_key("form")
                }),
        }
    }
}

/// What a session remembers between requests.
#[derive(Debug, Default)]
struct SessionState {
    /// What the client declared in `initialize`.
    capabilities: DeclaredCapabilities,
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

    /// Whether the session belongs to the same caller as `other`.
    fn shares_caller_with(&self, other: &Self) -> bool {
        self.user == other.user && self.tenant == other.tenant
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

    /// Record what the client declared in the `capabilities` of its
    /// `initialize`.
    pub(crate) fn record_capabilities(&self, capabilities: Option<&Value>) {
        self.state().capabilities = DeclaredCapabilities::from_initialize(capabilities);
    }

    /// What the client declared in `initialize`; nothing before it has.
    pub(crate) fn capabilities(&self) -> DeclaredCapabilities {
        self.state().capabilities
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

    /// Whether no request is being served in the session.
    fn is_idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
    }

    /// Whether the session has sat idle for at least `ttl` by `now`.
    fn is_expired(&self, ttl: Duration, now: Instant) -> bool {
        self.is_idle() && now.saturating_duration_since(*self.last_seen()) >= ttl
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

/// How many HTTP sessions a store holds at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionLimits {
    /// Across every caller.
    pub(crate) total: NonZeroUsize,
    /// For one caller identity; every anonymous client is the one anonymous
    /// caller.
    pub(crate) per_caller: NonZeroUsize,
}

/// Why a store would not take a new session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionRefusal {
    /// The store holds [`SessionLimits::total`] live sessions.
    ServerFull,
    /// The caller holds [`SessionLimits::per_caller`] live sessions, each with
    /// a request in flight, so none can give way.
    CallerFull,
}

impl fmt::Display for SessionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerFull => f.write_str("the server holds as many sessions as it may"),
            Self::CallerFull => {
                f.write_str("this caller holds as many sessions as it may, each serving a request")
            }
        }
    }
}

impl StdError for SessionRefusal {}

/// The live HTTP sessions of one server, by `Mcp-Session-Id`.
///
/// Held in process memory: a session minted by one instance of a service is
/// unknown to the others, which is why HTTP sessions are something a host
/// turns on only where every request of a client reaches the same instance.
#[derive(Debug)]
pub(crate) struct SessionStore {
    ttl: Duration,
    limits: SessionLimits,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// When expired sessions were last swept out.
    last_sweep: Mutex<Instant>,
}

impl SessionStore {
    /// A store whose sessions end after `ttl` idle, holding at most `limits`.
    pub(crate) fn new(ttl: Duration, limits: SessionLimits) -> Self {
        Self {
            ttl,
            limits,
            sessions: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    /// The idle time-to-live of its sessions.
    pub(crate) const fn ttl(&self) -> Duration {
        self.ttl
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Make `session` live, within the store's limits.
    ///
    /// A caller already holding [`SessionLimits::per_caller`] sessions gives
    /// up its least recently used idle one, which ends; one with none idle is
    /// refused. A full store first drops its expired sessions, and refuses
    /// when that frees no room: it never ends another caller's live session
    /// to make some.
    ///
    /// # Errors
    ///
    /// [`SessionRefusal`] when there is no room for the session.
    pub(crate) fn insert(&self, session: Arc<Session>) -> Result<(), SessionRefusal> {
        let now = Instant::now();
        self.sweep_if_due(now);
        let Some(id) = session.id().map(str::to_owned) else {
            return Ok(());
        };
        let mut sessions = self.sessions();
        let mut theirs = 0_usize;
        let mut least_recent: Option<(Instant, &String)> = None;
        for (key, live) in sessions.iter() {
            if !live.shares_caller_with(&session) {
                continue;
            }
            theirs += 1;
            if live.is_idle() {
                let seen = *live.last_seen();
                if least_recent.is_none_or(|(oldest, _)| seen < oldest) {
                    least_recent = Some((seen, key));
                }
            }
        }
        if theirs >= self.limits.per_caller.get() {
            let evicted = least_recent
                .map(|(_, key)| key.clone())
                .ok_or(SessionRefusal::CallerFull)?;
            if let Some(evicted) = sessions.remove(&evicted) {
                evicted.end();
            }
        }
        if sessions.len() >= self.limits.total.get() {
            Self::drop_expired(&mut sessions, self.ttl, now);
            if sessions.len() >= self.limits.total.get() {
                return Err(SessionRefusal::ServerFull);
            }
        }
        sessions.insert(id, session);
        Ok(())
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
        Self::drop_expired(&mut self.sessions(), self.ttl, now);
    }

    /// End and forget every session idle for `ttl` by `now`.
    fn drop_expired(sessions: &mut HashMap<String, Arc<Session>>, ttl: Duration, now: Instant) {
        sessions.retain(|_, session| {
            let expired = session.is_expired(ttl, now);
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

    fn limits(total: usize, per_caller: usize) -> SessionLimits {
        SessionLimits {
            total: NonZeroUsize::new(total).expect("a non-zero limit"), // Safe: test fixture
            per_caller: NonZeroUsize::new(per_caller).expect("a non-zero limit"), // Safe: test fixture
        }
    }

    fn store(ttl: Duration) -> SessionStore {
        SessionStore::new(ttl, limits(100, 100))
    }

    /// Mint a session for `ctx`, and its id.
    fn minted(ctx: &ToolContext) -> (Arc<Session>, String) {
        let session = Session::mint(ctx).expect("randomness"); // Safe: test assertion
        let id = session.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        (session, id)
    }

    #[test]
    fn a_minted_session_is_live_only_once_inserted() {
        let store = store(Duration::from_secs(60));
        let session = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let id = session
            .id()
            .map(str::to_owned)
            .expect("an HTTP session has an id"); // Safe: test assertion
        assert!(store.find(&id).is_none());
        store.insert(session).expect("room"); // Safe: test assertion
        assert!(store.find(&id).is_some());
    }

    #[test]
    fn ending_a_session_cancels_its_requests_and_forgets_it() {
        let store = store(Duration::from_secs(60));
        let session = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let id = session.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let token = session.cancellation().child_token();
        store.insert(session).expect("room"); // Safe: test assertion
        assert!(store.end(&id));
        assert!(token.is_cancelled());
        assert!(store.find(&id).is_none());
        assert!(!store.end(&id), "a session ends once");
    }

    #[test]
    fn an_idle_session_expires_and_a_busy_one_does_not() {
        let store = store(Duration::ZERO);
        let busy = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let idle = Session::mint(&ToolContext::default()).expect("randomness"); // Safe: test assertion
        let busy_id = busy.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let idle_id = idle.id().map(str::to_owned).expect("an id"); // Safe: test assertion
        let serving = busy.enter();
        let idle_token = idle.cancellation().child_token();
        store.insert(busy).expect("room"); // Safe: test assertion
        store.insert(idle).expect("room"); // Safe: test assertion
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
        assert_eq!(session.capabilities(), DeclaredCapabilities::default());
        assert_eq!(session.log_level(), None);
        session.record_capabilities(Some(&serde_json::json!({ "sampling": {} })));
        session.set_log_level(LogLevel::Warning);
        assert_eq!(
            session.capabilities(),
            DeclaredCapabilities {
                sampling: true,
                form_elicitation: false,
            }
        );
        assert_eq!(session.log_level(), Some(LogLevel::Warning));
    }

    #[test]
    fn declared_capabilities_read_only_what_a_server_request_needs() {
        let read = |capabilities: serde_json::Value| {
            DeclaredCapabilities::from_initialize(Some(&capabilities))
        };
        assert_eq!(
            read(serde_json::json!({ "sampling": {}, "elicitation": {} })),
            DeclaredCapabilities {
                sampling: true,
                form_elicitation: true,
            }
        );
        assert!(read(serde_json::json!({ "elicitation": { "form": {} } })).form_elicitation);
        assert!(!read(serde_json::json!({ "elicitation": { "url": {} } })).form_elicitation);
        assert!(!read(serde_json::json!({ "sampling": true })).sampling);
        assert_eq!(
            read(serde_json::json!({ "padding": "x".repeat(1 << 16) })),
            DeclaredCapabilities::default(),
            "nothing the engine does not read is kept"
        );
    }

    #[test]
    fn a_caller_at_its_limit_gives_up_its_least_recently_used_idle_session() {
        let store = SessionStore::new(Duration::from_secs(60), limits(100, 3));
        let caller = ToolContext::default();
        let (oldest, oldest_id) = minted(&caller);
        let (newer, newer_id) = minted(&caller);
        let (busy, busy_id) = minted(&caller);
        let oldest_token = oldest.cancellation().child_token();
        let serving = busy.enter();
        store.insert(oldest).expect("room"); // Safe: test assertion
        store.insert(Arc::clone(&newer)).expect("room"); // Safe: test assertion
        store.insert(busy).expect("room"); // Safe: test assertion
                                           // Used after the busy one started: the oldest is the least recent.
        drop(newer.enter());

        // Another caller's sessions neither count against this one's limit
        // nor give way to it.
        let (other, other_id) = minted(&ToolContext::new().with_user("u1"));
        store.insert(other).expect("room"); // Safe: test assertion

        let (fourth, fourth_id) = minted(&caller);
        store.insert(fourth).expect("an idle one gives way"); // Safe: test assertion
        assert!(
            store.find(&oldest_id).is_none(),
            "the least recent is evicted"
        );
        assert!(oldest_token.is_cancelled(), "an evicted session ends");
        for live in [&newer_id, &busy_id, &fourth_id, &other_id] {
            assert!(store.find(live).is_some());
        }

        // Every one of the caller's sessions busy: none can give way.
        let busy_too = [&newer_id, &fourth_id].map(|id| {
            store.find(id).expect("live").enter() // Safe: test assertion
        });
        let (fifth, fifth_id) = minted(&caller);
        assert_eq!(store.insert(fifth), Err(SessionRefusal::CallerFull));
        assert!(store.find(&fifth_id).is_none());
        drop((serving, busy_too));
    }

    #[test]
    fn a_full_store_makes_room_only_from_expired_sessions() {
        let store = SessionStore::new(Duration::from_secs(60), limits(2, 2));
        let (first, first_id) = minted(&ToolContext::new().with_user("u1"));
        let (second, _) = minted(&ToolContext::new().with_user("u2"));
        store.insert(first).expect("room"); // Safe: test assertion
        store.insert(second).expect("room"); // Safe: test assertion
        let (third, third_id) = minted(&ToolContext::new().with_user("u3"));
        assert_eq!(store.insert(third), Err(SessionRefusal::ServerFull));
        assert!(store.find(&third_id).is_none());
        assert!(store.find(&first_id).is_some(), "no live session gives way");

        let expiring = SessionStore::new(Duration::ZERO, limits(1, 1));
        let (stale, stale_id) = minted(&ToolContext::new().with_user("u1"));
        expiring.insert(stale).expect("room"); // Safe: test assertion
        let (fresh, fresh_id) = minted(&ToolContext::new().with_user("u2"));
        expiring.insert(fresh).expect("the expired one gives way"); // Safe: test assertion
        assert!(!expiring.sessions().contains_key(&stale_id));
        assert!(expiring.sessions().contains_key(&fresh_id));
    }
}
