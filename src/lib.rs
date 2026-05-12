// ABOUTME: Root library for dravr-tronc shared MCP server infrastructure
// ABOUTME: Re-exports protocol types, server, transports, auth, health, CLI, and tracing modules
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # dravr-tronc
//!
//! Shared MCP server infrastructure for dravr-xxx microservices.
//! Provides JSON-RPC 2.0 protocol types, a generic `McpServer<S>`,
//! stdio/HTTP transports, bearer auth middleware, health check traits,
//! CLI argument parsing, and tracing initialization.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use dravr_tronc::mcp::{McpServer, ToolRegistry};
//! use dravr_tronc::server::cli::ServerArgs;
//!
//! let registry = ToolRegistry::<MyState>::new();
//! let state = Arc::new(RwLock::new(MyState::default()));
//! let server = Arc::new(McpServer::new("my-server", "0.1.0", registry, state));
//! ```

pub mod error;
pub mod mcp;
#[cfg(feature = "notifications")]
pub mod notifications;
#[cfg(feature = "notifications")]
pub mod notify;
pub mod server;

// Convenience re-exports
pub use mcp::protocol;
pub use mcp::server::McpServer;
pub use mcp::tool::{McpTool, ToolRegistry};
pub use mcp::transport;
