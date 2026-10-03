// ABOUTME: A running call's channel to its client: progress, log messages, sampling and elicitation
// ABOUTME: Notifications and server-to-client requests travel the call's transport; answers route back by id
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The channel a running call has to its client.
//!
//! A `tools/call` answers with one response, but MCP lets the server talk to
//! the client while it works: report progress on the request
//! (`notifications/progress`), send log messages (`notifications/message`),
//! and — in the `initialize` era — ask the client something and wait for the
//! answer (`sampling/createMessage`, `elicitation/create`). A tool does all of
//! that through [`ToolContext::client`](crate::mcp::tool::ToolContext::client).
//!
//! The transport carries what the channel sends. Over stdio it is a line on
//! stdout like any other; over Streamable HTTP the call's `POST` is answered
//! with an event stream that carries each message as an event and the
//! response last. The client answers a server request with a JSON-RPC
//! response — a line on stdin, or a `POST` of its own — and the transport
//! routes it back here by the id the server minted, which is unguessable and
//! keyed by the caller identity (and session) the request was sent to, so no
//! other caller can answer it.
//!
//! What a channel may send depends on the call:
//!
//! - progress only on a request that carried a `progressToken`;
//! - a log message only on a server that declares the `logging` capability,
//!   at or above the level the client asked for — with `logging/setLevel` on
//!   its session, or `io.modelcontextprotocol/logLevel` in the request's
//!   `_meta` — and none when it asked for none;
//! - a request only in the `initialize` era, only to a client that declared
//!   the capability it needs in `initialize`, and only over a transport that
//!   remembers that declaration (a session). Revision 2026-07-28 forbids a
//!   server request inside a call; a tool there answers
//!   [`CallToolOutcome::InputRequired`](crate::mcp::host::CallToolOutcome::InputRequired)
//!   instead.
//!
//! A notification that may not be sent is dropped silently — progress and
//! logs are advisory — while a request that may not be sent fails with a
//! [`ClientRequestError`] saying why.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::error::INVALID_REQUEST;
use crate::mcp::cancellation::NOTIFICATIONS_CANCELLED;
use crate::mcp::elicitation::{ElicitRequest, ElicitResult, ELICITATION_CREATE};
use crate::mcp::logging::{LogLevel, LoggingMessageParams, NOTIFICATIONS_MESSAGE};
use crate::mcp::protocol::{JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse};
use crate::mcp::random_id::random_hex_id;
use crate::mcp::schema::{
    CreateMessageRequest, CreateMessageResult, ProgressNotification, ProgressToken,
};
use crate::mcp::session::Session;
use crate::mcp::tool::ToolContext;

/// `sampling/createMessage`: the server asks the client's model for a message.
pub const SAMPLING_CREATE_MESSAGE: &str = "sampling/createMessage";

/// How long a call waits for its client to answer a server request by default.
///
/// The figure the MCP SDKs use for a request in either direction; a host
/// states another with
/// [`McpServer::with_client_request_timeout`](crate::mcp::server::McpServer::with_client_request_timeout).
pub const DEFAULT_CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a server-to-client request got no answer.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClientRequestError {
    /// The call has no connection to a client: it was dispatched in-process,
    /// not through a transport.
    NoConnection,
    /// The call speaks revision 2026-07-28, which carries no server request
    /// inside a call: ask with an input-required result instead.
    StatelessRevision,
    /// The client did not declare the capability the request needs — or
    /// declared it where this server cannot see it: an `initialize` over HTTP
    /// without a session is not remembered.
    CapabilityNotDeclared {
        /// The capability, as the client would declare it.
        capability: &'static str,
    },
    /// The call was cancelled while it waited.
    Cancelled,
    /// The client did not answer in time.
    TimedOut(Duration),
    /// The connection to the client closed before it answered.
    Disconnected,
    /// The client answered with a JSON-RPC error.
    Rpc(JsonRpcError),
    /// The client's result is not the shape the method returns.
    Decode(serde_json::Error),
    /// The engine could not build the request.
    Internal(String),
}

