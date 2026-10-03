// ABOUTME: HTTP transport implementing MCP Streamable HTTP with JSON and SSE responses
// ABOUTME: Serves a POST /mcp endpoint that accepts JSON-RPC and responds via JSON or event stream
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::error::Error;
use std::future::{pending, Future};
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::rejection::StringRejection;
use axum::extract::State;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::uri::Authority;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{stream, FutureExt, Stream};
use serde_json::Value;
use tokio::net::{lookup_host, TcpListener};
use tokio::signal;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::error::{
    HEADER_MISMATCH, INTERNAL_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND,
    MISSING_REQUIRED_CLIENT_CAPABILITY, PARSE_ERROR, RATE_LIMITED, UNAUTHORIZED,
    UNSUPPORTED_PROTOCOL_VERSION,
};
use crate::mcp::auth::AuthError;
use crate::mcp::client_channel::ClientChannel;
use crate::mcp::modern::{
    is_modern_revision, ModernMeta, ModernRequestMeta, PROTOCOL_VERSION_HEADER,
};
use crate::mcp::protocol::{
    JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION, PROTOCOL_VERSION,
};
use crate::mcp::resource_metadata::{ProtectedResourceMetadata, WELL_KNOWN_PROTECTED_RESOURCE};
use crate::mcp::server::McpServer;
use crate::mcp::session::{Session, SessionRefusal, SessionStore, SessionUse};
use crate::mcp::tool::ToolContext;
use crate::mcp::transport::mirror::{check_response_headers, check_standard_headers};
use crate::server::auth::{bearer_credential, is_loopback_host, InsecureBindError};
use crate::server::request_guard::guard_requests;

/// `Mcp-Session-Id`: the session a request runs in, minted by `initialize` on
/// a server with [`McpServer::with_http_sessions`] (revision 2025-11-25).
pub const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// Build an Axum router with the `/mcp` POST endpoint
///
/// Returns a `Router` that can be merged into a larger application router
/// or served standalone. The router is parameterized over the MCP server's
/// state type.
///
/// It carries no request guard of its own: the application that merges it
/// layers [`guard_requests`] once over the whole router, and [`serve`] does so
/// for the standalone case. Two guards on one route would log it twice. Never
/// layer a request deadline over it — a tool call is dispatched whole before
/// the response is written.
///
/// A body over [`McpServer::max_request_bytes`] is refused with 413 before it
/// is buffered whole; the limit is set on the route itself, so it holds
/// whatever the merging application layers on top.
///
/// When the server publishes [`ProtectedResourceMetadata`] (see
/// [`McpServer::with_protected_resource_metadata`]), the router also answers
/// `GET` with it at the document's well-known path, and at the bare
/// `/.well-known/oauth-protected-resource` an MCP client falls back to. When
/// it keeps sessions ([`McpServer::with_http_sessions`]), `/mcp` also answers
/// `DELETE`, which ends one.
pub fn mcp_router<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Router {
    let body_limit = DefaultBodyLimit::max(server.max_request_bytes());
    let metadata = server.protected_resource_metadata().cloned();
    let mut endpoint = post(handle_mcp_post::<S>).layer(body_limit);
    if server.sessions().is_some() {
        endpoint = endpoint.delete(handle_mcp_delete::<S>);
    }
    let router = Router::new().route("/mcp", endpoint).with_state(server);
    match metadata {
        Some(metadata) => router.merge(resource_metadata_router(metadata)),
        None => router,
    }
}

/// The routes publishing `metadata`: its RFC 9728 well-known path and, when
/// that carries the resource's path, the bare one too.
fn resource_metadata_router(metadata: Arc<ProtectedResourceMetadata>) -> Router {
    let path = metadata.metadata_path();
    let serve_metadata = get(serve_resource_metadata).with_state(metadata);
    let router = Router::new().route(&path, serve_metadata.clone());
    if path == WELL_KNOWN_PROTECTED_RESOURCE {
        router
    } else {
        router.route(WELL_KNOWN_PROTECTED_RESOURCE, serve_metadata)
    }
}

/// Answer the metadata document. It is public by design — a client reads it
/// before it holds any credential, often from a browser — so it carries
/// `Access-Control-Allow-Origin: *` and passes no Origin or Host gate.
async fn serve_resource_metadata(
    State(metadata): State<Arc<ProtectedResourceMetadata>>,
) -> Response {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(metadata.as_ref()),
    )
        .into_response()
}

/// [`mcp_router`] under [`guard_requests`]: the router [`serve`] serves, less
/// the loopback `Host` check it adds on a loopback bind.
///
/// Public so a server bound some other way — the testkit's port-0 server, an
/// in-process test client — answers as the standalone one does.
pub fn guarded_mcp_router<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Router {
    mcp_router(server).layer(from_fn(guard_requests))
}

/// Start a standalone HTTP server serving only the `/mcp` endpoint
///
/// Binds to the given host and port, serves until shutdown. Every request goes
/// through [`guard_requests`]: it gets a request id, a completion log line, and
/// a JSON `500` if a tool handler panics, instead of a dropped connection.
///
/// **A server with no [`AuthHook`](crate::mcp::auth::AuthHook) serves loopback
/// only.** This router is all it serves, so the hook is the only thing that can
/// authenticate a request here; without one, every caller that can reach the
/// socket can call every tool. `host` is resolved first and the bind is refused
/// with an [`InsecureBindError`] — before any socket opens — unless every
/// address it resolves to is loopback. That judges what would actually be
/// bound, not the name: `localhost` passes because it resolves to `127.0.0.1`
/// or `::1`, and would be refused on a machine whose resolver says otherwise.
/// Attach a hook ([`ApiKeyAuthHook`](crate::mcp::auth::ApiKeyAuthHook) for a
/// shared key) to serve a reachable interface.
///
/// **A loopback bind serves a loopback `Host` only**, unless the server names
/// its own list with [`McpServer::with_allowed_hosts`]. A page that rebinds
/// its name to 127.0.0.1 reaches a loopback server with that name in `Host`,
/// so this is what stops it where the `Origin` gate cannot.
///
/// It shuts down gracefully on [`shutdown_signal`] (SIGINT, or SIGTERM on
/// Unix): it stops accepting, lets every request in flight finish, then
/// returns `Ok`. [`serve_with_shutdown`] takes the trigger from the caller.
///
/// # Errors
///
/// An [`InsecureBindError`] for a reachable bind with no hook; otherwise a
/// resolution, bind or serve failure.
pub async fn serve<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    serve_with_shutdown(server, host, port, shutdown_signal()).await
}

/// [`serve`], shutting down gracefully when `shutdown` resolves.
///
/// Once it does, no new connection is accepted, every request in flight is
/// answered, and the call returns `Ok` once the last one is.
///
/// For a binary that owns its own shutdown sequence (draining a queue,
/// flushing telemetry) and triggers the transport's part of it itself.
///
/// # Errors
///
/// As [`serve`].
pub async fn serve_with_shutdown<S, F>(
    server: Arc<McpServer<S>>,
    host: &str,
    port: u16,
    shutdown: F,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    S: Send + Sync + ?Sized + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let authenticated = server.has_auth_hook();
    let addr = format!("{host}:{port}");
    let resolved: Vec<SocketAddr> = lookup_host(&addr)
        .await
        .map_err(|e| format!("Failed to resolve {addr}: {e}"))?
        .collect();
    if !authenticated && !resolved.iter().all(|a| a.ip().is_loopback()) {
        return Err(Box::new(InsecureBindError::new(host)));
    }

    // A loopback bind is the one DNS rebinding reaches, so unless the host
    // named its own list, only a loopback `Host` is served there.
    let loopback = resolved.iter().all(|a| a.ip().is_loopback());
    let host_guarded = loopback && server.allowed_hosts().is_none();
    let mut app = mcp_router(server);
    if host_guarded {
        app = app.layer(from_fn(require_loopback_host));
    }
    let app = app.layer(from_fn(guard_requests));
    let listener = TcpListener::bind(resolved.as_slice())
        .await
        .map_err(|e| format!("Failed to bind {addr}: {e}"))?;

    info!(
        address = %addr,
        authenticated,
        protocol_version = PROTOCOL_VERSION,
        "HTTP MCP transport listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| format!("HTTP server error: {e}"))?;

    info!(address = %addr, "HTTP MCP transport stopped");
    Ok(())
}

/// Resolve when the process is asked to stop: SIGINT (Ctrl-C) or, on Unix,
/// SIGTERM — what Cloud Run and Kubernetes send before they kill a container.
///
/// [`serve`] shuts down on it; a binary serving its own router passes it to
/// `axum::serve(..).with_graceful_shutdown`. A signal whose handler cannot be
/// installed is logged and never resolves, leaving the other to stop the
/// process.
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Err(e) = signal::ctrl_c().await {
            error!(error = %e, "Cannot listen for SIGINT; it will not stop the server gracefully");
            pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(e) => {
                error!(error = %e, "Cannot listen for SIGTERM; it will not stop the server gracefully");
                pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = pending::<()>();

    tokio::select! {
        () = interrupt => info!("SIGINT received, shutting down"),
        () = terminate => info!("SIGTERM received, shutting down"),
    }
}

/// Handle an incoming MCP POST request
///
/// Enforces the `Origin` allowlist (403) and, when the server names one, the
/// `Host` allowlist (403; see [`McpServer::with_allowed_hosts`]), refuses a body not declared
/// `application/json` (415), a body over the size limit (413), and a client that does not accept both
/// `application/json` and `text/event-stream` (406), refuses a body that is
/// not a JSON-RPC message (400), refuses an unsupported `MCP-Protocol-Version`
/// (400, -32022) and a modern body sent without one (400, -32020), resolves the
/// request's `Mcp-Session-Id` on a server that keeps sessions (404 for one that
/// is not live, or belongs to another caller), authenticates via the server's hook (401 +
/// `WWW-Authenticate` on rejection, per RFC 9728; 429 + `Retry-After` on a
/// spent budget; 500 when the host failed to decide), then dispatches under
/// the resolved per-call context.
///
/// A request's response is rendered as JSON or a single SSE event, whichever
/// the client's `Accept` weighs higher — unless the call talks to its client
/// before it answers (progress, log messages, a request of its own; see
/// [`ClientChannel`]): then the
/// answer is an event stream carrying each of those as an event, and the
/// response last. An accepted notification is answered 202 Accepted with no
/// body, and so is a client's response to a server request, which is routed
/// to the call waiting for it when it comes from the caller (and session) the
/// request was sent to.
///
/// On a server with [`McpServer::with_http_sessions`], a successful
/// `initialize` is answered with a fresh `Mcp-Session-Id`.
pub async fn handle_mcp_post<S: Send + Sync + ?Sized + 'static>(
    State(server): State<Arc<McpServer<S>>>,
    uri: Uri,
    headers: HeaderMap,
    body: Result<String, StringRejection>,
) -> Response {
    // 1. Origin allowlist and, when the host names one, the Host allowlist
    // (DNS-rebinding protection).
    if let Err(refusal) = check_origin_and_host(&server, &headers, &uri) {
        return *refusal;
    }

    // 2. Media types. A body that is not `application/json` is refused before
    // it is parsed: a browser sends `text/plain` and form bodies cross-origin
    // without a CORS preflight, so admitting them would let a page reach the
    // tools without the CORS policy ever being consulted. And the client must
    // accept both answers this endpoint may give.
    if !is_json_content_type(&headers) {
        return transport_refusal(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            INVALID_REQUEST,
            "Unsupported Media Type: Content-Type must be application/json",
        );
    }
    if !accepts_json_and_event_stream(&headers) {
        return transport_refusal(
            StatusCode::NOT_ACCEPTABLE,
            INVALID_REQUEST,
            "Not Acceptable: Accept must list application/json and text/event-stream",
        );
    }

    // 3. Read the body. One over the limit is a 413, one that is not UTF-8
    // cannot be JSON; neither reached the parser, so neither has an id.
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return transport_refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                INVALID_REQUEST,
                &format!("Request body exceeds {} bytes", server.max_request_bytes()),
            );
        }
        Err(rejection) => {
            return transport_refusal(rejection.status(), PARSE_ERROR, &rejection.body_text());
        }
    };

    // Parse the JSON-RPC envelope. A body that is not a message — not JSON,
    // a batch, an id that is neither a string nor an integer — is one this
    // server cannot accept, which Streamable HTTP answers with an HTTP error
    // status, never a 2xx. A client's response to a request the server sent
    // it takes its own path.
    let request = match JsonRpcMessage::parse(&body) {
        Ok(JsonRpcMessage::Request(request)) => request,
        Ok(JsonRpcMessage::Response(response)) => {
            return accept_client_response(&server, &headers, response).await;
        }
        Err(refusal) => return (StatusCode::BAD_REQUEST, Json(*refusal)).into_response(),
    };
    serve_request(server, &headers, request).await
}

