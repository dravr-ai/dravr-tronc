// ABOUTME: McpTestClient — MCP over the in-process router or HTTP, with bearer, headers, _meta, session
// ABOUTME: Reads a streamed answer event by event, answering the server's requests as a real client does
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error as StdError;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use crate::error::METHOD_NOT_FOUND;
use crate::mcp::modern::{meta_keys, PROTOCOL_VERSION_2026_07_28, PROTOCOL_VERSION_HEADER};
use crate::mcp::protocol::{JsonRpcError, JsonRpcResponse, JSONRPC_VERSION, PROTOCOL_VERSION};
use crate::mcp::schema::{Tool, ToolCall, ToolResponse};
use crate::mcp::server::McpServer;
use crate::mcp::transport::http::{guarded_mcp_router, MCP_SESSION_ID_HEADER};
use crate::mcp::transport::mirror::{
    encode_header_value, mirrored_name_field, MCP_METHOD_HEADER, MCP_NAME_HEADER,
};

/// The `clientInfo.name` the test client introduces itself with.
pub const TESTKIT_CLIENT_NAME: &str = "dravr-tronc-testkit";

/// What a Streamable HTTP client accepts: both renderings, unweighted, as the
/// specification has a client declare. The server answers a single response
/// as JSON on that tie, and a call that talks to its client first as an
/// event stream.
const ACCEPT_JSON_AND_SSE: &str = "application/json, text/event-stream";

/// What answers a request the server sends the client mid-call: given its
/// method and params, the result to send back, or the error.
pub type ServerRequestHandler =
    Arc<dyn Fn(&str, &Value) -> Result<Value, JsonRpcError> + Send + Sync>;

/// Where a request goes.
#[derive(Clone)]
enum Transport {
    /// Through a router, by `oneshot`, without a socket.
    InProcess(Router),
    /// Over HTTP to an `/mcp` URL.
    Http { url: String, http: reqwest::Client },
}

/// An MCP client for tests.
///
/// Every request it sends carries the bearer token, extra headers and `_meta`
/// keys set on it. Build it with [`Self::in_process`], [`Self::over_router`]
/// or [`Self::http`] (or
/// [`McpTestServer::client`](crate::testkit::McpTestServer::client)), then
/// set what every request should carry with the `with_*` builders. A client
/// is cheap to clone into a second one with different credentials.
///
/// A call the server answers with an event stream is read event by event:
/// its notifications are collected (see [`Self::exchange`]), and a request
/// the server sends mid-call is answered on the spot by the handler set with
/// [`Self::on_server_request`] — or refused as method-not-found without one,
/// as a client that cannot serve it would. The `Mcp-Session-Id` a server
/// hands out at `initialize` is sent on every later request, by this client
/// and every clone of it.
#[derive(Clone)]
pub struct McpTestClient {
    transport: Transport,
    bearer: Option<String>,
    headers: Vec<(String, String)>,
    meta: Map<String, Value>,
    next_id: Arc<AtomicI64>,
    capabilities: Value,
    session: Arc<Mutex<Option<String>>>,
    on_server_request: Option<ServerRequestHandler>,
}

impl fmt::Debug for McpTestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = match &self.transport {
            Transport::InProcess(_) => "in-process",
            Transport::Http { url, .. } => url.as_str(),
        };
        f.debug_struct("McpTestClient")
            .field("transport", &transport)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .field("headers", &self.headers)
            .field("meta", &self.meta)
            .field("capabilities", &self.capabilities)
            .field("session", &self.session_id())
            .finish_non_exhaustive()
    }
}

impl McpTestClient {
    /// A client for `server`, in-process: each request goes through the
    /// router [`serve`](crate::mcp::transport::http::serve) binds — origin
    /// check, auth hook, request guard and all — by `oneshot`, with no socket.
    pub fn in_process<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Self {
        Self::over_router(guarded_mcp_router(server))
    }

