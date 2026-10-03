// ABOUTME: McpTestServer — an McpServer bound to 127.0.0.1 on a port the system picks
// ABOUTME: Serves the router `serve` binds until dropped; hands out HTTP test clients for its URL
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::mcp::server::McpServer;
use crate::mcp::transport::http::guarded_mcp_router;
use crate::testkit::client::McpTestClient;

/// An [`McpServer`] listening on loopback, port 0, for a test that needs a
/// real socket — a second process, a client library, a conformance suite.
///
/// It serves the router [`serve`](crate::mcp::transport::http::serve)
/// binds, so an answer here is the answer in production. The server stops
/// when this is dropped.
#[derive(Debug)]
pub struct McpTestServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl McpTestServer {
    /// Bind `server` to `127.0.0.1:0` and start serving it.
    ///
    /// # Errors
    ///
    /// The bind failure.
    pub async fn start<S: Send + Sync + ?Sized + 'static>(
        server: Arc<McpServer<S>>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let app = guarded_mcp_router(server);
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(error = %e, %addr, "Test MCP server stopped");
            }
        });
        Ok(Self { addr, task })
    }

    /// The address it listens on.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL of its `/mcp` endpoint.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// An HTTP test client for it.
    #[must_use]
    pub fn client(&self) -> McpTestClient {
        McpTestClient::http(self.url())
    }
}

impl Drop for McpTestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