/// The `Origin` gate and, when the server names a host list, the `Host` gate:
/// `Err` carries the 403 (or 400) to answer.
fn check_origin_and_host<S: Send + Sync + ?Sized + 'static>(
    server: &McpServer<S>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<(), Refusal> {
    if !origin_allowed(headers, server.allowed_origins()) {
        debug!(origin = ?headers.get(header::ORIGIN), "Rejected MCP request: origin not allowed");
        return Err(Box::new(
            (StatusCode::FORBIDDEN, "Origin not allowed").into_response(),
        ));
    }
    if let Some(allowed) = server.allowed_hosts() {
        check_host(headers, uri, allowed).map_err(|refusal| Box::new(refusal.into_response()))?;
    }
    Ok(())
}

/// A request refused before dispatch: the answer to send, boxed so the
/// `Result` carrying it stays small.
type Refusal = Box<Response>;

/// The request's `MCP-Protocol-Version`, judged: `Ok(None)` when it carries
/// none, the version when this server accepts it, and the 400 to answer
/// otherwise (-32020 for a repeated or unreadable header, -32022 for a
/// revision the server does not speak), echoing `id`.
fn accepted_protocol_version<'h, S: Send + Sync + ?Sized + 'static>(
    server: &McpServer<S>,
    headers: &'h HeaderMap,
    id: Option<&Value>,
) -> Result<Option<&'h str>, Refusal> {
    let version = protocol_version_header(headers)
        .map_err(|reason| Box::new(header_mismatch(id.cloned(), &reason)))?;
    match version {
        Some(version) if !server.accepts_protocol_version(version) => Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(JsonRpcResponse::error_with_data(
                    id.cloned(),
                    UNSUPPORTED_PROTOCOL_VERSION,
                    "Unsupported protocol version".to_owned(),
                    serde_json::json!({
                        "supported": server.advertised_protocol_versions(),
                        "requested": version,
                    }),
                )),
            )
                .into_response(),
        )),
        version => Ok(version),
    }
}

/// Serve one JSON-RPC request (or notification) sent to `/mcp`.
async fn serve_request<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    headers: &HeaderMap,
    mut request: JsonRpcRequest,
) -> Response {
    // 4. Populate transport-derived fields for the auth hook. The credential
    // comes from the `Authorization` header only; `parse` never reads one out
    // of the body.
    request.auth_token = bearer_token(headers);
    // Every other header reaches the hook too, so a host can tell which of its
    // public origins the client dialed — an RFC 9728 challenge must name that
    // one — without the transport knowing what a host reads.
    request.headers = forwarded_headers(headers);
    // `MCP-Protocol-Version` names the revision the request speaks. On a
    // stateless server there is no session to check it against, so this is
    // the only place an unsupported one can be refused (-32022) rather than
    // served as if we spoke it. A request with no header is an
    // `initialize`-era one (a pre-2025-06-18 client sends none); a body
    // carrying modern `_meta` without it is refused, since every modern POST
    // must carry the header. Era detection reads the header back from the
    // metadata and holds it to the body's `_meta` (-32020 on disagreement).
    let version = match accepted_protocol_version(&server, headers, request.id.as_ref()) {
        Ok(version) => version,
        Err(refusal) => return *refusal,
    };
    if let Some(version) = version {
        request = request.with_metadata(PROTOCOL_VERSION_HEADER, version);
    } else if request.id.is_some()
        && !matches!(
            ModernRequestMeta::from_params(request.params.as_ref()),
            ModernMeta::Legacy
        )
    {
        return header_mismatch(
            request.id,
            "Header mismatch: MCP-Protocol-Version header is required",
        );
    }
    let modern = version.is_some_and(is_modern_revision);
    // SEP-2243: the mirrors a gateway routes on must say what the body says.
    if let Err(reason) = check_standard_headers(headers, &request, modern) {
        return header_mismatch(request.id, &reason);
    }

    // 5. The session, on a server that keeps them: `initialize` starts a new
    // one, any other request names its own. Revision 2026-07-28 has none.
    let initialize = request.method == "initialize";
    let sessions = server.sessions().filter(|_| !modern).cloned();
    let session = match &sessions {
        Some(_) if initialize => None,
        Some(store) => match named_session(store, headers, request.id.as_ref()) {
            Ok(session) => session,
            Err(refusal) => return *refusal,
        },
        None => None,
    };

    // 6. Authenticate (RFC 9728 resource-server posture).
    let ctx = match server.authenticate(&request).await {
        Ok(ctx) => ctx,
        Err(refusal) => return auth_refusal_response(refusal),
    };
    if let Some(session) = &session {
        if !session.is_owned_by(&ctx) {
            return session_not_found(request.id);
        }
    }
    let session = match (&sessions, initialize) {
        (Some(_), true) => match Session::mint(&ctx) {
            Ok(minted) => Some(minted),
            Err(e) => {
                error!(error = %e, "No OS randomness for a session id");
                return transport_refusal(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    INTERNAL_ERROR,
                    "Could not start a session",
                );
            }
        },
        _ => session,
    };

    // 7. Dispatch under the resolved context, connected to the client through
    // the call's own answer.
    let ctx = match &session {
        Some(session) => ctx.with_cancellation(session.cancellation().clone()),
        None => ctx,
    };
    let (outbound, outbox) = mpsc::unbounded_channel();
    let connection = server.http_client_connection(outbound, session.clone(), &ctx);
    let ctx = ToolContext {
        client: ClientChannel::connected(connection),
        ..ctx
    };
    let serving = session.as_ref().map(Session::enter);
    let prefers_sse = prefers_event_stream(headers);

    if initialize {
        // A handshake runs no tool and sends nothing before its answer, which
        // carries the new session's id when it succeeded and the store had
        // room for it.
        let request_id = request.id.clone();
        let response = server.handle_request_with_context(request, &ctx).await;
        drop(serving);
        let minted = match (&sessions, session, &response) {
            (Some(store), Some(session), Some(answer)) if answer.is_success() => {
                let id = session.id().map(str::to_owned);
                if let Err(refusal) = store.insert(session) {
                    warn!(%refusal, "Refused to start an HTTP session");
                    return no_room_for_session(request_id, refusal);
                }
                id
            }
            _ => None,
        };
        let mut rendered = render_response(response, modern, prefers_sse);
        if let Some(id) = minted.and_then(|id| HeaderValue::from_str(&id).ok()) {
            rendered.headers_mut().insert(MCP_SESSION_ID_HEADER, id);
        }
        return rendered;
    }

    answer_call(server, request, ctx, outbox, modern, prefers_sse, serving).await
}

/// The live session a request's `Mcp-Session-Id` names, `None` when it names
/// none, or the refusal: 400 for a repeated or unreadable header, 404 for an
/// id that is not live — which tells the client to initialize again.
fn named_session(
    store: &SessionStore,
    headers: &HeaderMap,
    id: Option<&Value>,
) -> Result<Option<Arc<Session>>, Refusal> {
    let mut values = headers.get_all(MCP_SESSION_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    let session_id = match (value.to_str(), values.next()) {
        (Ok(session_id), None) => session_id.trim(),
        _ => {
            return Err(Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(JsonRpcResponse::error(
                        id.cloned(),
                        INVALID_REQUEST,
                        "Mcp-Session-Id must be one visible-ASCII value",
                    )),
                )
                    .into_response(),
            ));
        }
    };
    store
        .find(session_id)
        .map(Some)
        .ok_or_else(|| Box::new(session_not_found(id.cloned())))
}

/// The 503 of an `initialize` whose session the store has no room for: the
/// handshake is not answered, since a session-keeping server's answer names
/// the session the client goes on in.
fn no_room_for_session(id: Option<Value>, refusal: SessionRefusal) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(JsonRpcResponse::error(
            id,
            INTERNAL_ERROR,
            refusal.to_string(),
        )),
    )
        .into_response()
}

/// The 404 of a session id that is not live — expired, ended, never minted,
/// or another caller's, which is reported the same way so the answer cannot
/// confirm someone else's session exists.
fn session_not_found(id: Option<Value>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(JsonRpcResponse::error(
            id,
            INVALID_REQUEST,
            "Session not found: initialize a new session",
        )),
    )
        .into_response()
}