impl fmt::Display for ClientRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConnection => f.write_str("the call has no connection to a client"),
            Self::StatelessRevision => f.write_str(
                "revision 2026-07-28 carries no server-to-client request inside a call; \
                 answer with an input-required result",
            ),
            Self::CapabilityNotDeclared { capability } => write!(
                f,
                "the client did not declare the '{capability}' capability on a session this \
                 server holds"
            ),
            Self::Cancelled => f.write_str("the call was cancelled while waiting for the client"),
            Self::TimedOut(after) => write!(f, "the client did not answer within {after:?}"),
            Self::Disconnected => f.write_str("the connection closed before the client answered"),
            Self::Rpc(error) => write!(
                f,
                "the client answered with error {}: {}",
                error.code, error.message
            ),
            Self::Decode(e) => write!(f, "the client's result has an unexpected shape: {e}"),
            Self::Internal(why) => write!(f, "the request could not be built: {why}"),
        }
    }
}

impl StdError for ClientRequestError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Decode(e) => Some(e),
            Self::NoConnection
            | Self::StatelessRevision
            | Self::CapabilityNotDeclared { .. }
            | Self::Cancelled
            | Self::TimedOut(_)
            | Self::Disconnected
            | Self::Rpc(_)
            | Self::Internal(_) => None,
        }
    }
}

/// Who a server request went to: the caller identity the auth hook resolved
/// and, over HTTP, the session. An answer counts only from the same caller.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CallerKey {
    user: Option<String>,
    tenant: Option<String>,
    session: Option<String>,
}

impl CallerKey {
    /// The key of the caller `ctx` names, in the session `session` names.
    pub(crate) fn new(ctx: &ToolContext, session: Option<&str>) -> Self {
        Self {
            user: ctx.user_id.clone(),
            tenant: ctx.tenant_id.clone(),
            session: session.map(str::to_owned),
        }
    }
}

/// One server request awaiting its answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PendingKey {
    caller: CallerKey,
    /// The id the server minted for it.
    id: String,
}

/// The server requests awaiting an answer, by caller and id.
///
/// Over HTTP one table serves the whole server, since an answer arrives on a
/// `POST` of its own; a stdio connection holds its own and closes it when
/// stdin does, failing every request still waiting.
#[derive(Debug)]
pub(crate) struct PendingRequests {
    /// `None` once closed.
    entries: Mutex<Option<HashMap<PendingKey, oneshot::Sender<JsonRpcResponse>>>>,
}

impl PendingRequests {
    /// An open table.
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Some(HashMap::new())),
        }
    }

    fn entries(
        &self,
    ) -> MutexGuard<'_, Option<HashMap<PendingKey, oneshot::Sender<JsonRpcResponse>>>> {
        // Each critical section is one insert, removal or lookup.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait for the answer to `key`; `None` when the table is closed.
    fn register(&self, key: PendingKey) -> Option<oneshot::Receiver<JsonRpcResponse>> {
        let (answer, answered) = oneshot::channel();
        self.entries().as_mut()?.insert(key, answer);
        Some(answered)
    }

    fn forget(&self, key: &PendingKey) {
        if let Some(entries) = self.entries().as_mut() {
            entries.remove(key);
        }
    }

    /// Hand `response` to the request it answers, when `caller` sent one with
    /// its id that is still waiting. Returns whether one was.
    pub(crate) fn deliver(&self, caller: &CallerKey, response: JsonRpcResponse) -> bool {
        let Some(Value::String(id)) = &response.id else {
            return false;
        };
        let key = PendingKey {
            caller: caller.clone(),
            id: id.clone(),
        };
        let waiting = self.entries().as_mut().and_then(|e| e.remove(&key));
        waiting.is_some_and(|answer| answer.send(response).is_ok())
    }

    /// Close the table: every request still waiting fails as disconnected,
    /// and none can wait from now on.
    pub(crate) fn close(&self) {
        self.entries().take();
    }
}

/// What a transport gives a call to reach its client: where messages go, where
/// answers come back, and the session the call runs in.
#[derive(Clone)]
pub(crate) struct ClientConnection {
    outbound: mpsc::UnboundedSender<JsonRpcMessage>,
    pending: Arc<PendingRequests>,
    session: Option<Arc<Session>>,
    caller: CallerKey,
    request_timeout: Duration,
}