    /// A client for the host's own `router`, in-process: each request is a
    /// `POST /mcp` (or, for [`Self::end_session`], a `DELETE /mcp`) sent
    /// through it by `oneshot`, with no socket.
    ///
    /// For a host that merges [`mcp_router`](crate::mcp::transport::http::mcp_router)
    /// into an application of its own: the test then passes through whatever
    /// that application layers over `/mcp` — its auth middleware, CORS, the
    /// request guard — exactly as a client of the deployed service would.
    /// [`Self::in_process`] is this over the router
    /// [`serve`](crate::mcp::transport::http::serve) binds.
    ///
    /// ```rust,ignore
    /// let app = Router::new()
    ///     .merge(mcp_router(Arc::clone(&server)))
    ///     .layer(from_fn(require_host_key))
    ///     .layer(from_fn(guard_requests));
    /// let client = McpTestClient::over_router(app).with_header("x-host-key", "k");
    /// ```
    pub fn over_router(router: Router) -> Self {
        Self::over(Transport::InProcess(router))
    }

    /// A client for the `/mcp` endpoint at `url`.
    pub fn http(url: impl Into<String>) -> Self {
        Self::over(Transport::Http {
            url: url.into(),
            http: reqwest::Client::new(),
        })
    }

    fn over(transport: Transport) -> Self {
        Self {
            transport,
            bearer: None,
            headers: Vec::new(),
            meta: Map::new(),
            next_id: Arc::new(AtomicI64::new(1)),
            capabilities: json!({}),
            session: Arc::new(Mutex::new(None)),
            on_server_request: None,
        }
    }

    /// Declare these client capabilities in [`Self::initialize`] — say
    /// `{"sampling": {}, "elicitation": {}}` to let a tool ask the client.
    /// Empty unless set.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Answer every request the server sends mid-call with `handler`, given
    /// the request's method and params.
    #[must_use]
    pub fn on_server_request(
        mut self,
        handler: impl Fn(&str, &Value) -> Result<Value, JsonRpcError> + Send + Sync + 'static,
    ) -> Self {
        self.on_server_request = Some(Arc::new(handler));
        self
    }

    /// The `Mcp-Session-Id` the server handed out, if it has.
    #[must_use]
    pub fn session_id(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// End the session with `DELETE /mcp` and return the HTTP answer; the
    /// client stops sending the id once the server accepts.
    ///
    /// # Errors
    ///
    /// A header that is not valid HTTP, or a transport failure.
    pub async fn end_session(&self) -> Result<RawResponse, TestClientError> {
        let reply = self
            .send_with(Method::DELETE, String::new(), self.request_headers())
            .await?;
        let answer = reply.collect().await?;
        if answer.status.is_success() {
            *self.session.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        Ok(answer)
    }

    /// Send `Authorization: Bearer <token>` on every request.
    #[must_use]
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    /// Send this header on every request. An `Accept` set here replaces the
    /// client's own, which lists both renderings unweighted.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Put this key in the `_meta` of every request. A `_meta` key a call's
    /// own params carry wins over it.
    #[must_use]
    pub fn with_meta(mut self, key: impl Into<String>, value: Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Speak the modern stateless revision (`2026-07-28`): every request
    /// declares its protocol version, client identity and (empty) client
    /// capabilities in `_meta`, and the version in `MCP-Protocol-Version`.
    ///
    /// A client is legacy (`2025-11-25`, `initialize`-based) until this is
    /// called. Declare a capability with [`Self::with_meta`] under
    /// [`meta_keys::CLIENT_CAPABILITIES`] after calling this.
    #[must_use]
    pub fn modern(self) -> Self {
        self.with_meta(
            meta_keys::PROTOCOL_VERSION,
            json!(PROTOCOL_VERSION_2026_07_28),
        )
        .with_meta(
            meta_keys::CLIENT_INFO,
            json!({ "name": TESTKIT_CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") }),
        )
        .with_meta(meta_keys::CLIENT_CAPABILITIES, json!({}))
        .with_header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION_2026_07_28)
    }

    /// Run the legacy `initialize` handshake and return its result.
    ///
    /// # Errors
    ///
    /// A transport failure, a body that is not a JSON-RPC response, or the
    /// JSON-RPC error the server answered with.
    pub async fn initialize(&self) -> Result<Value, TestClientError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": self.capabilities,
            "clientInfo": { "name": TESTKIT_CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
        });
        self.result("initialize", Some(params)).await
    }

    /// `tools/list`, as the tool definitions it returned.
    ///
    /// # Errors
    ///
    /// As [`Self::initialize`], or [`TestClientError::Decode`] when `tools`
    /// is not a list of tool definitions.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, TestClientError> {
        let mut result = self.result("tools/list", None).await?;
        let tools = result.get_mut("tools").map_or(Value::Null, Value::take);
        serde_json::from_value(tools).map_err(TestClientError::Decode)
    }

    /// `tools/call` of `name` with `arguments`, as the tool's result.
    ///
    /// A tool that ran and failed is an `Ok` result with `is_error` set; an
    /// unknown tool, like any protocol error, is [`TestClientError::Rpc`].
    ///
    /// # Errors
    ///
    /// As [`Self::initialize`], or [`TestClientError::Decode`] when the
    /// result is not a tool result.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolResponse, TestClientError> {
        let params = serde_json::to_value(ToolCall::new(name, arguments))
            .map_err(|e| TestClientError::InvalidRequest(e.to_string()))?;
        let result = self.result("tools/call", Some(params)).await?;
        serde_json::from_value(result).map_err(TestClientError::Decode)
    }