/// A dispatched request's single answer: 202 with no body when there is none
/// (a notification, or a request the client cancelled), else the response
/// under its status, as JSON or one SSE event, whichever the client prefers.
fn render_response(response: Option<JsonRpcResponse>, modern: bool, prefers_sse: bool) -> Response {
    let Some(response) = response else {
        // An accepted notification: Streamable HTTP requires 202 Accepted with
        // no body (basic/transports §Sending Messages to the Server).
        return StatusCode::ACCEPTED.into_response();
    };

    debug!(method = "mcp", "Handled HTTP MCP request");

    // A modern refusal carries its HTTP status; anything else is a 200
    // rendered as JSON or a single SSE event, whichever the client weighs
    // higher (a tie is answered as an event stream).
    let status = response_status(&response, modern);
    if status != StatusCode::OK {
        return (status, Json(response)).into_response();
    }
    if prefers_sse {
        respond_sse(&response)
    } else {
        Json(response).into_response()
    }
}

/// A dispatch in flight: the call's eventual response.
type Dispatch = Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send>>;

/// Dispatch `request` and answer it: as [`render_response`] when the call
/// sent its client nothing first, else as an event stream of what it sent,
/// the response last.
///
/// The choice is made by whichever comes first, the response or a message.
/// Once streaming, the call is polled by the stream itself, so a client that
/// goes away drops the call — and any request it is waiting on — with it,
/// exactly as hyper drops a handler whose connection closed. `serving` keeps
/// the request counted in its session until the answer is over.
async fn answer_call<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    request: JsonRpcRequest,
    ctx: ToolContext,
    mut outbox: mpsc::UnboundedReceiver<JsonRpcMessage>,
    modern: bool,
    prefers_sse: bool,
    serving: Option<SessionUse>,
) -> Response {
    let id = request.id.clone();
    let mut call: Dispatch =
        Box::pin(async move { server.handle_request_with_context(request, &ctx).await });

    let first = tokio::select! {
        biased;
        response = &mut call => {
            let sent = drain(&mut outbox);
            if sent.is_empty() {
                drop(serving);
                return render_response(response, modern, prefers_sse);
            }
            let stream = CallStream {
                phase: Phase::Draining(response),
                sent: sent.into(),
                outbox,
                serving,
            };
            return Sse::new(stream.into_events()).into_response();
        }
        Some(message) = outbox.recv() => message,
    };

    // The call is now polled by the response body, outside the request
    // guard's panic containment, so a panic is caught here and answered as
    // the JSON-RPC internal error it is.
    let call: Dispatch = Box::pin(AssertUnwindSafe(call).catch_unwind().map(move |outcome| {
        outcome.unwrap_or_else(|_| {
            error!("A call panicked while streaming its answer");
            Some(JsonRpcResponse::error(
                id,
                INTERNAL_ERROR,
                "The call failed while answering".to_owned(),
            ))
        })
    }));
    let stream = CallStream {
        phase: Phase::Running(call),
        sent: VecDeque::from([first]),
        outbox,
        serving,
    };
    Sse::new(stream.into_events())
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Every message already queued on `outbox`.
fn drain(outbox: &mut mpsc::UnboundedReceiver<JsonRpcMessage>) -> Vec<JsonRpcMessage> {
    let mut sent = Vec::new();
    while let Ok(message) = outbox.try_recv() {
        sent.push(message);
    }
    sent
}

/// Where a streamed call is.
enum Phase {
    /// Still running.
    Running(Dispatch),
    /// Answered: what is still queued goes out, then this response.
    Draining(Option<JsonRpcResponse>),
    /// The response is out.
    Done,
}

/// The event stream of a call that talked to its client before answering.
struct CallStream {
    phase: Phase,
    /// Messages taken off the outbox, not yet sent.
    sent: VecDeque<JsonRpcMessage>,
    outbox: mpsc::UnboundedReceiver<JsonRpcMessage>,
    /// Held until the stream ends, so the session counts the request busy.
    serving: Option<SessionUse>,
}

impl CallStream {
    /// The stream's next message: queued ones first, then whatever the call
    /// sends or answers.
    async fn next(&mut self) -> Option<JsonRpcMessage> {
        if let Some(message) = self.sent.pop_front() {
            return Some(message);
        }
        loop {
            match &mut self.phase {
                Phase::Running(call) => {
                    let sent_or_answered = tokio::select! {
                        biased;
                        Some(message) = self.outbox.recv() => Ok(message),
                        response = call => Err(response),
                    };
                    match sent_or_answered {
                        Ok(message) => return Some(message),
                        Err(response) => self.phase = Phase::Draining(response),
                    }
                }
                Phase::Draining(response) => {
                    if let Ok(message) = self.outbox.try_recv() {
                        return Some(message);
                    }
                    let response = response.take();
                    self.phase = Phase::Done;
                    self.serving = None;
                    return response.map(JsonRpcMessage::Response);
                }
                Phase::Done => return None,
            }
        }
    }

    fn into_events(self) -> impl Stream<Item = Result<Event, Infallible>> + Send {
        stream::unfold(self, |mut call| async move {
            let message = call.next().await?;
            Some((Ok(sse_event(&message)), call))
        })
    }
}

/// Accept a client's response to a server request: 202 with no body once it
/// is judged, routed to the call waiting for it when the caller and session
/// match the ones the request was sent to.
///
/// It passes the gates a request does — protocol version, an `Mcp-Session-Id`
/// that is live and the caller's own, authentication — and carries none of
/// the SEP-2243 mirrors, which a response has nothing to fill. Whether a call
/// was waiting does not change the answer, so a caller cannot probe for
/// another's pending requests, and a late answer to one already given up on
/// is not an error.
async fn accept_client_response<S: Send + Sync + ?Sized + 'static>(
    server: &McpServer<S>,
    headers: &HeaderMap,
    response: JsonRpcResponse,
) -> Response {
    let version = match accepted_protocol_version(server, headers, None) {
        Ok(version) => version,
        Err(refusal) => return *refusal,
    };
    if let Err(reason) = check_response_headers(headers) {
        return header_mismatch(None, &reason);
    }
    let modern = version.is_some_and(is_modern_revision);
    let session = match server.sessions().filter(|_| !modern) {
        Some(store) => match named_session(store, headers, None) {
            Ok(session) => session,
            Err(refusal) => return *refusal,
        },
        None => None,
    };
    let message = transport_message(headers, response.id.clone(), version);
    let ctx = match server.authenticate(&message).await {
        Ok(ctx) => ctx,
        Err(refusal) => return auth_refusal_response(refusal),
    };
    if let Some(session) = &session {
        if !session.is_owned_by(&ctx) {
            return session_not_found(None);
        }
    }
    let delivered =
        server.deliver_client_response(&ctx, session.as_ref().and_then(|s| s.id()), response);
    debug!(delivered, "Client answered a server request");
    StatusCode::ACCEPTED.into_response()
}

/// End the session `Mcp-Session-Id` names: `DELETE /mcp`, routed only on a
/// server with [`McpServer::with_http_sessions`].
///
/// Passes the `Origin` and `Host` gates and authentication as a `POST` does;
/// answers 204 once the session is ended (its calls still running are
/// cancelled), 400 without the header, and 404 for a session that is not
/// live or not the caller's.
pub async fn handle_mcp_delete<S: Send + Sync + ?Sized + 'static>(
    State(server): State<Arc<McpServer<S>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(refusal) = check_origin_and_host(&server, &headers, &uri) {
        return *refusal;
    }
    let Some(store) = server.sessions() else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    };
    let session = match named_session(store, &headers, None) {
        Ok(Some(session)) => session,
        Ok(None) => {
            return transport_refusal(
                StatusCode::BAD_REQUEST,
                INVALID_REQUEST,
                "DELETE needs the Mcp-Session-Id of the session to end",
            );
        }
        Err(refusal) => return *refusal,
    };
    let version = protocol_version_header(&headers).ok().flatten();
    let ctx = match server
        .authenticate(&transport_message(&headers, None, version))
        .await
    {
        Ok(ctx) => ctx,
        Err(refusal) => return auth_refusal_response(refusal),
    };
    if !session.is_owned_by(&ctx) {
        return session_not_found(None);
    }
    if let Some(id) = session.id() {
        store.end(id);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// What the auth hook is handed for an HTTP message that is not a JSON-RPC
/// request — a client's response, a `DELETE` ending a session: an empty
/// `method`, the message's `id` if it has one, and the credential and headers
/// the transport read, exactly as a request carries them.
fn transport_message(
    headers: &HeaderMap,
    id: Option<Value>,
    version: Option<&str>,
) -> JsonRpcRequest {
    let mut message = JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_owned(),
        method: String::new(),
        params: None,
        id,
        auth_token: bearer_token(headers),
        headers: forwarded_headers(headers),
        metadata: HashMap::new(),
    };
    if let Some(version) = version {
        message = message.with_metadata(PROTOCOL_VERSION_HEADER, version);
    }
    message
}

/// Render an [`AuthHook`](crate::mcp::auth::AuthHook) refusal as its status
/// code, with a JSON-RPC error body so clients can parse it (never a bare text
/// body).
fn auth_refusal_response(refusal: AuthError) -> Response {
    match refusal {
        AuthError::Unauthorized { www_authenticate } => (
            // RFC 9728: 401 with the `WWW-Authenticate` challenge.
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, www_authenticate)],
            Json(JsonRpcResponse::error(
                None,
                UNAUTHORIZED,
                "Unauthorized".to_owned(),
            )),
        )
            .into_response(),
        AuthError::Forbidden { reason } => (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::error(None, UNAUTHORIZED, reason)),
        )
            .into_response(),
        // RFC 6750 §3.1: an insufficient-scope refusal is a 403 that still
        // carries the challenge, so the client learns which grant it needs
        // rather than only that it was refused.
        AuthError::InsufficientScope {
            www_authenticate,
            reason,
        } => (
            StatusCode::FORBIDDEN,
            [(header::WWW_AUTHENTICATE, www_authenticate)],
            Json(JsonRpcResponse::error(None, UNAUTHORIZED, reason)),
        )
            .into_response(),
        // A valid credential over its budget: 429, never 401, so the client
        // waits instead of discarding a good token. The header and the body
        // carry the same wait.
        AuthError::RateLimited {
            retry_after_secs,
            reason,
        } => {
            let retry_after_secs = retry_after_secs.max(1);
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, retry_after_secs.to_string())],
                Json(JsonRpcResponse::error_with_data(
                    None,
                    RATE_LIMITED,
                    reason,
                    serde_json::json!({ "retry_after_secs": retry_after_secs }),
                )),
            )
                .into_response()
        }
        AuthError::Internal { reason } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonRpcResponse::error(None, INTERNAL_ERROR, reason)),
        )
            .into_response(),
    }
}

