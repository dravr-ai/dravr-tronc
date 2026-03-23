// ABOUTME: Transport module providing stdio and HTTP backends for MCP communication
// ABOUTME: Each transport reads JSON-RPC requests, dispatches via McpServer, and writes responses

pub mod http;
pub mod stdio;