    /// Send a JSON-RPC request for `method` and return its result.
    ///
    /// # Errors
    ///
    /// As [`Self::initialize`].
    pub async fn result(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, TestClientError> {
        let response = self.request(method, params).await?;
        match (response.result, response.error) {
            (_, Some(error)) => Err(TestClientError::Rpc(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(Value::Null),
        }
    }

    /// Send a JSON-RPC request for `method` and return the whole response,
    /// error included.
    ///
    /// # Errors
    ///
    /// A transport failure, or a body that is not a JSON-RPC response.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, TestClientError> {
        Ok(self.exchange(method, params).await?.response)
    }

    /// Send a JSON-RPC request for `method` and return everything the server
    /// sent back: the messages that came before the response — notifications,
    /// and requests this client answered — and the response itself.
    ///
    /// # Errors
    ///
    /// A transport failure, an answer that is not JSON-RPC, a stream that
    /// ended before the response, or a server request whose answer the server
    /// refused.
    pub async fn exchange(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Exchange, TestClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut request = json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "method": method });
        let params = self.with_client_meta(params);
        let mirrors = self.mirror_headers(method, params.as_ref());
        if let Some(params) = params {
            request["params"] = params;
        }
        let mut headers = self.request_headers();
        headers.extend(mirrors);
        let reply = self
            .send_with(Method::POST, request.to_string(), headers)
            .await?;
        if !is_event_stream(&reply.headers) {
            let answer = reply.collect().await?;
            return Ok(Exchange {
                messages: Vec::new(),
                response: answer.rpc()?,
            });
        }
        self.read_stream(reply).await
    }

    /// Read an event stream to its response, answering each server request
    /// on the way.
    async fn read_stream(&self, mut reply: Reply) -> Result<Exchange, TestClientError> {
        let mut buffer: Vec<u8> = Vec::new();
        let mut messages = Vec::new();
        loop {
            while let Some(event) = take_event(&mut buffer) {
                let Some(data) = event_data(&event) else {
                    continue;
                };
                let message: Value =
                    serde_json::from_str(&data).map_err(|_| TestClientError::NotJsonRpc {
                        status: reply.status,
                        body: data.clone(),
                    })?;
                if message.get("method").is_none() {
                    let response = serde_json::from_value(message).map_err(|_| {
                        TestClientError::NotJsonRpc {
                            status: reply.status,
                            body: data,
                        }
                    })?;
                    return Ok(Exchange { messages, response });
                }
                if message.get("id").is_some() {
                    self.answer(&message).await?;
                }
                messages.push(message);
            }
            match reply.body.chunk().await? {
                Some(chunk) => buffer.extend_from_slice(&chunk),
                None => {
                    return Err(TestClientError::NotJsonRpc {
                        status: reply.status,
                        body: String::from_utf8_lossy(&buffer).into_owned(),
                    });
                }
            }
        }
    }