impl ClientConnection {
    /// A connection sending on `outbound`, awaiting answers in `pending`, for
    /// the caller `ctx` in `session`.
    pub(crate) fn new(
        outbound: mpsc::UnboundedSender<JsonRpcMessage>,
        pending: Arc<PendingRequests>,
        session: Option<Arc<Session>>,
        ctx: &ToolContext,
        request_timeout: Duration,
    ) -> Self {
        let caller = CallerKey::new(ctx, session.as_ref().and_then(|s| s.id()));
        Self {
            outbound,
            pending,
            session,
            caller,
            request_timeout,
        }
    }

    fn send(&self, message: JsonRpcRequest) -> bool {
        self.outbound.send(JsonRpcMessage::Request(message)).is_ok()
    }
}

/// The protocol era of the call a channel serves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Era {
    /// Not bound to a call by the engine.
    #[default]
    Unbound,
    /// An `initialize`-era call.
    Legacy,
    /// A revision 2026-07-28 call, with the log level its `_meta` asks for.
    Modern { log_level: Option<LogLevel> },
}

/// What the engine knows about the call a channel serves.
#[derive(Debug, Clone, Default)]
struct CallScope {
    era: Era,
    progress_token: Option<ProgressToken>,
    /// Whether the server declares the `logging` capability.
    logging: bool,
    cancellation: CancellationToken,
}

/// A running call's channel to its client; see the [module docs](self).
///
/// Cheap to clone. The default channel reaches no client: its notifications
/// are dropped and its requests fail with [`ClientRequestError::NoConnection`],
/// which is what a call dispatched in-process gets.
#[derive(Clone, Default)]
pub struct ClientChannel {
    connection: Option<ClientConnection>,
    scope: CallScope,
}

impl fmt::Debug for ClientChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("connected", &self.connection.is_some())
            .field("era", &self.scope.era)
            .field("progress_token", &self.scope.progress_token)
            .finish_non_exhaustive()
    }
}

/// What a server request needs the client to have declared.
#[derive(Debug, Clone, Copy)]
enum Needs {
    Sampling,
    FormElicitation,
}

impl Needs {
    const fn capability(self) -> &'static str {
        match self {
            Self::Sampling => "sampling",
            Self::FormElicitation => "elicitation",
        }
    }

    /// Whether `capabilities`, as declared in `initialize`, covers it.
    ///
    /// Form elicitation is declared as `elicitation: {}` — what every client
    /// before URL mode sends — or with a `form` member.
    fn declared_in(self, capabilities: &Value) -> bool {
        match self {
            Self::Sampling => capabilities.get("sampling").is_some_and(Value::is_object),
            Self::FormElicitation => capabilities
                .get("elicitation")
                .and_then(Value::as_object)
                .is_some_and(|elicitation| {
                    elicitation.is_empty() || elicitation.contains_key("form")
                }),
        }
    }
}

impl ClientChannel {
    /// A channel over `connection`, not yet bound to a call.
    pub(crate) fn connected(connection: ClientConnection) -> Self {
        Self {
            connection: Some(connection),
            scope: CallScope::default(),
        }
    }

    /// This channel bound to an `initialize`-era call.
    pub(crate) fn for_legacy_call(
        &self,
        progress_token: Option<ProgressToken>,
        logging: bool,
        cancellation: CancellationToken,
    ) -> Self {
        self.bound(Era::Legacy, progress_token, logging, cancellation)
    }

    /// This channel bound to a revision 2026-07-28 call asking for logs at
    /// `log_level` and above.
    pub(crate) fn for_modern_call(
        &self,
        progress_token: Option<ProgressToken>,
        log_level: Option<LogLevel>,
        logging: bool,
        cancellation: CancellationToken,
    ) -> Self {
        self.bound(
            Era::Modern { log_level },
            progress_token,
            logging,
            cancellation,
        )
    }