/// The request's `MCP-Protocol-Version`, trimmed, or `None` when it carries
/// none.
///
/// # Errors
///
/// The `HeaderMismatchError` reason when the header is repeated or is not
/// visible ASCII: neither names one revision the body could agree with.
fn protocol_version_header(headers: &HeaderMap) -> Result<Option<&str>, String> {
    let mut values = headers.get_all(PROTOCOL_VERSION_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("Header mismatch: MCP-Protocol-Version is repeated".to_owned());
    }
    value.to_str().map(|v| Some(v.trim())).map_err(|_| {
        "Header mismatch: MCP-Protocol-Version contains characters not permitted in an HTTP field value"
            .to_owned()
    })
}

/// A `HeaderMismatchError` (-32020) answered with HTTP 400, echoing `id`.
fn header_mismatch(id: Option<Value>, reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(JsonRpcResponse::error(
            id,
            HEADER_MISMATCH,
            reason.to_owned(),
        )),
    )
        .into_response()
}

/// The HTTP status a dispatched response travels under.
///
/// A header that disagrees with its body is a 400 in any era. A modern
/// (2026-07-28) request's protocol refusals carry their status too, which is
/// how a client probing the era tells a modern server from a legacy one: 400
/// for an unsupported revision or an undeclared client capability, 404 for a
/// method the server does not implement. An `initialize`-era client expects
/// those as JSON-RPC errors under 200, and keeps getting them that way.
fn response_status(response: &JsonRpcResponse, modern: bool) -> StatusCode {
    match response.error.as_ref().map(|error| error.code) {
        Some(HEADER_MISMATCH) => StatusCode::BAD_REQUEST,
        Some(UNSUPPORTED_PROTOCOL_VERSION | MISSING_REQUIRED_CLIENT_CAPABILITY) if modern => {
            StatusCode::BAD_REQUEST
        }
        Some(METHOD_NOT_FOUND) if modern => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    }
}

/// A refusal the transport makes before dispatch: `status`, with a JSON-RPC
/// error carrying no `id` — the request was never read far enough to have one
/// that is answered.
fn transport_refusal(status: StatusCode, code: i32, message: &str) -> Response {
    (
        status,
        Json(JsonRpcResponse::error(None, code, message.to_owned())),
    )
        .into_response()
}

/// The media type every `POST /mcp` body must carry.
const APPLICATION_JSON: &str = "application/json";

/// The media type of the streamed answer.
const TEXT_EVENT_STREAM: &str = "text/event-stream";

/// Whether the request's `Content-Type` is `application/json`, parameters
/// such as `charset` allowed. A missing or unreadable header is not.
fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case(APPLICATION_JSON))
        })
}

/// One media range of an `Accept` header: its `type/subtype` and its weight.
struct MediaRange<'a> {
    media: &'a str,
    quality: f32,
}

impl MediaRange<'_> {
    /// Whether this range names `media` (`type/subtype`), exactly or through a
    /// `*/*` or `type/*` wildcard, compared case-insensitively.
    fn covers(&self, media: &str) -> bool {
        if self.media == "*/*" || self.media.eq_ignore_ascii_case(media) {
            return true;
        }
        match (self.media.split_once('/'), media.split_once('/')) {
            (Some((range_type, "*")), Some((wanted_type, _))) => {
                range_type.eq_ignore_ascii_case(wanted_type)
            }
            _ => false,
        }
    }
}

/// The media ranges of the request's `Accept` header (RFC 9110 §12.5.1). A
/// range without a readable `q` weighs 1; an unreadable header lists none.
fn accept_ranges(headers: &HeaderMap) -> Vec<MediaRange<'_>> {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|range| {
            let mut parts = range.split(';');
            let media = parts.next()?.trim();
            if media.is_empty() {
                return None;
            }
            let quality = parts
                .filter_map(|param| param.trim().split_once('='))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
                .and_then(|(_, q)| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            Some(MediaRange { media, quality })
        })
        .collect()
}

/// The weight the client gives `media`: the highest `q` among the ranges
/// covering it, or 0 when none does.
fn accept_weight(ranges: &[MediaRange<'_>], media: &str) -> f32 {
    ranges
        .iter()
        .filter(|range| range.covers(media))
        .map(|range| range.quality)
        .fold(0.0, f32::max)
}

/// Whether the client accepts both answers a POST may get — the Streamable
/// HTTP rule that its `Accept` list `application/json` and
/// `text/event-stream`. A range weighted `q=0` refuses its type.
fn accepts_json_and_event_stream(headers: &HeaderMap) -> bool {
    let ranges = accept_ranges(headers);
    accept_weight(&ranges, APPLICATION_JSON) > 0.0
        && accept_weight(&ranges, TEXT_EVENT_STREAM) > 0.0
}

/// Whether to answer as an event stream: the client weighs it at least as
/// high as JSON.
fn prefers_event_stream(headers: &HeaderMap) -> bool {
    let ranges = accept_ranges(headers);
    accept_weight(&ranges, TEXT_EVENT_STREAM) >= accept_weight(&ranges, APPLICATION_JSON)
}

/// Whether a request with these `headers` passes the `allowed` `Origin` list —
/// [`is_origin_allowed`] applied to the request's `Origin` header.
///
/// A present `Origin` that is not readable text is judged as the empty origin,
/// which no list admits but `"*"`: present-and-invalid is refused, never read
/// as absent. Public so a host serving another route over the same catalog
/// gates it by reading the header exactly as `POST /mcp` does.
#[must_use]
pub fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let origin = headers
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or_default());
    is_origin_allowed(origin, allowed)
}

/// Whether a request carrying `origin` passes the `allowed` list — the gate
/// MCP requires on every Streamable HTTP request to stop DNS rebinding.
///
/// - no `Origin` header (a non-browser client): accepted
/// - an empty list: only a loopback origin is accepted (see
///   [`McpServer::with_allowed_origins`]), so a page on another site that
///   rebinds its name to 127.0.0.1 cannot drive a local server
/// - a list containing `"*"`: every origin is accepted, by explicit opt-in
/// - otherwise the origin must be listed exactly
///
/// A host gating an HTTP route reads the header through [`origin_allowed`];
/// this is the rule it applies, for a caller that already holds the origin.
#[must_use]
pub fn is_origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    origin.is_none_or(|origin| {
        if allowed.is_empty() {
            is_loopback_origin(origin)
        } else {
            allowed.iter().any(|a| a == "*" || a == origin)
        }
    })
}

/// Whether `origin` is a serialized `http` or `https` origin (RFC 6454
/// `scheme://host[:port]`) whose host is a loopback interface.
///
/// Anything that is not exactly that shape is refused rather than guessed at:
/// the opaque origin `null`, other schemes, and any authority carrying a path,
/// userinfo, query, fragment or whitespace. The host test is
/// [`is_loopback_host`], the same one the startup posture check uses.
fn is_loopback_origin(origin: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return false;
    }
    split_authority(authority).is_some_and(|(host, _)| is_loopback_host(host))
}

/// Split a bare authority (`host[:port]`, an IPv6 literal bracketed) into its
/// host and optional port, or `None` when it is not exactly that shape: one
/// carrying a path, userinfo, query, fragment or whitespace, a bracket left
/// open, anything after `]` but `:port`, or a port that is not decimal.
fn split_authority(authority: &str) -> Option<(&str, Option<&str>)> {
    if authority.is_empty()
        || authority.contains(|c: char| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#'))
    {
        return None;
    }

    // Split off an optional `:port`, minding the colons inside a bracketed
    // IPv6 literal, after whose `]` only nothing or `:port` may follow.
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (literal, rest) = bracketed.split_once(']')?;
            if rest.is_empty() {
                (literal, None)
            } else {
                (literal, Some(rest.strip_prefix(':')?))
            }
        }
        None => authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port))),
    };
    port.is_none_or(is_port).then_some((host, port))
}

/// Whether a request naming `authority` (its `Host`, or the `:authority` of an
/// HTTP/2 request) passes the `allowed` host list — the server-side half of
/// DNS-rebinding protection.
///
/// A page that rebinds its own name to 127.0.0.1 reaches a local server with
/// that name in `Host`, and a browser need not send `Origin` on every
/// request, so the `Origin` gate alone cannot stop it. A loopback authority
/// (`localhost`, with or without its trailing dot, `127.0.0.0/8`, `[::1]`; any
/// port) always passes. Otherwise an entry `host` admits that host on any
/// port and `host:port` that pair only, compared case-insensitively. A
/// missing or malformed authority passes nothing.
#[must_use]
pub fn is_host_allowed(authority: Option<&str>, allowed: &[String]) -> bool {
    let Some(authority) = authority.map(str::trim) else {
        return false;
    };
    let Some((host, port)) = split_authority(authority) else {
        return false;
    };
    let bare = host.strip_suffix('.').unwrap_or(host);
    if is_loopback_host(bare) {
        return true;
    }
    allowed.iter().any(|entry| {
        split_authority(entry.trim()).is_some_and(|(allowed_host, allowed_port)| {
            allowed_host.eq_ignore_ascii_case(host)
                && allowed_port.is_none_or(|allowed_port| Some(allowed_port) == port)
        })
    })
}

/// A refusal of the request's authority: its status and plain-text reason.
type HostRefusal = (StatusCode, &'static str);

/// The refusal of an authority no list admits.
const HOST_NOT_ALLOWED: HostRefusal = (StatusCode::FORBIDDEN, "Host not allowed");

/// The authority a request targets: its `Host` header, else the URI's
/// authority (HTTP/2 carries `:authority` there, not in `Host`).
///
/// # Errors
///
/// A refusal for a request naming no single authority: `Host` repeated (403,
/// a request-smuggling vector), or a `Host` disagreeing with an absolute-form
/// target (400, RFC 9112 §3.2).
fn request_authority<'r>(
    headers: &'r HeaderMap,
    uri: &'r Uri,
) -> Result<Option<&'r str>, HostRefusal> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(HOST_NOT_ALLOWED);
    }
    let host = match host.map(HeaderValue::to_str) {
        Some(Ok(host)) => Some(host),
        Some(Err(_)) => return Err(HOST_NOT_ALLOWED),
        None => None,
    };
    let target = uri.authority().map(Authority::as_str);
    match (host, target) {
        (Some(host), Some(target)) if !host.trim().eq_ignore_ascii_case(target) => Err((
            StatusCode::BAD_REQUEST,
            "Host does not match the request target",
        )),
        (Some(host), _) => Ok(Some(host)),
        (None, target) => Ok(target),
    }
}