    /// Answer the server's `request` with this client's handler, as a POST.
    async fn answer(&self, request: &Value) -> Result<(), TestClientError> {
        let method = request["method"].as_str().unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let outcome = self.on_server_request.as_ref().map_or_else(
            || {
                Err(JsonRpcError::new(
                    METHOD_NOT_FOUND,
                    format!("Method not found: {method}"),
                ))
            },
            |handler| handler(method, &params),
        );
        let mut answer = json!({ "jsonrpc": JSONRPC_VERSION, "id": request["id"] });
        match outcome {
            Ok(result) => answer["result"] = result,
            Err(error) => {
                answer["error"] = serde_json::to_value(error).map_err(TestClientError::Decode)?;
            }
        }
        let accepted = self
            .send_with(Method::POST, answer.to_string(), self.request_headers())
            .await?
            .collect()
            .await?;
        if accepted.status == StatusCode::ACCEPTED {
            Ok(())
        } else {
            Err(TestClientError::NotJsonRpc {
                status: accepted.status,
                body: accepted.body,
            })
        }
    }

    /// Send a JSON-RPC notification: no id, so no JSON-RPC answer, only the
    /// HTTP one (`202 Accepted` when the server takes it).
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub async fn notify(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<RawResponse, TestClientError> {
        let mut notification = json!({ "jsonrpc": JSONRPC_VERSION, "method": method });
        let params = self.with_client_meta(params);
        let mirrors = self.mirror_headers(method, params.as_ref());
        if let Some(params) = params {
            notification["params"] = params;
        }
        self.send(notification.to_string(), mirrors).await
    }

    /// POST `body` as it is — no id, no `_meta` added — with this client's
    /// headers, and return the HTTP answer whatever it is.
    ///
    /// # Errors
    ///
    /// A header that is not valid HTTP, or a transport failure.
    pub async fn raw(&self, body: impl Into<String>) -> Result<RawResponse, TestClientError> {
        self.send(body.into(), Vec::new()).await
    }

    /// POST `body` with this client's headers plus `mirrors`, and read the
    /// whole answer.
    async fn send(
        &self,
        body: String,
        mirrors: Vec<(String, String)>,
    ) -> Result<RawResponse, TestClientError> {
        let mut headers = self.request_headers();
        headers.extend(mirrors);
        self.send_with(Method::POST, body, headers)
            .await?
            .collect()
            .await
    }

    /// Send `body` as `method` with exactly `headers`, and return the answer
    /// as it starts to arrive. A session id the answer hands out is kept.
    async fn send_with(
        &self,
        method: Method,
        body: String,
        headers: Vec<(String, String)>,
    ) -> Result<Reply, TestClientError> {
        let reply = match &self.transport {
            Transport::InProcess(router) => {
                Self::in_process_reply(router.clone(), method, body, headers).await?
            }
            Transport::Http { url, http } => {
                Self::http_reply(http, url, method, body, headers).await?
            }
        };
        if let Some(session) = reply
            .headers
            .get(MCP_SESSION_ID_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.to_owned());
        }
        Ok(reply)
    }