    fn bound(
        &self,
        era: Era,
        progress_token: Option<ProgressToken>,
        logging: bool,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            connection: self.connection.clone(),
            scope: CallScope {
                era,
                progress_token,
                logging,
                cancellation,
            },
        }
    }

    /// The session the call runs in, if its transport keeps one.
    pub(crate) fn session(&self) -> Option<&Arc<Session>> {
        self.connection.as_ref()?.session.as_ref()
    }

    /// The token the client asked progress on this call to be reported under.
    #[must_use]
    pub fn progress_token(&self) -> Option<&ProgressToken> {
        self.scope.progress_token.as_ref()
    }

    /// Report progress on the call: `progress` so far, out of `total` when
    /// known, with an optional human-readable `message`.
    ///
    /// Sent only when the request carried a `progressToken`; dropped
    /// otherwise. `progress` must grow with every report, as the
    /// specification requires.
    pub fn progress(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        let (Some(connection), Some(token)) = (&self.connection, &self.scope.progress_token) else {
            return;
        };
        let notification =
            ProgressNotification::new(token.clone(), progress, total, message.map(str::to_owned));
        if let Some(message) = notification_of(&notification.method, &notification.params) {
            connection.send(message);
        }
    }

    /// The least severe level the client wants log messages at on this call,
    /// or `None` when it wants none (or the server declares no `logging`).
    #[must_use]
    pub fn log_level(&self) -> Option<LogLevel> {
        if !self.scope.logging {
            return None;
        }
        match self.scope.era {
            Era::Unbound => None,
            Era::Legacy => self.session().and_then(|s| s.log_level()),
            Era::Modern { log_level } => log_level,
        }
    }

    /// Whether a message at `level` would be sent; lets a tool skip building
    /// data no one will read.
    #[must_use]
    pub fn wants_log(&self, level: LogLevel) -> bool {
        self.connection.is_some() && self.log_level().is_some_and(|minimum| level >= minimum)
    }

    /// Send a log message at `level`, from the logger named `logger` when
    /// given, carrying `data`. Dropped unless [`Self::wants_log`].
    pub fn log(&self, level: LogLevel, logger: Option<&str>, data: Value) {
        if !self.wants_log(level) {
            return;
        }
        let params = LoggingMessageParams {
            level,
            logger: logger.map(str::to_owned),
            data,
        };
        if let (Some(connection), Some(message)) = (
            &self.connection,
            notification_of(NOTIFICATIONS_MESSAGE, &params),
        ) {
            connection.send(message);
        }
    }

    /// Ask the client's model for a message (`sampling/createMessage`) and
    /// wait for it.
    ///
    /// # Errors
    ///
    /// A [`ClientRequestError`]: the request cannot be sent on this call, or
    /// the client did not answer it with a result.
    pub async fn create_message(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<CreateMessageResult, ClientRequestError> {
        self.request(SAMPLING_CREATE_MESSAGE, request, Needs::Sampling)
            .await
    }

    /// Ask the person behind the client to fill in a form
    /// (`elicitation/create`) and wait for what they did.
    ///
    /// # Errors
    ///
    /// A [`ClientRequestError`]: the request cannot be sent on this call, or
    /// the client did not answer it with a result.
    pub async fn elicit(
        &self,
        request: &ElicitRequest,
    ) -> Result<ElicitResult, ClientRequestError> {
        self.request(ELICITATION_CREATE, request, Needs::FormElicitation)
            .await
    }

    /// Send the server request `method` with `params` and wait for its result.
    async fn request<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        needs: Needs,
    ) -> Result<R, ClientRequestError> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(ClientRequestError::NoConnection)?;
        match self.scope.era {
            Era::Unbound => return Err(ClientRequestError::NoConnection),
            Era::Modern { .. } => return Err(ClientRequestError::StatelessRevision),
            Era::Legacy => {}
        }
        let declared = connection
            .session
            .as_ref()
            .and_then(|session| session.capabilities())
            .is_some_and(|capabilities| needs.declared_in(&capabilities));
        if !declared {
            return Err(ClientRequestError::CapabilityNotDeclared {
                capability: needs.capability(),
            });
        }

        let params = serde_json::to_value(params)
            .map_err(|e| ClientRequestError::Internal(e.to_string()))?;
        let id = random_hex_id().map_err(|e| {
            ClientRequestError::Internal(format!("no OS randomness for an id: {e}"))
        })?;
        let key = PendingKey {
            caller: connection.caller.clone(),
            id: id.clone(),
        };
        let answered = connection
            .pending
            .register(key.clone())
            .ok_or(ClientRequestError::Disconnected)?;
        let mut waiting = Waiting {
            connection,
            key,
            outcome: Abandoned::Dropped,
        };
        if !connection.send(JsonRpcRequest::with_id(
            method,
            Some(params),
            Value::String(id),
        )) {
            waiting.outcome = Abandoned::Answered;
            return Err(ClientRequestError::Disconnected);
        }

        let answer = tokio::select! {
            biased;
            () = self.scope.cancellation.cancelled() => {
                waiting.outcome = Abandoned::Cancelled;
                return Err(ClientRequestError::Cancelled);
            }
            answer = timeout(connection.request_timeout, answered) => answer,
        };
        let response = match answer {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                // The table was closed under it: the client is gone.
                waiting.outcome = Abandoned::Answered;
                return Err(ClientRequestError::Disconnected);
            }
            Err(_) => {
                waiting.outcome = Abandoned::TimedOut;
                return Err(ClientRequestError::TimedOut(connection.request_timeout));
            }
        };
        waiting.outcome = Abandoned::Answered;
        match (response.result, response.error) {
            (_, Some(error)) => Err(ClientRequestError::Rpc(error)),
            (Some(result), None) => {
                serde_json::from_value(result).map_err(ClientRequestError::Decode)
            }
            (None, None) => Err(ClientRequestError::Rpc(JsonRpcError::new(
                INVALID_REQUEST,
                "the client's response carries neither a result nor an error",
            ))),
        }
    }
}