/// Whether a request passes the `allowed` host list — [`is_host_allowed`]
/// applied to the authority it targets.
///
/// # Errors
///
/// The refusal to answer with: 403 for an authority not allowed (or none),
/// 400 for a `Host` that contradicts the request target.
fn check_host(headers: &HeaderMap, uri: &Uri, allowed: &[String]) -> Result<(), HostRefusal> {
    let authority = request_authority(headers, uri)?;
    if is_host_allowed(authority, allowed) {
        Ok(())
    } else {
        debug!(host = ?authority, "Rejected MCP request: host not allowed");
        Err(HOST_NOT_ALLOWED)
    }
}

/// Middleware [`serve`] layers over a loopback bind whose server names no
/// host list: only a loopback `Host` reaches it, so a page rebinding its own
/// name to 127.0.0.1 cannot drive it.
async fn require_loopback_host(request: Request, next: Next) -> Response {
    match check_host(request.headers(), request.uri(), &[]) {
        Ok(()) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    }
}

/// Whether `port` is a decimal TCP port.
fn is_port(port: &str) -> bool {
    port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
}

/// The bearer credential of the request's `Authorization` header, read with
/// the crate's one scheme parser ([`bearer_credential`]).
///
/// Public so a host serving another route over the same catalog hands its
/// [`AuthHook`](crate::mcp::auth::AuthHook) exactly the credential `POST /mcp`
/// would.
#[must_use]
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_credential)
        .map(str::to_owned)
}

/// Headers a transport never forwards in [`JsonRpcRequest::headers`]: the
/// bearer already travels as [`JsonRpcRequest::auth_token`], and a cookie or a
/// proxy credential is nothing a JSON-RPC handler should hold.
const WITHHELD_HEADERS: [header::HeaderName; 3] = [
    header::AUTHORIZATION,
    header::COOKIE,
    header::PROXY_AUTHORIZATION,
];

/// The request's HTTP headers as [`JsonRpcRequest::headers`] carries them.
///
/// The [`AuthHook`](crate::mcp::auth::AuthHook) reads them under lower-case
/// names, as strings, credentials withheld. A value that is not visible ASCII
/// is dropped, and a repeated header keeps its last value. `None` when nothing
/// is left.
///
/// Public so a host serving another route over the same catalog hands its
/// hook exactly the headers `POST /mcp` would.
#[must_use]
pub fn forwarded_headers(headers: &HeaderMap) -> Option<HashMap<String, Value>> {
    let forwarded: HashMap<String, Value> = headers
        .iter()
        .filter(|(name, _)| !WITHHELD_HEADERS.contains(name))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), Value::String(value.to_owned())))
        })
        .collect();
    (!forwarded.is_empty()).then_some(forwarded)
}

/// One message as an SSE event.
fn sse_event<T: serde::Serialize>(message: &T) -> Event {
    let data = serde_json::to_string(message).unwrap_or_else(|e| {
        error!(error = %e, "SSE serialization failed");
        format!(
            r#"{{"jsonrpc":"2.0","error":{{"code":-32603,"message":"Serialization failed: {e}"}}}}"#
        )
    });
    Event::default().data(data)
}