    /// The SEP-2243 mirrors a conforming client sends with `method`:
    /// `Mcp-Method`, `Mcp-Name` for a method that names a target, and
    /// `MCP-Protocol-Version` when the params declare a revision in `_meta`
    /// and this client sets no such header itself.
    fn mirror_headers(&self, method: &str, params: Option<&Value>) -> Vec<(String, String)> {
        let mut mirrors = vec![(MCP_METHOD_HEADER.to_owned(), encode_header_value(method))];
        let name = mirrored_name_field(method)
            .and_then(|field| params.and_then(|p| p.get(field)))
            .and_then(Value::as_str);
        if let Some(name) = name {
            mirrors.push((MCP_NAME_HEADER.to_owned(), encode_header_value(name)));
        }
        let declared = params
            .and_then(|p| p.get("_meta"))
            .and_then(|meta| meta.get(meta_keys::PROTOCOL_VERSION))
            .and_then(Value::as_str);
        let has_header = self
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER));
        if let (Some(version), false) = (declared, has_header) {
            mirrors.push((PROTOCOL_VERSION_HEADER.to_owned(), version.to_owned()));
        }
        mirrors
    }

    /// The headers every request carries, in order.
    fn request_headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![(
            CONTENT_TYPE.as_str().to_owned(),
            "application/json".to_owned(),
        )];
        let own_accept = self
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(ACCEPT.as_str()));
        if !own_accept {
            headers.push((ACCEPT.as_str().to_owned(), ACCEPT_JSON_AND_SSE.to_owned()));
        }
        if let Some(token) = &self.bearer {
            headers.push((AUTHORIZATION.as_str().to_owned(), format!("Bearer {token}")));
        }
        if let Some(session) = self.session_id() {
            headers.push((MCP_SESSION_ID_HEADER.to_owned(), session));
        }
        headers.extend(self.headers.iter().cloned());
        headers
    }

    async fn in_process_reply(
        router: Router,
        method: Method,
        body: String,
        headers: Vec<(String, String)>,
    ) -> Result<Reply, TestClientError> {
        let mut builder = Request::builder().method(method).uri("/mcp");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(Body::from(body))
            .map_err(|e| TestClientError::InvalidRequest(e.to_string()))?;
        let response = match router.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        Ok(Reply {
            status: response.status(),
            headers: response.headers().clone(),
            body: ReplyBody::InProcess(response.into_body()),
        })
    }

    async fn http_reply(
        http: &reqwest::Client,
        url: &str,
        method: Method,
        body: String,
        headers: Vec<(String, String)>,
    ) -> Result<Reply, TestClientError> {
        let mut request = http.request(method, url).body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TestClientError::Transport(e.without_url().to_string()))?;
        Ok(Reply {
            status: response.status(),
            headers: response.headers().clone(),
            body: ReplyBody::Http(response),
        })
    }

    /// `params` with this client's `_meta` keys under `_meta`; a key the
    /// params already carry is kept. Params that are not an object are
    /// sent as they are.
    fn with_client_meta(&self, params: Option<Value>) -> Option<Value> {
        if self.meta.is_empty() {
            return params;
        }
        let mut params = params.unwrap_or_else(|| Value::Object(Map::new()));
        let Some(object) = params.as_object_mut() else {
            return Some(params);
        };
        let meta = object
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            for (key, value) in &self.meta {
                meta.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        Some(params)
    }
}

/// What a JSON-RPC request got back.
#[derive(Debug, Clone)]
pub struct Exchange {
    /// Every message the server sent before the response, in order:
    /// notifications, and the requests this client answered. Empty when the
    /// answer was not an event stream.
    pub messages: Vec<Value>,
    /// The response.
    pub response: JsonRpcResponse,
}

impl Exchange {
    /// The notifications named `method` among [`Self::messages`].
    #[must_use]
    pub fn notifications(&self, method: &str) -> Vec<&Value> {
        self.messages
            .iter()
            .filter(|m| m.get("id").is_none() && m["method"] == method)
            .collect()
    }

    /// The requests the server sent among [`Self::messages`].
    #[must_use]
    pub fn server_requests(&self) -> Vec<&Value> {
        self.messages
            .iter()
            .filter(|m| m.get("id").is_some())
            .collect()
    }
}

/// An answer whose body has not been read yet.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: ReplyBody,
}

/// The body of a [`Reply`], read chunk by chunk.
enum ReplyBody {
    InProcess(Body),
    Http(reqwest::Response),
}

impl ReplyBody {
    /// The next chunk of the body, `None` at its end.
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, TestClientError> {
        match self {
            Self::InProcess(body) => loop {
                match body.frame().await {
                    None => return Ok(None),
                    Some(Err(e)) => return Err(TestClientError::Transport(e.to_string())),
                    Some(Ok(frame)) => {
                        if let Ok(data) = frame.into_data() {
                            return Ok(Some(data.to_vec()));
                        }
                    }
                }
            },
            Self::Http(response) => response
                .chunk()
                .await
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .map_err(|e| TestClientError::Transport(e.without_url().to_string())),
        }
    }
}

