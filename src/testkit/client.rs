// ABOUTME: McpTestClient — MCP over the in-process router or HTTP, with bearer, headers and _meta
// ABOUTME: Typed initialize/list_tools/call_tool, a JSON-RPC request, and raw bodies with their status
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error as StdError;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use crate::mcp::modern::{meta_keys, PROTOCOL_VERSION_2026_07_28, PROTOCOL_VERSION_HEADER};
use crate::mcp::protocol::{JsonRpcError, JsonRpcResponse, JSONRPC_VERSION, PROTOCOL_VERSION};
use crate::mcp::schema::{Tool, ToolResponse};
use crate::mcp::server::McpServer;
use crate::mcp::transport::http::guarded_mcp_router;
use crate::mcp::transport::mirror::{
    encode_header_value, mirrored_name_field, MCP_METHOD_HEADER, MCP_NAME_HEADER,
};

/// The `clientInfo.name` the test client introduces itself with.
pub const TESTKIT_CLIENT_NAME: &str = "dravr-tronc-testkit";

/// What a Streamable HTTP client accepts: both renderings, as the transport
/// requires a client to declare.
const ACCEPT_JSON_AND_SSE: &str = "application/json, text/event-stream";

/// Where a request goes.
#[derive(Clone)]
enum Transport {
    /// Through the router `serve` binds, without a socket.
    InProcess(Router),
    /// Over HTTP to an `/mcp` URL.
    Http { url: String, http: reqwest::Client },
}

/// An MCP client for tests.
///
/// Every request it sends carries the bearer token, extra headers and `_meta`
/// keys set on it. Build it with [`Self::in_process`] or [`Self::http`] (or
/// [`McpTestServer::client`](crate::testkit::McpTestServer::client)), then
/// set what every request should carry with the `with_*` builders. A client
/// is cheap to clone into a second one with different credentials.
#[derive(Clone)]
pub struct McpTestClient {
    transport: Transport,
    bearer: Option<String>,
    headers: Vec<(String, String)>,
    meta: Map<String, Value>,
    next_id: Arc<AtomicI64>,
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
            .finish_non_exhaustive()
    }
}

impl McpTestClient {
    /// A client for `server`, in-process: each request goes through the
    /// router [`serve`](crate::mcp::transport::http::serve) binds — origin
    /// check, auth hook, request guard and all — by `oneshot`, with no socket.
    pub fn in_process<S: Send + Sync + ?Sized + 'static>(server: Arc<McpServer<S>>) -> Self {
        Self::over(Transport::InProcess(guarded_mcp_router(server)))
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
        }
    }

    /// Send `Authorization: Bearer <token>` on every request.
    #[must_use]
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    /// Send this header on every request.
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
            "capabilities": {},
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
        let params = json!({ "name": name, "arguments": arguments });
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut request = json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "method": method });
        let params = self.with_client_meta(params);
        let mirrors = self.mirror_headers(method, params.as_ref());
        if let Some(params) = params {
            request["params"] = params;
        }
        self.send(request.to_string(), mirrors).await?.rpc()
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

    /// POST `body` with this client's headers plus `mirrors`.
    async fn send(
        &self,
        body: String,
        mirrors: Vec<(String, String)>,
    ) -> Result<RawResponse, TestClientError> {
        let mut headers = self.request_headers();
        headers.extend(mirrors);
        match &self.transport {
            Transport::InProcess(router) => {
                Self::raw_in_process(router.clone(), body, headers).await
            }
            Transport::Http { url, http } => Self::raw_http(http, url, body, headers).await,
        }
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
        let mut headers = vec![
            (
                CONTENT_TYPE.as_str().to_owned(),
                "application/json".to_owned(),
            ),
            (ACCEPT.as_str().to_owned(), ACCEPT_JSON_AND_SSE.to_owned()),
        ];
        if let Some(token) = &self.bearer {
            headers.push((AUTHORIZATION.as_str().to_owned(), format!("Bearer {token}")));
        }
        headers.extend(self.headers.iter().cloned());
        headers
    }

    async fn raw_in_process(
        router: Router,
        body: String,
        headers: Vec<(String, String)>,
    ) -> Result<RawResponse, TestClientError> {
        let mut builder = Request::builder().method(Method::POST).uri("/mcp");
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
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| TestClientError::Transport(e.to_string()))?
            .to_bytes();
        Ok(RawResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn raw_http(
        http: &reqwest::Client,
        url: &str,
        body: String,
        headers: Vec<(String, String)>,
    ) -> Result<RawResponse, TestClientError> {
        let mut request = http.post(url).body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TestClientError::Transport(e.without_url().to_string()))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .text()
            .await
            .map_err(|e| TestClientError::Transport(e.without_url().to_string()))?;
        Ok(RawResponse {
            status,
            headers,
            body,
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
    /// The JSON the body carries — the body itself, or the data of its first
    /// event when the server answered with an event stream.
    ///
    /// # Errors
    ///
    /// [`TestClientError::NotJsonRpc`] when there is no JSON to read.
    pub fn json(&self) -> Result<Value, TestClientError> {
        let is_sse = self
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let payload = if is_sse {
            first_event_data(&self.body)
        } else {
            self.body.clone()
        };
        serde_json::from_str(&payload).map_err(|_| self.not_json_rpc())
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

/// The `data` of the first event of a `text/event-stream` body, its lines
/// joined as the event-stream format joins them.
fn first_event_data(body: &str) -> String {
    body.lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why a test client call did not produce what it asked for.
#[derive(Debug)]
pub enum TestClientError {
    /// The request could not be built (a header that is not valid HTTP).
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