/// The notification `method` with `params`, or `None` (logged) when the
/// params do not serialize.
fn notification_of<P: Serialize>(method: &str, params: &P) -> Option<JsonRpcRequest> {
    match serde_json::to_value(params) {
        Ok(params) => Some(JsonRpcRequest::notification(method, Some(params))),
        Err(e) => {
            debug!(method, error = %e, "Dropping a notification whose params do not serialize");
            None
        }
    }
}

/// Why a server request stopped waiting, as its cancellation notice says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandoned {
    /// Settled: answered, or never reached the client — nothing to cancel.
    Answered,
    /// The waiting future was dropped: the call ended under it.
    Dropped,
    /// The call was cancelled.
    Cancelled,
    /// The client took too long.
    TimedOut,
}

/// A server request in flight: dropping it forgets the request and, unless it
/// was settled, tells the client the server no longer wants the answer.
struct Waiting<'c> {
    connection: &'c ClientConnection,
    key: PendingKey,
    outcome: Abandoned,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.connection.pending.forget(&self.key);
        let reason = match self.outcome {
            Abandoned::Answered => return,
            Abandoned::Dropped => "The call that sent this request ended",
            Abandoned::Cancelled => "The call that sent this request was cancelled",
            Abandoned::TimedOut => "The server stopped waiting for an answer",
        };
        let notice = JsonRpcRequest::notification(
            NOTIFICATIONS_CANCELLED,
            Some(serde_json::json!({ "requestId": self.key.id, "reason": reason })),
        );
        self.connection.send(notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::elicitation::ElicitationSchema;
    use futures::executor::block_on;
    use serde_json::json;
    use tokio::task::yield_now;

    /// A legacy-era channel over a fresh connection whose session declared
    /// `capabilities`, and the receiving end of what it sends.
    fn legacy_channel(
        capabilities: Option<Value>,
        progress_token: Option<ProgressToken>,
    ) -> (
        ClientChannel,
        mpsc::UnboundedReceiver<JsonRpcMessage>,
        Arc<PendingRequests>,
    ) {
        let (outbound, inbox) = mpsc::unbounded_channel();
        let pending = Arc::new(PendingRequests::new());
        let ctx = ToolContext::default();
        let session = Session::connection();
        session.record_capabilities(capabilities);
        let connection = ClientConnection::new(
            outbound,
            Arc::clone(&pending),
            Some(session),
            &ctx,
            Duration::from_secs(5),
        );
        let channel = ClientChannel::connected(connection).for_legacy_call(
            progress_token,
            true,
            CancellationToken::new(),
        );
        (channel, inbox, pending)
    }

    fn sent(inbox: &mut mpsc::UnboundedReceiver<JsonRpcMessage>) -> Value {
        let request = match inbox.try_recv() {
            Ok(JsonRpcMessage::Request(request)) => Some(request),
            _ => None,
        };
        let request = request.expect("a request or notification was sent"); // Safe: test assertion
        serde_json::to_value(request).expect("serialize") // Safe: test assertion
    }

    fn sampling_request() -> CreateMessageRequest {
        serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": { "type": "text", "text": "hello" } }],
            "maxTokens": 10
        }))
        .expect("a sampling request") // Safe: test assertion
    }

    #[test]
    fn progress_needs_a_token() {
        let (silent, mut inbox, _) = legacy_channel(None, None);
        silent.progress(1.0, None, None);
        assert!(inbox.try_recv().is_err());

        let (channel, mut inbox, _) = legacy_channel(None, Some(ProgressToken::from("t1")));
        channel.progress(50.0, Some(100.0), Some("half"));
        assert_eq!(
            sent(&mut inbox),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": { "progressToken": "t1", "progress": 50.0, "total": 100.0, "message": "half" }
            })
        );
    }

    #[test]
    fn logs_follow_the_session_level_and_none_without_one() {
        let (channel, mut inbox, _) = legacy_channel(None, None);
        channel.log(LogLevel::Emergency, None, json!("unasked"));
        assert!(inbox.try_recv().is_err(), "no level asked, no logs");

        channel
            .session()
            .expect("a session") // Safe: test assertion
            .set_log_level(LogLevel::Warning);
        channel.log(LogLevel::Info, None, json!("too quiet"));
        assert!(inbox.try_recv().is_err());
        channel.log(LogLevel::Error, Some("db"), json!({ "rows": 0 }));
        assert_eq!(
            sent(&mut inbox),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": { "level": "error", "logger": "db", "data": { "rows": 0 } }
            })
        );
    }

    #[test]
    fn a_modern_call_logs_at_its_meta_level_and_never_requests() {
        let (outbound, mut inbox) = mpsc::unbounded_channel();
        let ctx = ToolContext::default();
        let connection = ClientConnection::new(
            outbound,
            Arc::new(PendingRequests::new()),
            None,
            &ctx,
            Duration::from_secs(5),
        );
        let channel = ClientChannel::connected(connection).for_modern_call(
            None,
            Some(LogLevel::Info),
            true,
            CancellationToken::new(),
        );
        channel.log(LogLevel::Debug, None, json!("below"));
        assert!(inbox.try_recv().is_err());
        channel.log(LogLevel::Info, None, json!("at"));
        assert_eq!(sent(&mut inbox)["params"]["data"], "at");

        let refused = block_on(channel.create_message(&sampling_request()));
        assert!(matches!(
            refused,
            Err(ClientRequestError::StatelessRevision)
        ));
    }

    #[test]
    fn a_server_without_the_logging_capability_sends_no_logs() {
        let (outbound, mut inbox) = mpsc::unbounded_channel();
        let ctx = ToolContext::default();
        let connection = ClientConnection::new(
            outbound,
            Arc::new(PendingRequests::new()),
            None,
            &ctx,
            Duration::from_secs(5),
        );
        let channel = ClientChannel::connected(connection).for_modern_call(
            None,
            Some(LogLevel::Debug),
            false,
            CancellationToken::new(),
        );
        channel.log(LogLevel::Emergency, None, json!("undeclared"));
        assert!(inbox.try_recv().is_err());
    }

    #[tokio::test]
    async fn requests_need_a_connection_and_the_declared_capability() {
        let unconnected = ClientChannel::default();
        assert!(matches!(
            unconnected.create_message(&sampling_request()).await,
            Err(ClientRequestError::NoConnection)
        ));

        let (channel, mut inbox, _) = legacy_channel(Some(json!({ "elicitation": {} })), None);
        assert!(matches!(
            channel.create_message(&sampling_request()).await,
            Err(ClientRequestError::CapabilityNotDeclared {
                capability: "sampling"
            })
        ));
        assert!(inbox.try_recv().is_err(), "a refused request is never sent");

        let (url_only, _, _) = legacy_channel(Some(json!({ "elicitation": { "url": {} } })), None);
        let form = ElicitRequest {
            message: "who?".to_owned(),
            requested_schema: ElicitationSchema::default(),
        };
        assert!(matches!(
            url_only.elicit(&form).await,
            Err(ClientRequestError::CapabilityNotDeclared {
                capability: "elicitation"
            })
        ));
    }

    #[tokio::test]
    async fn an_answer_reaches_the_request_it_names() {
        let (channel, mut inbox, pending) = legacy_channel(Some(json!({ "sampling": {} })), None);
        let call = tokio::spawn(async move { channel.create_message(&sampling_request()).await });

        let request = loop {
            if let Ok(JsonRpcMessage::Request(request)) = inbox.try_recv() {
                break request;
            }
            yield_now().await;
        };
        assert_eq!(request.method, SAMPLING_CREATE_MESSAGE);
        assert_eq!(
            request.params.as_ref().map(|p| &p["maxTokens"]),
            Some(&json!(10))
        );
        let id = request.id.expect("a request has an id"); // Safe: test assertion

        let caller = CallerKey::new(&ToolContext::default(), None);
        let stranger = CallerKey::new(&ToolContext::new().with_user("mallory"), None);
        let answer = JsonRpcResponse::success(
            Some(id),
            json!({ "role": "assistant", "content": { "type": "text", "text": "hi" },
                    "model": "m", "stopReason": "endTurn" }),
        );
        assert!(
            !pending.deliver(&stranger, answer.clone()),
            "another caller cannot answer it"
        );
        assert!(pending.deliver(&caller, answer));

        let result = call
            .await
            .expect("the call ran") // Safe: test assertion
            .expect("the client answered"); // Safe: test assertion
        assert_eq!(result.content.text, "hi");
    }

    #[tokio::test]
    async fn a_cancelled_call_drops_its_request_and_tells_the_client() {
        let (outbound, mut inbox) = mpsc::unbounded_channel();
        let pending = Arc::new(PendingRequests::new());
        let ctx = ToolContext::default();
        let session = Session::connection();
        session.record_capabilities(Some(json!({ "sampling": {} })));
        let connection = ClientConnection::new(
            outbound,
            Arc::clone(&pending),
            Some(session),
            &ctx,
            Duration::from_secs(5),
        );
        let cancel = CancellationToken::new();
        let channel =
            ClientChannel::connected(connection).for_legacy_call(None, true, cancel.clone());
        let call = tokio::spawn(async move { channel.create_message(&sampling_request()).await });

        let id = loop {
            if let Ok(JsonRpcMessage::Request(request)) = inbox.try_recv() {
                break request.id.expect("an id"); // Safe: test assertion
            }
            yield_now().await;
        };
        cancel.cancel();
        assert!(matches!(
            call.await.expect("the call ran"), // Safe: test assertion
            Err(ClientRequestError::Cancelled)
        ));
        let notice = sent(&mut inbox);
        assert_eq!(notice["method"], "notifications/cancelled");
        assert_eq!(notice["params"]["requestId"], id);

        // The late answer finds nothing waiting.
        let late = JsonRpcResponse::success(Some(id), json!({}));
        assert!(!pending.deliver(&CallerKey::new(&ctx, None), late));
    }

    #[tokio::test]
    async fn a_closed_table_fails_the_waiting_request() {
        let (channel, mut inbox, pending) = legacy_channel(Some(json!({ "sampling": {} })), None);
        let call = tokio::spawn(async move { channel.create_message(&sampling_request()).await });
        while inbox.try_recv().is_err() {
            yield_now().await;
        }
        pending.close();
        assert!(matches!(
            call.await.expect("the call ran"), // Safe: test assertion
            Err(ClientRequestError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn an_unanswered_request_times_out() {
        let (outbound, _inbox) = mpsc::unbounded_channel();
        let ctx = ToolContext::default();
        let session = Session::connection();
        session.record_capabilities(Some(json!({ "sampling": {} })));
        let connection = ClientConnection::new(
            outbound,
            Arc::new(PendingRequests::new()),
            Some(session),
            &ctx,
            Duration::from_millis(20),
        );
        let channel = ClientChannel::connected(connection).for_legacy_call(
            None,
            true,
            CancellationToken::new(),
        );
        assert!(matches!(
            channel.create_message(&sampling_request()).await,
            Err(ClientRequestError::TimedOut(_))
        ));
    }
}
