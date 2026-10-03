// ABOUTME: Testkit for MCP servers built on tronc — a test client, a port-0 server, assertions
// ABOUTME: Feature `testkit`, for a consumer's dev-dependencies; drives a server the way a client does
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Test an [`McpServer`](crate::mcp::server::McpServer) over its real
//! transport instead of a hand-rolled JSON-RPC helper.
//!
//! - [`McpTestClient`] speaks MCP to a server either in-process — each
//!   request goes through the same axum router `serve` binds, or through the
//!   host's own router with its layers ([`McpTestClient::over_router`]), by
//!   `tower::ServiceExt::oneshot`, with no socket — or over HTTP to a URL. It
//!   carries a bearer token, extra headers and `_meta` keys on every request,
//!   speaks either protocol era, and offers `initialize`, `list_tools`,
//!   `call_tool`, a JSON-RPC `request`, `exchange` for the notifications a
//!   call streams before its response, and `raw` for a body the typed calls
//!   would never send. It keeps the `Mcp-Session-Id` a server hands out, and
//!   answers the requests a tool sends its client mid-call.
//! - [`McpTestServer`] binds a server to `127.0.0.1:0` and hands out clients
//!   for whichever port the system chose, for a test that needs a real socket.
//! - [`assert`] holds assertions on tool results and JSON-RPC responses, and
//!   [`assert::assert_tools_snapshot`], which pins a server's whole
//!   `tools/list` to a committed file.
//!
//! ```rust,ignore
//! use dravr_tronc::testkit::{assert::assert_tool_success, McpTestClient};
//!
//! let client = McpTestClient::in_process(Arc::new(server)).with_bearer("key");
//! let result = client.call_tool("greet", json!({ "name": "Pierre" })).await?;
//! assert_tool_success(&result);
//! ```

pub mod assert;
mod client;
mod server;

pub use client::{
    Exchange, McpTestClient, RawResponse, ServerRequestHandler, TestClientError,
    TESTKIT_CLIENT_NAME,
};
pub use server::McpTestServer;