impl Reply {
    /// Read the whole body.
    async fn collect(mut self) -> Result<RawResponse, TestClientError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.body.chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(RawResponse {
            status: self.status,
            headers: self.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

/// Whether `headers` declare a `text/event-stream` body.
fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

/// Take the first complete event — the text up to a blank line — off the
/// front of `buffer`.
fn take_event(buffer: &mut Vec<u8>) -> Option<String> {
    let end = buffer.windows(2).position(|pair| pair == b"\n\n")?;
    let event: Vec<u8> = buffer.drain(..end + 2).collect();
    Some(String::from_utf8_lossy(&event).into_owned())
}

/// The `data` of one event, its lines joined as the event-stream format
/// joins them; `None` for an event with none (a keep-alive comment).
fn event_data(event: &str) -> Option<String> {
    let lines: Vec<&str> = event
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// An HTTP answer as it arrived.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// The status code.
    pub status: StatusCode,
    /// The response headers.
    pub headers: HeaderMap,
    /// The body, as text.
    pub body: String,
}

impl RawResponse {
    /// The JSON the body carries — the body itself, or, when the server
    /// answered with an event stream, the data of its JSON-RPC response: the
    /// event without a `method`, which a stream carries last.
    ///
    /// # Errors
    ///
    /// [`TestClientError::NotJsonRpc`] when there is no JSON to read.
    pub fn json(&self) -> Result<Value, TestClientError> {
        if !is_event_stream(&self.headers) {
            return serde_json::from_str(&self.body).map_err(|_| self.not_json_rpc());
        }
        let mut buffer = self.body.clone().into_bytes();
        if !buffer.ends_with(b"\n\n") {
            buffer.extend_from_slice(b"\n\n");
        }
        let mut response = None;
        while let Some(event) = take_event(&mut buffer) {
            let parsed =
                event_data(&event).and_then(|data| serde_json::from_str::<Value>(&data).ok());
            if let Some(message) = parsed.filter(|m| m.get("method").is_none()) {
                response = Some(message);
            }
        }
        response.ok_or_else(|| self.not_json_rpc())
    }

    /// The body as a JSON-RPC response.
    ///
    /// # Errors
    ///
    /// [`TestClientError::NotJsonRpc`] when it is not one.
    pub fn rpc(&self) -> Result<JsonRpcResponse, TestClientError> {
        serde_json::from_value(self.json()?).map_err(|_| self.not_json_rpc())
    }

    fn not_json_rpc(&self) -> TestClientError {
        TestClientError::NotJsonRpc {
            status: self.status,
            body: self.body.clone(),
        }
    }
}

/// Why a test client call did not produce what it asked for.
#[derive(Debug)]
#[non_exhaustive]
pub enum TestClientError {
    /// The request could not be built (a header that is not valid HTTP,
    /// params that do not serialise).
    InvalidRequest(String),
    /// The request could not be sent or its answer read.
    Transport(String),
    /// The answer is not a JSON-RPC response.
    NotJsonRpc {
        /// Its status code.
        status: StatusCode,
        /// Its body.
        body: String,
    },
    /// The server answered with a JSON-RPC error.
    Rpc(JsonRpcError),
    /// The result is not the shape the method returns.
    Decode(serde_json::Error),
}

impl fmt::Display for TestClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(why) => write!(f, "could not build the request: {why}"),
            Self::Transport(why) => write!(f, "transport failed: {why}"),
            Self::NotJsonRpc { status, body } => {
                write!(f, "HTTP {status} without a JSON-RPC response: {body}")
            }
            Self::Rpc(error) => write!(f, "JSON-RPC error {}: {}", error.code, error.message),
            Self::Decode(e) => write!(f, "unexpected result shape: {e}"),
        }
    }
}

impl StdError for TestClientError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Decode(e) => Some(e),
            Self::InvalidRequest(_)
            | Self::Transport(_)
            | Self::NotJsonRpc { .. }
            | Self::Rpc(_) => None,
        }
    }
}
