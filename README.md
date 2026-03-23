# dravr-tronc

Shared MCP server infrastructure for the [dravr](https://github.com/dravr-ai) microservice ecosystem.

Extracts the common Model Context Protocol (MCP) boilerplate — JSON-RPC 2.0 types, server dispatcher, transports, auth middleware, health checks, CLI args, and tracing setup — into a single reusable crate.

## Used by

- [dravr-canot](https://github.com/dravr-ai/dravr-canot) — Multi-platform messaging (Slack, WhatsApp, Discord, Messenger, Telegram)
- [dravr-embacle](https://github.com/dravr-ai/dravr-embacle) — Pluggable LLM provider proxy (12 CLI runners + OpenAI API)
- [dravr-sciotte](https://github.com/dravr-ai/dravr-sciotte) — Sport activity scraper (Strava, Garmin)
- [dravr-cageux](https://github.com/dravr-ai/dravr-cageux) — Sports science intelligence engine
- [dravr-commere](https://github.com/dravr-ai/dravr-commere) — Push notification service

## What it provides

| Module | Purpose |
|--------|---------|
| `mcp::protocol` | JSON-RPC 2.0 types: requests, responses, errors, MCP initialize/tools/call |
| `mcp::server` | Generic `McpServer<S>` dispatcher (initialize, tools/list, tools/call, ping) |
| `mcp::tool` | `McpTool<S>` trait + `ToolRegistry<S>` for tool discovery and dispatch |
| `mcp::transport::stdio` | Newline-delimited JSON over stdin/stdout |
| `mcp::transport::http` | Axum POST `/mcp` handler with SSE support |
| `server::auth` | Configurable bearer token middleware (env-var driven, constant-time comparison) |
| `server::health` | `HealthResponse` builder with status codes |
| `server::cli` | `ServerArgs` / `McpArgs` clap structs for `#[command(flatten)]` |
| `server::tracing_init` | stderr for stdio transport, stdout for HTTP |
| `error` | `ErrorResponse` for REST APIs + JSON-RPC error code constants |

## Quick start

```toml
[dependencies]
dravr-tronc = "0.1"
```

```rust
use std::sync::Arc;
use tokio::sync::RwLock;
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::ToolRegistry;

struct MyState { /* your domain state */ }

let registry = ToolRegistry::<MyState>::new();
// registry.register(Box::new(MyTool));
let state = Arc::new(RwLock::new(MyState {}));
let server = Arc::new(McpServer::new("my-server", "0.1.0", registry, state));

// Stdio transport
// dravr_tronc::mcp::transport::stdio::run(server).await?;

// HTTP transport
// dravr_tronc::mcp::transport::http::serve(server, "127.0.0.1", 3000).await?;
```

## Implementing a tool

```rust
use async_trait::async_trait;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::McpTool;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

struct GreetTool;

#[async_trait]
impl McpTool<MyState> for GreetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "greet".to_owned(),
            description: "Greet someone".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn execute(&self, _state: &Arc<RwLock<MyState>>, args: Value) -> CallToolResult {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("world");
        CallToolResult::text(format!("Hello, {name}!"))
    }
}
```

## Auth middleware

```rust
use axum::{middleware, Router};
use dravr_tronc::server::auth::require_auth;

let app = Router::new()
    .route("/api/endpoint", axum::routing::get(handler))
    .layer(middleware::from_fn(|req, next| {
        require_auth("MY_API_KEY_ENV", req, next)
    }));
```

Reads the API key from the specified environment variable on every request. If unset, all requests pass through (dev mode). Uses constant-time comparison.

## Architecture

Each consumer project follows the same 3-crate workspace pattern:

```
my-project/
├── src/                          # Core domain library
├── crates/my-project-mcp/        # MCP server (library + binary crate, powered by dravr-tronc)
│   ├── src/state.rs              # Project-specific ServerState
│   └── src/tools/                # Domain-specific McpTool<ServerState> implementations
└── crates/my-project-server/     # Unified REST API + MCP server (binary crate, powered by dravr-tronc)
    ├── src/router.rs             # Axum routes + mcp_router() merge
    ├── src/auth.rs               # Delegates to dravr_tronc::server::auth
    └── src/main.rs               # CLI + transport dispatch
```

dravr-tronc owns the generic infrastructure. Each project owns its domain state and tool implementations.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
