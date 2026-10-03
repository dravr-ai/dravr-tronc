// ABOUTME: In-flight request registry that lets notifications/cancelled stop a running request
// ABOUTME: Keyed by caller, session and JSON-RPC id; each entry owns the request's CancellationToken
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Request cancellation (`notifications/cancelled`).
//!
//! A client that no longer wants a request's result sends
//! `notifications/cancelled` naming the request's id. The server registers
//! every request it is serving here, keyed by who sent it and its id, and
//! hands the request's [`CancellationToken`] to the tool through
//! [`ToolContext::cancellation`]. The notification fires that token: the
//! engine stops waiting on the request and drops its future, and a tool doing
//! work outside that future (a spawned job, a blocking section) watches the
//! token itself.
//!
//! JSON-RPC ids are chosen by the client and are unique only per client, so
//! the key carries the caller identity the auth hook resolved and, over HTTP,
//! the session the request was sent in: the same [`CallerKey`] a server
//! request's answer is matched on. A notification reaches only the requests
//! of its own session, so two clients counting their ids from 0 under one
//! identity — two anonymous ones, or two sharing an API key — cannot cancel
//! each other's calls once each holds a session. Sessionless callers the auth
//! hook cannot tell apart share a key space, the same boundary [`TaskOwner`]
//! draws for tasks.
//!
//! [`TaskOwner`]: crate::mcp::tasks::TaskOwner

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::mcp::client_channel::CallerKey;
use crate::mcp::tool::ToolContext;

/// Method name of the client's cancellation notification.
pub(crate) const NOTIFICATIONS_CANCELLED: &str = "notifications/cancelled";

/// Who sent a request, in which session, and which id they gave it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RequestKey {
    caller: CallerKey,
    /// The id's JSON text, so `1` and `"1"` stay distinct as JSON-RPC requires.
    request: String,
}

impl RequestKey {
    fn new(ctx: &ToolContext, request_id: &Value) -> Self {
        let session = ctx.client.session().and_then(|session| session.id());
        Self {
            caller: CallerKey::new(ctx, session),
            request: request_id.to_string(),
        }
    }
}

/// One registered request: a serial telling apart two live requests that
/// share a key (two anonymous clients both sending id 1), and its token.
type Entry = (u64, CancellationToken);

/// The requests a server is serving right now.
#[derive(Debug, Default)]
pub(crate) struct InFlightRequests {
    next_serial: AtomicU64,
    entries: Mutex<HashMap<RequestKey, Vec<Entry>>>,
}

impl InFlightRequests {
    /// The table, recovered from a poisoned lock: every critical section is a
    /// single insert, removal or lookup, so a panic elsewhere cannot leave it
    /// half-written.
    fn entries(&self) -> MutexGuard<'_, HashMap<RequestKey, Vec<Entry>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a request for the duration of the returned guard.
    ///
    /// The request's token is a child of `ctx.cancellation`, so a transport
    /// that already cancels on its own signal (a dropped connection) still
    /// reaches the request.
    pub(crate) fn register(&self, ctx: &ToolContext, request_id: &Value) -> InFlightGuard<'_> {
        let key = RequestKey::new(ctx, request_id);
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let token = ctx.cancellation.child_token();
        self.entries()
            .entry(key.clone())
            .or_default()
            .push((serial, token.clone()));
        InFlightGuard {
            registry: self,
            key,
            serial,
            token,
        }
    }

    /// Cancel the caller's in-flight request with this id. Returns whether one
    /// was found; a request that already finished is not an error, since the
    /// notification can always cross the response in flight.
    pub(crate) fn cancel(&self, ctx: &ToolContext, request_id: &Value) -> bool {
        let key = RequestKey::new(ctx, request_id);
        self.entries().get(&key).is_some_and(|entries| {
            for (_, token) in entries {
                token.cancel();
            }
            !entries.is_empty()
        })
    }

    /// Forget one registration.
    fn release(&self, key: &RequestKey, serial: u64) {
        let mut entries = self.entries();
        if let Some(live) = entries.get_mut(key) {
            live.retain(|(entry_serial, _)| *entry_serial != serial);
            if live.is_empty() {
                entries.remove(key);
            }
        }
    }
}

/// A request's registration; dropping it — the request answered, or its
/// future dropped by the transport — unregisters the request.
#[derive(Debug)]
pub(crate) struct InFlightGuard<'a> {
    registry: &'a InFlightRequests,
    key: RequestKey,
    serial: u64,
    token: CancellationToken,
}

impl InFlightGuard<'_> {
    /// The request's cancellation token.
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.registry.release(&self.key, self.serial);
    }
}

/// Read `requestId` out of a `notifications/cancelled` payload. A string or a
/// number, per JSON-RPC; anything else names no request.
pub(crate) fn cancelled_request_id(params: Option<&Value>) -> Option<&Value> {
    params
        .and_then(|p| p.get("requestId"))
        .filter(|id| id.is_string() || id.is_number())
}