/// Wrap a JSON-RPC response in a single SSE event
fn respond_sse(response: &JsonRpcResponse) -> Response {
    let event = sse_event(response);
    let event_stream = stream::once(async { Ok::<_, Infallible>(event) });
    Sse::new(event_stream).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{INVALID_PARAMS, PARSE_ERROR};
    use crate::mcp::auth::AuthHook;
    use crate::mcp::schema::{Tool, ToolResponse};
    use crate::mcp::server::DEFAULT_MAX_REQUEST_BYTES;
    use crate::mcp::tool::{McpTool, ToolCapabilities, ToolContext, ToolRegistry};
    use crate::mcp::transport::mirror::{MCP_METHOD_HEADER, MCP_NAME_HEADER};
    use axum::body::Body;
    use http::Request;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    /// What a conforming client sends, weighted so the answer is plain JSON.
    const TEST_ACCEPT: &str = "application/json, text/event-stream;q=0.5";

    struct TestState;

    struct HelloTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for HelloTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "hello".to_owned(),
                description: "Says hello".to_owned(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(
            &self,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("hello world".to_owned())
        }
    }

    fn make_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let state = Arc::new(TestState);
        let server = Arc::new(McpServer::new("test", "0.1.0", registry, state));
        mcp_router(server)
    }

    struct AdminTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for AdminTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "admin_op".to_owned(),
                description: "Admin-only".to_owned(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        fn capabilities(&self) -> ToolCapabilities {
            ToolCapabilities::ADMIN_ONLY
        }

        async fn execute(
            &self,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("admin ok".to_owned())
        }
    }

    /// Accepts `Bearer admin` (admin context) and `Bearer user` (non-admin);
    /// rejects anything else with 401 + a `WWW-Authenticate` challenge.
    struct TestAuthHook;

    #[async_trait::async_trait]
    impl AuthHook<TestState> for TestAuthHook {
        async fn authenticate(
            &self,
            request: &JsonRpcRequest,
            _state: &Arc<TestState>,
        ) -> Result<ToolContext, AuthError> {
            match request.auth_token.as_deref() {
                Some("admin") => Ok(ToolContext::new().with_user("u1").as_admin(true)),
                Some("user") => Ok(ToolContext::new().with_user("u2")),
                _ => Err(AuthError::Unauthorized {
                    www_authenticate: "Bearer resource_metadata=\"https://example.test/.well-known/oauth-protected-resource\"".to_owned(),
                }),
            }
        }
    }

    fn make_authed_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        registry.register(Box::new(AdminTool));
        let state = Arc::new(TestState);
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, state)
                .with_auth_hook(Arc::new(TestAuthHook))
                .with_allowed_origins(vec!["https://app.example.test".to_owned()]),
        );
        mcp_router(server)
    }

    #[tokio::test]
    async fn mcp_post_ping_returns_json() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 200);

        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["jsonrpc"], "2.0");
        assert!(json.get("result").is_some());
    }

    #[tokio::test]
    async fn mcp_post_tools_call() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "hello world");
    }

    #[tokio::test]
    async fn mcp_post_invalid_json_returns_parse_error() {
        let app = make_app();
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body("not json".to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["error"]["code"], PARSE_ERROR);
    }

    /// Streamable HTTP §Sending Messages to the Server: an accepted
    /// notification "MUST return HTTP status code 202 Accepted with no body".
    /// Older Python SDK clients fail on the 204 this used to return.
    #[tokio::test]
    async fn mcp_post_notification_returns_202_with_no_body() {
        let (status, body) = post(
            make_app(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty(), "a 202 carries no body; got {body:?}");
    }

    /// POST `body` to `/mcp` with the extra `headers`, returning the status and
    /// the raw response body.
    async fn post(app: Router, body: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(body.to_owned()).expect("request"); // Safe: test assertion
        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A body that is JSON but not a Request is one the server cannot accept:
    /// a 4xx carrying -32600, not a 200 carrying -32700.
    #[tokio::test]
    async fn json_that_is_not_a_request_is_refused_with_400_invalid_request() {
        for body in [
            // A message with no method that is no response either.
            r#"{"jsonrpc":"2.0","id":7}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{},"error":{"code":1,"message":"x"}}"#,
            // A batch: MCP carries none.
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            // A null id: not a notification, and not answerable either.
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            // An id that is neither a string nor an integer.
            r#"{"jsonrpc":"2.0","id":{"n":1},"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        ] {
            let (status, response) = post(make_app(), body, &[]).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let json: Value = serde_json::from_str(&response).expect("json"); // Safe: test assertion
            assert_eq!(json["error"]["code"], INVALID_REQUEST, "{body}");
            assert_eq!(json["id"], Value::Null, "{body}");
        }
    }

    /// DNS-rebinding protection is on by default: with no allowlist a browser
    /// origin that is not loopback is refused with 403.
    #[tokio::test]
    async fn empty_allowlist_refuses_a_non_loopback_origin() {
        for origin in [
            "https://evil.test",
            "http://192.168.1.10:3000",
            "http://localhost.evil.test",
            "http://127.0.0.1.nip.io",
            "http://evil.test@localhost",
            "http://localhost/path",
            "http://localhost:99999",
            "http://::1",
            "ftp://localhost",
            "null",
        ] {
            let (status, _) = post(
                make_app(),
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                &[("origin", origin)],
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "origin {origin}");
        }
    }

    /// An `Origin` header that is present but not readable text is invalid,
    /// not absent: reading it as absent would admit it as a non-browser client.
    #[tokio::test]
    async fn an_unreadable_origin_is_refused_not_read_as_absent() {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header(
                "origin",
                header::HeaderValue::from_bytes(b"http://localhost\xff")
                    .expect("obs-text is a legal header byte"), // Safe: test fixture
            )
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_owned())
            .expect("request"); // Safe: test fixture
        let response = make_app().oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Local development keeps working without configuration: a page served
    /// from a loopback origin, on any port, reaches an unconfigured server.
    #[tokio::test]
    async fn empty_allowlist_admits_a_loopback_origin() {
        for origin in [
            "http://localhost:3000",
            "https://localhost",
            "http://LOCALHOST:8082",
            "http://127.0.0.1:8081",
            "http://127.1.2.3",
            "http://[::1]:5173",
            "http://[::1]",
        ] {
            let (status, _) = post(
                make_app(),
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                &[("origin", origin)],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "origin {origin}");
        }
    }

    /// `"*"` is the explicit opt-out: a host that lists it accepts any origin.
    #[tokio::test]
    async fn wildcard_allowlist_admits_any_origin() {
        let server = Arc::new(
            McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
                .with_allowed_origins(vec!["*".to_owned()]),
        );
        let (status, _) = post(
            mcp_router(server),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            &[("origin", "https://evil.test")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// A configured allowlist is exact: it does not also admit loopback, and a
    /// request with no `Origin` (a non-browser client) is still accepted.
    #[test]
    fn a_configured_allowlist_is_matched_exactly() {
        let allowed = vec!["https://app.example.test".to_owned()];
        assert!(is_origin_allowed(
            Some("https://app.example.test"),
            &allowed
        ));
        assert!(!is_origin_allowed(Some("http://localhost:3000"), &allowed));
        assert!(!is_origin_allowed(Some("https://evil.test"), &allowed));
        assert!(is_origin_allowed(None, &allowed));
        assert!(is_origin_allowed(None, &[]));
    }

    /// A body over the server's limit is refused with 413 and a JSON-RPC
    /// error, and one within it is served.
    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_with_413() {
        let app = || {
            mcp_router(Arc::new(
                McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
                    .with_max_request_bytes(64),
            ))
        };
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let (status, _) = post(app(), ping, &[]).await;
        assert_eq!(status, StatusCode::OK);

        let padded = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#,
            "x".repeat(128)
        );
        let (status, body) = post(app(), &padded, &[]).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
        assert_eq!(json["error"]["code"], INVALID_REQUEST);
    }

    /// The default limit is stated, not axum's implicit one.
    #[test]
    fn the_default_body_limit_is_the_stated_one() {
        let server = McpServer::new(
            "test",
            "0.1.0",
            ToolRegistry::<TestState>::new(),
            Arc::new(TestState),
        );
        assert_eq!(server.max_request_bytes(), DEFAULT_MAX_REQUEST_BYTES);
    }

    /// A body that is not UTF-8 cannot be JSON: a 400 parse error.
    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_a_parse_error() {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(Body::from(vec![0x7B, 0xFF, 0x7D]))
            .expect("request"); // Safe: test fixture
        let response = make_app().oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["error"]["code"], PARSE_ERROR);
    }

    /// GET `path` on `app`, returning the status, the CORS header and body.
    async fn get_path(app: Router, path: &str) -> (StatusCode, Option<String>, String) {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .expect("request"); // Safe: test fixture
        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let status = response.status();
        let cors = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        (status, cors, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A server that publishes its metadata gets it served at the RFC 9728
    /// path its resource derives and at the bare fallback, readable from a
    /// browser; one that does not publishes nothing.
    #[tokio::test]
    async fn published_resource_metadata_is_served_at_its_well_known_paths() {
        let metadata = ProtectedResourceMetadata::new(
            "https://mcp.example.test/mcp",
            vec!["https://auth.example.test".to_owned()],
        )
        .expect("valid resource") // Safe: test fixture
        .with_scopes(vec!["mcp:tools".to_owned()]);
        let app = || {
            mcp_router(Arc::new(
                McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
                    .with_protected_resource_metadata(metadata.clone()),
            ))
        };
        for path in [
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
        ] {
            let (status, cors, body) = get_path(app(), path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(cors.as_deref(), Some("*"), "{path}");
            let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
            assert_eq!(json["resource"], "https://mcp.example.test/mcp");
            assert_eq!(
                json["authorization_servers"][0],
                "https://auth.example.test"
            );
            assert_eq!(json["scopes_supported"][0], "mcp:tools");
        }

        let (status, _, _) = get_path(make_app(), "/.well-known/oauth-protected-resource").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "nothing is published unasked"
        );
    }

    /// Loopback authorities always pass; a listed `host` admits any port,
    /// `host:port` only that pair; anything malformed or absent passes nothing.
    #[test]
    fn host_allowlist_rules() {
        let none: Vec<String> = Vec::new();
        for loopback in [
            "localhost",
            "LOCALHOST:8080",
            "localhost.:3000",
            "127.0.0.1",
            "127.9.9.9:1",
            "[::1]:5173",
        ] {
            assert!(is_host_allowed(Some(loopback), &none), "{loopback}");
        }
        for refused in [
            "attacker.test",
            "localhost.attacker.test",
            "127.0.0.1.nip.io",
            "localhost:abc",
            "user@localhost",
            "",
        ] {
            assert!(!is_host_allowed(Some(refused), &none), "{refused}");
        }
        assert!(!is_host_allowed(None, &none));

        let allowed = vec![
            "mcp.example.test".to_owned(),
            "api.example.test:8443".to_owned(),
        ];
        assert!(is_host_allowed(Some("mcp.example.test"), &allowed));
        assert!(is_host_allowed(Some("Mcp.Example.Test:9000"), &allowed));
        assert!(is_host_allowed(Some("api.example.test:8443"), &allowed));
        assert!(!is_host_allowed(Some("api.example.test"), &allowed));
        assert!(!is_host_allowed(Some("api.example.test:443"), &allowed));
        assert!(!is_host_allowed(Some("evil.test"), &allowed));
    }

    /// A router whose server names a host list refuses any other `Host`, a
    /// repeated one, and one contradicting an absolute-form target.
    #[tokio::test]
    async fn a_named_host_list_is_enforced_by_the_router() {
        let app = || {
            mcp_router(Arc::new(
                McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
                    .with_allowed_hosts(vec!["mcp.example.test".to_owned()]),
            ))
        };
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let (status, _) = post(app(), ping, &[("host", "mcp.example.test")]).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = post(app(), ping, &[("host", "evil.test")]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = post(app(), ping, &[]).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "no Host names no allowed authority"
        );
        let (status, _) = post(
            app(),
            ping,
            &[("host", "mcp.example.test"), ("host", "evil.test")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let absolute = Request::builder()
            .method("POST")
            .uri("http://evil.test/mcp")
            .header("host", "mcp.example.test")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(ping.to_owned())
            .expect("request"); // Safe: test fixture
        let response = app().oneshot(absolute).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// With no list, the router checks no `Host`: a host serving a public
    /// name behind its own router answers to whatever its proxy forwards.
    #[tokio::test]
    async fn no_host_list_checks_no_host() {
        let (status, _) = post(
            make_app(),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            &[("host", "mcp.public.example")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The token comes from the `Authorization` header only. A body field
    /// named `auth` used to survive whenever no header was sent, so any caller
    /// could write its own credential into the JSON and be served as admin.
    #[tokio::test]
    async fn a_token_in_the_body_does_not_authenticate() {
        let (status, _) = post(
            make_authed_app(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"admin_op"},"auth":"admin"}"#,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// RFC 7235 §2.1: the auth-scheme is case-insensitive.
    #[tokio::test]
    async fn the_bearer_scheme_is_matched_case_insensitively() {
        for authorization in ["bearer user", "BEARER user"] {
            let (status, body) = post(
                make_authed_app(),
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#,
                &[("authorization", authorization)],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{authorization}");
            let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
            assert_eq!(
                json["result"]["content"][0]["text"], "hello world",
                "{authorization}"
            );
        }
    }

    #[tokio::test]
    async fn mcp_post_sse_accept_returns_event_stream() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let content_type = response
            .headers()
            .get("content-type")
            .expect("content-type") // Safe: test assertion
            .to_str()
            .expect("str"); // Safe: test assertion
        assert!(content_type.contains("text/event-stream"));
    }

    /// POST a ping carrying exactly `headers`, returning the status and body.
    async fn post_raw(headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut builder = Request::builder().method("POST").uri("/mcp");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_owned())
            .expect("request"); // Safe: test fixture
        let response = make_app().oneshot(request).await.expect("response"); // Safe: test assertion
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A body that is not declared JSON is refused with 415 before it is
    /// read: a browser sends `text/plain` and form bodies cross-origin without
    /// a preflight, so admitting them would bypass the CORS policy.
    #[tokio::test]
    async fn a_body_not_declared_json_is_refused_with_415() {
        for content_type in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
            Some("multipart/form-data; boundary=x"),
            Some("application/jsonp"),
        ] {
            let mut headers = vec![("accept", TEST_ACCEPT)];
            if let Some(content_type) = content_type {
                headers.push(("content-type", content_type));
            }
            let (status, body) = post_raw(&headers).await;
            assert_eq!(
                status,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{content_type:?}"
            );
            let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
            assert_eq!(json["error"]["code"], INVALID_REQUEST);
            assert_eq!(json["id"], Value::Null);
        }
    }

    /// `application/json` is matched on its media type alone: parameters and
    /// case do not matter.
    #[tokio::test]
    async fn a_json_content_type_with_parameters_is_accepted() {
        for content_type in ["application/json; charset=utf-8", "Application/JSON"] {
            let (status, _) =
                post_raw(&[("content-type", content_type), ("accept", TEST_ACCEPT)]).await;
            assert_eq!(status, StatusCode::OK, "{content_type}");
        }
    }

    /// A client that does not accept both answers this endpoint may give is
    /// refused with 406.
    #[tokio::test]
    async fn an_accept_missing_either_type_is_refused_with_406() {
        for accept in [
            None,
            Some("application/json"),
            Some("text/event-stream"),
            Some("text/html"),
            Some("application/json, text/event-stream;q=0"),
            Some("*/*;q=0"),
        ] {
            let mut headers = vec![("content-type", APPLICATION_JSON)];
            if let Some(accept) = accept {
                headers.push(("accept", accept));
            }
            let (status, body) = post_raw(&headers).await;
            assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "{accept:?}");
            let json: Value = serde_json::from_str(&body).expect("json"); // Safe: test assertion
            assert_eq!(json["error"]["code"], INVALID_REQUEST);
        }
    }

    /// Wildcards cover both types, and the higher weight picks the rendering;
    /// a tie is answered as an event stream.
    #[tokio::test]
    async fn accept_wildcards_pass_and_weights_pick_the_rendering() {
        for (accept, sse) in [
            ("*/*", true),
            ("application/*, text/*", true),
            ("application/json, text/event-stream", true),
            ("application/json;q=0.4, text/event-stream;q=0.9", true),
            ("application/json, text/event-stream;q=0.5", false),
            ("application/json, */*;q=0.1", false),
        ] {
            let (status, body) =
                post_raw(&[("content-type", APPLICATION_JSON), ("accept", accept)]).await;
            assert_eq!(status, StatusCode::OK, "{accept}");
            assert_eq!(body.starts_with("data:"), sse, "{accept}: {body}");
        }
    }

    #[tokio::test]
    async fn mcp_post_tools_list() {
        let app = make_app();
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        let tools = json["result"]["tools"].as_array().expect("tools"); // Safe: test assertion
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "hello");
    }

    #[tokio::test]
    async fn mcp_post_initialize() {
        let app = make_app();
        let body = r#"{
            "jsonrpc":"2.0",
            "id":5,
            "method":"initialize",
            "params":{
                "protocolVersion":"2024-11-05",
                "capabilities":{},
                "clientInfo":{"name":"test"}
            }
        }"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["serverInfo"]["name"], "test");
    }

    #[tokio::test]
    async fn mcp_post_disallowed_origin_returns_403() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header("origin", "https://evil.test")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 403);
    }

    #[tokio::test]
    async fn mcp_post_missing_token_returns_401_with_challenge() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 401);
        let challenge = response
            .headers()
            .get("www-authenticate")
            .expect("www-authenticate header") // Safe: test assertion
            .to_str()
            .expect("str"); // Safe: test assertion
        assert!(challenge.contains("Bearer"));
        assert!(challenge.contains("resource_metadata"));
    }

    #[tokio::test]
    async fn mcp_post_authenticated_user_runs_allowed_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header("origin", "https://app.example.test")
            .header("authorization", "Bearer user")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "hello world");
    }

    #[tokio::test]
    async fn mcp_post_admin_token_runs_admin_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"admin_op"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header("authorization", "Bearer admin")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["content"][0]["text"], "admin ok");
        assert_eq!(json["result"]["isError"], false);
    }

    #[tokio::test]
    async fn mcp_post_non_admin_blocked_from_admin_tool() {
        let app = make_authed_app();
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"admin_op"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header("authorization", "Bearer user")
            .body(body.to_owned())
            .expect("request"); // Safe: test assertion

        let response = app.oneshot(request).await.expect("response"); // Safe: test assertion
                                                                      // The request is authenticated (200), but the registry's ADMIN_ONLY gate
                                                                      // turns it into a tool-level error result.
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body") // Safe: test assertion
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("json"); // Safe: test assertion
        assert_eq!(json["result"]["isError"], true);
        assert!(json["result"]["content"][0]["text"]
            .as_str()
            .expect("text") // Safe: test assertion
            .contains("admin"));
    }

    /// A scope refusal is a 403 that still carries the challenge.
    ///
    /// RFC 6750 §3.1: the point of `insufficient_scope` is that the client
    /// learns which grant it needs. A bare 403 tells it only that it lost, so
    /// the header is the whole value of the variant — asserted here because a
    /// renderer that dropped it would still return the "right" status.
    #[tokio::test]
    async fn insufficient_scope_returns_403_with_the_challenge() {
        struct ScopeHook;

        #[async_trait::async_trait]
        impl AuthHook<TestState> for ScopeHook {
            async fn authenticate(
                &self,
                _request: &JsonRpcRequest,
                _state: &Arc<TestState>,
            ) -> Result<ToolContext, AuthError> {
                Err(AuthError::InsufficientScope {
                    www_authenticate:
                        "Bearer error=\"insufficient_scope\", scope=\"fitness:write\"".to_owned(),
                    reason: "the grant does not cover this tool".to_owned(),
                })
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, Arc::new(TestState))
                .with_auth_hook(Arc::new(ScopeHook)),
        );
        let app = mcp_router(server);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", TEST_ACCEPT)
                    .body(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                               "params":{"name":"hello","arguments":{}}})
                        .to_string(),
                    )
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .expect("an insufficient-scope refusal must carry the challenge") // Safe: test assertion
            .to_str()
            .expect("ascii"); // Safe: test assertion
        assert!(
            challenge.contains("insufficient_scope"),
            "challenge names the error: {challenge}"
        );
        assert!(
            challenge.contains("fitness:write"),
            "challenge names the scope the client must ask for: {challenge}"
        );
    }

    /// Admits a request only when it sees the forwarded host it was sent and
    /// no credential among the headers.
    struct HostReadingHook;

    #[async_trait::async_trait]
    impl AuthHook<TestState> for HostReadingHook {
        async fn authenticate(
            &self,
            request: &JsonRpcRequest,
            _state: &Arc<TestState>,
        ) -> Result<ToolContext, AuthError> {
            let headers = request.headers.clone().unwrap_or_default();
            let host = headers.get("x-forwarded-host").and_then(Value::as_str);
            let leaked = ["authorization", "cookie", "proxy-authorization"]
                .iter()
                .any(|name| headers.contains_key(*name));
            if host == Some("mcp.example.test") && !leaked {
                Ok(ToolContext::new().with_user("u1"))
            } else {
                Err(AuthError::Unauthorized {
                    www_authenticate: format!("Bearer error=\"host {host:?} leaked {leaked}\""),
                })
            }
        }
    }

    /// The hook reads the dialed host from the forwarded headers, and never a
    /// credential: the bearer arrives as `auth_token` only, the cookie not at
    /// all.
    #[tokio::test]
    async fn hook_sees_forwarded_headers_without_credentials() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, Arc::new(TestState))
                .with_auth_hook(Arc::new(HostReadingHook)),
        );
        let response = mcp_router(server)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", TEST_ACCEPT)
                    .header("x-forwarded-host", "mcp.example.test")
                    .header("authorization", "Bearer secret-token")
                    .header("cookie", "session=secret")
                    .header("proxy-authorization", "Basic c2VjcmV0")
                    .body(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                               "params":{"name":"hello","arguments":{}}})
                        .to_string(),
                    )
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture
        let status = response.status();
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        assert_eq!(status, StatusCode::OK, "hook refused: {challenge:?}");
    }

    /// Credentials never enter the forwarded map; every other header does,
    /// under its lower-case name.
    #[test]
    fn forwarded_headers_withholds_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer t".parse().expect("value")); // Safe: test fixture
        headers.insert(header::COOKIE, "a=b".parse().expect("value")); // Safe: test fixture
        let host = "app.example.test".parse().expect("value"); // Safe: test fixture
        headers.insert("X-Forwarded-Host", host);
        let forwarded = forwarded_headers(&headers).expect("one header remains"); // Safe: test fixture
        assert_eq!(forwarded.len(), 1);
        assert_eq!(
            forwarded.get("x-forwarded-host"),
            Some(&Value::String("app.example.test".to_owned()))
        );

        let mut only_credentials = HeaderMap::new();
        only_credentials.insert(header::COOKIE, "a=b".parse().expect("value")); // Safe: test fixture
        assert_eq!(forwarded_headers(&only_credentials), None);
    }

    /// A hook that refuses every request with the one [`AuthError`] it holds.
    struct RefusingHook(AuthError);

    #[async_trait::async_trait]
    impl AuthHook<TestState> for RefusingHook {
        async fn authenticate(
            &self,
            _request: &JsonRpcRequest,
            _state: &Arc<TestState>,
        ) -> Result<ToolContext, AuthError> {
            Err(self.0.clone())
        }
    }

    /// POST a `tools/call` through a server whose hook refuses with `refusal`.
    async fn refused_call(refusal: AuthError) -> Response {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HelloTool));
        let server = Arc::new(
            McpServer::new("test", "0.1.0", registry, Arc::new(TestState))
                .with_auth_hook(Arc::new(RefusingHook(refusal))),
        );
        mcp_router(server)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", TEST_ACCEPT)
                    .body(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                               "params":{"name":"hello","arguments":{}}})
                        .to_string(),
                    )
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response") // Safe: test fixture
    }

    /// The JSON body of `response`.
    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &response
                .into_body()
                .collect()
                .await
                .expect("body") // Safe: test fixture
                .to_bytes(),
        )
        .expect("json body") // Safe: test fixture
    }

    /// A spent budget is a 429 carrying the wait, never a 401.
    ///
    /// A 401 tells an OAuth client its token is dead, so it refreshes and
    /// re-authorizes, and the budget refuses it again. The header and the
    /// JSON-RPC error carry the same wait, and no challenge is sent.
    #[tokio::test]
    async fn rate_limited_returns_429_with_retry_after() {
        let response = refused_call(AuthError::RateLimited {
            retry_after_secs: 42,
            reason: "Request budget spent".to_owned(),
        })
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .expect("a 429 must carry Retry-After"), // Safe: test assertion
            "42"
        );
        assert!(
            response.headers().get(header::WWW_AUTHENTICATE).is_none(),
            "a spent budget is not a credential challenge"
        );
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], RATE_LIMITED);
        assert_eq!(body["error"]["message"], "Request budget spent");
        assert_eq!(body["error"]["data"]["retry_after_secs"], 42);
    }

    /// A zero wait still reads as a wait: the refusal is in force.
    #[tokio::test]
    async fn rate_limited_floors_the_wait_at_one_second() {
        let response = refused_call(AuthError::RateLimited {
            retry_after_secs: 0,
            reason: "Request budget spent".to_owned(),
        })
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .expect("a 429 must carry Retry-After"), // Safe: test assertion
            "1"
        );
        assert_eq!(
            json_body(response).await["error"]["data"]["retry_after_secs"],
            1
        );
    }

    /// A host-side failure during authentication is a 500, never a 401 that
    /// would send the client to discard a good token.
    #[tokio::test]
    async fn internal_returns_500_without_a_challenge() {
        let response = refused_call(AuthError::Internal {
            reason: "Authentication is temporarily unavailable".to_owned(),
        })
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
        assert!(response.headers().get(header::RETRY_AFTER).is_none());
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], INTERNAL_ERROR);
        assert_eq!(
            body["error"]["message"],
            "Authentication is temporarily unavailable"
        );
    }

    /// An `MCP-Protocol-Version` the server does not speak is refused.
    ///
    /// The header used to be read into request metadata that nothing read
    /// back, which reads as wired and is not. On a stateless server this is the
    /// only place a per-request revision assertion can be judged.
    #[tokio::test]
    async fn an_unsupported_protocol_version_header_is_refused() {
        let response = make_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", TEST_ACCEPT)
                    .header(PROTOCOL_VERSION_HEADER, "1999-01-01")
                    .body(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string())
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(
            &response
                .into_body()
                .collect()
                .await
                .expect("body") // Safe: test fixture
                .to_bytes(),
        )
        .expect("json"); // Safe: test fixture
        assert_eq!(body["error"]["code"], -32_022);
        assert_eq!(body["error"]["data"]["requested"], "1999-01-01");
        assert!(
            body["error"]["data"]["supported"].is_array(),
            "the refusal tells the client what we DO speak: {body}"
        );
    }

    /// A supported revision on the header is served normally — the gate must
    /// refuse the wrong one without refusing the right one.
    #[tokio::test]
    async fn a_supported_protocol_version_header_is_served() {
        let supported = McpServer::new("test", "0.1.0", ToolRegistry::new(), Arc::new(TestState))
            .advertised_protocol_versions()
            .first()
            .cloned()
            .expect("a server advertises at least one revision"); // Safe: test assertion

        let response = make_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", TEST_ACCEPT)
                    .header(PROTOCOL_VERSION_HEADER, supported.as_str())
                    .header(MCP_METHOD_HEADER, "tools/list")
                    .body(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
                               "params": modern_params(&supported, json!({}))})
                        .to_string(),
                    )
                    .expect("request"), // Safe: test fixture
            )
            .await
            .expect("response"); // Safe: test fixture

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert!(body["result"]["tools"].is_array(), "{body}");
    }

    /// `params` with the modern `_meta` for `version` merged into `rest`.
    fn modern_params(version: &str, mut rest: Value) -> Value {
        rest["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        rest
    }

    /// POST `body` with the version header (when given) and `extra` headers,
    /// returning the status and the JSON body. `Mcp-Method` mirrors the body
    /// unless `extra` sets it.
    async fn post_versioned(
        version: Option<&str>,
        extra: &[(&str, &str)],
        body: &Value,
    ) -> (StatusCode, Value) {
        post_versioned_to(make_app(), version, extra, body).await
    }

    async fn post_versioned_to(
        app: Router,
        version: Option<&str>,
        extra: &[(&str, &str)],
        body: &Value,
    ) -> (StatusCode, Value) {
        let mut headers: Vec<(&str, &str)> = extra.to_vec();
        if let Some(version) = version {
            headers.push((PROTOCOL_VERSION_HEADER, version));
        }
        let method = body["method"].as_str().unwrap_or_default();
        if !extra.iter().any(|(name, _)| *name == MCP_METHOD_HEADER) {
            headers.push((MCP_METHOD_HEADER, method));
        }
        let (status, raw) = post(app, &body.to_string(), &headers).await;
        let json = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&raw).expect("json body") // Safe: test assertion
        };
        (status, json)
    }

    const MODERN: &str = "2026-07-28";

    /// A modern body without the header used to be served as modern from the
    /// body alone; every modern POST must carry `MCP-Protocol-Version`.
    #[tokio::test]
    async fn a_modern_body_without_the_version_header_is_a_header_mismatch() {
        let body = json!({"jsonrpc":"2.0","id":4,"method":"tools/list",
                          "params": modern_params(MODERN, json!({}))});
        let (status, json) = post_versioned(None, &[], &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], HEADER_MISMATCH);
        assert_eq!(json["id"], 4, "the refusal echoes the request id");
    }

    /// A modern header over a body with no `_meta` used to be served as
    /// legacy; it is a malformed modern request.
    #[tokio::test]
    async fn a_modern_header_over_a_legacy_body_is_invalid_params() {
        let body = json!({"jsonrpc":"2.0","id":5,"method":"tools/list"});
        let (status, json) = post_versioned(Some(MODERN), &[], &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["error"]["code"], INVALID_PARAMS);
    }

    /// The header and the body's `_meta` must name the same revision.
    #[tokio::test]
    async fn a_header_naming_another_revision_than_the_body_is_refused() {
        let body = json!({"jsonrpc":"2.0","id":6,"method":"tools/list",
                          "params": modern_params(MODERN, json!({}))});
        let (status, json) = post_versioned(Some(PROTOCOL_VERSION), &[], &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], HEADER_MISMATCH);
        assert_eq!(json["id"], 6);
    }

    /// A repeated version header names no one revision.
    #[tokio::test]
    async fn a_repeated_version_header_is_refused() {
        let body = json!({"jsonrpc":"2.0","id":7,"method":"tools/list",
                          "params": modern_params(MODERN, json!({}))});
        let (status, json) =
            post_versioned(Some(MODERN), &[(PROTOCOL_VERSION_HEADER, MODERN)], &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], HEADER_MISMATCH);
    }

    /// A method the revision removed is a 404 method-not-found for a modern
    /// request, while a legacy client still gets `ping` answered.
    #[tokio::test]
    async fn a_removed_method_is_a_modern_404_and_still_served_to_legacy() {
        for method in ["ping", "initialize", "logging/setLevel"] {
            let body = json!({"jsonrpc":"2.0","id":8,"method":method,
                              "params": modern_params(MODERN, json!({}))});
            let (status, json) = post_versioned(Some(MODERN), &[], &body).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
            assert_eq!(json["error"]["code"], METHOD_NOT_FOUND, "{method}");
        }

        let legacy = json!({"jsonrpc":"2.0","id":9,"method":"ping"});
        let (status, json) = post_versioned(Some(PROTOCOL_VERSION), &[], &legacy).await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.get("result").is_some(), "{json}");
    }

    /// An `initialize`-era client keeps getting protocol errors under 200.
    #[tokio::test]
    async fn a_legacy_unknown_method_stays_a_200() {
        let body = json!({"jsonrpc":"2.0","id":10,"method":"nope/nothing"});
        let (status, json) = post_versioned(Some(PROTOCOL_VERSION), &[], &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
    }

    /// A tool whose `region` argument is mirrored into `Mcp-Param-Region`,
    /// and whose nested `target.shard` into `Mcp-Param-Shard`.
    struct RegionTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for RegionTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "run_query".to_owned(),
                description: "Runs a query in a region".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "region": { "type": "string", "x-mcp-header": "Region" },
                        "limit": { "type": "integer", "x-mcp-header": "Limit" },
                        "target": {
                            "type": "object",
                            "properties": {
                                "shard": { "type": "integer", "x-mcp-header": "Shard" }
                            }
                        },
                        "query": { "type": "string" }
                    }
                }),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(
            &self,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("ran".to_owned())
        }
    }

    fn make_region_app() -> Router {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(RegionTool));
        mcp_router(Arc::new(McpServer::new(
            "test",
            "0.1.0",
            registry,
            Arc::new(TestState),
        )))
    }

    /// A modern `tools/call` of `run_query` with `arguments`.
    fn region_call(arguments: &Value) -> Value {
        json!({"jsonrpc":"2.0","id":20,"method":"tools/call",
               "params": modern_params(MODERN, json!({"name":"run_query","arguments":arguments}))})
    }

    /// POST a modern `run_query` call carrying `headers` beside the version.
    async fn call_region(arguments: &Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        post_versioned_to(
            make_region_app(),
            Some(MODERN),
            headers,
            &region_call(arguments),
        )
        .await
    }

    /// The canonical conforming call: every mirror present and matching.
    #[tokio::test]
    async fn a_call_carrying_matching_mirrors_is_served() {
        let (status, json) = call_region(
            &json!({"region":"us-west1","limit":42,"target":{"shard":7},"query":"q"}),
            &[
                (MCP_NAME_HEADER, "run_query"),
                ("mcp-param-region", "us-west1"),
                ("Mcp-Param-Limit", "42.0"),
                ("mcp-param-shard", "7"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["result"]["content"][0]["text"], "ran");
    }

    /// Every SEP-2243 failure is a 400 `HeaderMismatchError` echoing the id.
    #[tokio::test]
    async fn header_mirror_failures_are_400_header_mismatch() {
        let args = json!({"region":"us-west1","query":"q"});
        let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
            ("Mcp-Name missing", vec![("mcp-param-region", "us-west1")]),
            (
                "Mcp-Name names another tool",
                vec![(MCP_NAME_HEADER, "other"), ("mcp-param-region", "us-west1")],
            ),
            (
                "Mcp-Method names another method",
                vec![
                    (MCP_METHOD_HEADER, "tools/list"),
                    (MCP_NAME_HEADER, "run_query"),
                    ("mcp-param-region", "us-west1"),
                ],
            ),
            (
                "Mcp-Param-Region missing",
                vec![(MCP_NAME_HEADER, "run_query")],
            ),
            (
                "Mcp-Param-Region disagrees",
                vec![
                    (MCP_NAME_HEADER, "run_query"),
                    ("mcp-param-region", "eu-west1"),
                ],
            ),
            (
                "Mcp-Param-Limit with no limit in the body",
                vec![
                    (MCP_NAME_HEADER, "run_query"),
                    ("mcp-param-region", "us-west1"),
                    ("mcp-param-limit", "1"),
                ],
            ),
            (
                "malformed Base64 padding",
                vec![
                    (MCP_NAME_HEADER, "run_query"),
                    ("mcp-param-region", "=?base64?dXMtd2VzdDE?="),
                ],
            ),
            (
                "repeated Mcp-Name",
                vec![
                    (MCP_NAME_HEADER, "run_query"),
                    (MCP_NAME_HEADER, "run_query"),
                    ("mcp-param-region", "us-west1"),
                ],
            ),
        ];
        for (case, headers) in cases {
            let (status, json) = call_region(&args, &headers).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {json}");
            assert_eq!(json["error"]["code"], HEADER_MISMATCH, "{case}");
            assert_eq!(json["id"], 20, "{case}");
        }
    }

    /// Mcp-Method is required on every modern request.
    #[tokio::test]
    async fn a_modern_request_without_mcp_method_is_refused() {
        let body = json!({"jsonrpc":"2.0","id":21,"method":"tools/list",
                          "params": modern_params(MODERN, json!({}))});
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header(PROTOCOL_VERSION_HEADER, MODERN)
            .body(body.to_string())
            .expect("request"); // Safe: test fixture
        let response = make_app().oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["error"]["code"], HEADER_MISMATCH);
    }

    /// The Base64 sentinel carries what a plain field value cannot, and is
    /// decoded before the comparison.
    #[tokio::test]
    async fn base64_wrapped_mirrors_are_decoded_before_comparison() {
        let (status, json) = call_region(
            &json!({"region":"Hello, 世界"}),
            &[
                (MCP_NAME_HEADER, "=?base64?cnVuX3F1ZXJ5?="),
                ("mcp-param-region", "=?base64?SGVsbG8sIOS4lueVjA==?="),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    /// An `Mcp-Param-*` value outside visible ASCII must travel wrapped.
    #[tokio::test]
    async fn a_param_header_with_raw_non_ascii_is_refused() {
        let body = region_call(&json!({"region":"x"}));
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", TEST_ACCEPT)
            .header(PROTOCOL_VERSION_HEADER, MODERN)
            .header(MCP_METHOD_HEADER, "tools/call")
            .header(MCP_NAME_HEADER, "run_query")
            .header(
                "mcp-param-region",
                header::HeaderValue::from_bytes(b"caf\xe9").expect("obs-text"), // Safe: test fixture
            )
            .body(body.to_string())
            .expect("request"); // Safe: test fixture
        let response = make_region_app().oneshot(request).await.expect("response"); // Safe: test assertion
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// An integer beyond the JavaScript safe range could not have been
    /// mirrored faithfully.
    #[tokio::test]
    async fn an_unsafe_integer_mirror_is_refused() {
        let (status, json) = call_region(
            &json!({"region":"r","limit":9_007_199_254_740_993_u64}),
            &[
                (MCP_NAME_HEADER, "run_query"),
                ("mcp-param-region", "r"),
                ("mcp-param-limit", "9007199254740993"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    }

    /// A legacy request needs no mirror, but one it carries must still match.
    #[tokio::test]
    async fn legacy_mirrors_are_optional_but_must_match() {
        let legacy = json!({"jsonrpc":"2.0","id":22,"method":"tools/call",
                            "params":{"name":"run_query","arguments":{"region":"us-west1"}}});
        let (status, json) = post(make_region_app(), &legacy.to_string(), &[]).await;
        assert_eq!(status, StatusCode::OK, "{json}");

        let (status, json) = post(
            make_region_app(),
            &legacy.to_string(),
            &[("mcp-param-region", "eu-west1")],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    }
}
