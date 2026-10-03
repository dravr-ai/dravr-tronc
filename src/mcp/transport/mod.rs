// ABOUTME: Transport module providing stdio and HTTP backends for MCP communication
// ABOUTME: Each transport reads JSON-RPC requests, dispatches via McpServer, and writes responses
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod http;
pub mod mirror;
pub mod stdio;
