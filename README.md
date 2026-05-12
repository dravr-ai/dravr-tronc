# dravr-tronc

A lightweight Rust runtime for building [MCP](https://modelcontextprotocol.io/) servers with optional REST API support. Provides everything you need to go from zero to a production-ready MCP server in minutes — JSON-RPC 2.0 protocol, stdio and HTTP transports, bearer auth, health checks, CLI parsing, and structured tracing.

## Why dravr-tronc?

Building an MCP server in Rust means writing the same boilerplate every time: JSON-RPC 2.0 types, request dispatching, transport layers, auth middleware, CLI args. dravr-tronc extracts all of that into a single crate so you only write your domain logic.

- **Generic over state** — `McpServer<S>` works with any `Send + Sync` state type
- **Two transports** — stdio (for editor/CLI integration) and HTTP with SSE (for web clients)
- **Zero configuration** — sensible defaults, env-var driven auth, plug and play
- **Production ready** — constant-time auth, structured tracing, health checks, 76 tests
- **Minimal dependencies** — axum, tokio, serde, clap, tracing (no framework lock-in)

## Quick start

```toml
[dependencies]
dravr-tronc = "0.3"
```

### 1. Define your state and tools

```rust
use std::sync::Arc;
use async_trait::async_trait;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::{McpTool, McpServer, ToolRegistry};
use serde_json::{json, Value};
use tokio::sync::RwLock;

struct AppState {
    greeting: String,
}

struct GreetTool;

#[async_trait]
impl McpTool<AppState> for GreetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "greet".to_owned(),
            description: "Greet someone by name".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn execute(&self, state: &Arc<RwLock<AppState>>, args: Value) -> CallToolResult {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("world");
        let greeting = state.read().await.greeting.clone();
        CallToolResult::text(format!("{greeting}, {name}!"))
    }
}
```

### 2. Wire it up

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dravr_tronc::server::tracing_init::init("stdio");

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GreetTool));

    let state = Arc::new(RwLock::new(AppState {
        greeting: "Hello".to_owned(),
    }));
    let server = Arc::new(McpServer::new("my-mcp-server", "0.1.0", registry, state));

    // Serve over stdin/stdout (for Claude Desktop, Cursor, etc.)
    dravr_tronc::mcp::transport::stdio::run(server).await
}
```

### 3. Or serve over HTTP

```rust
// Serve over HTTP with SSE support
dravr_tronc::mcp::transport::http::serve(server, "127.0.0.1", 3000).await?;
```

### 4. Merge into an existing Axum app

```rust
use dravr_tronc::mcp::transport::http::mcp_router;

let app = axum::Router::new()
    .route("/health", axum::routing::get(health_handler))
    .route("/api/data", axum::routing::get(data_handler))
    .merge(mcp_router(server))  // adds POST /mcp
    .layer(axum::middleware::from_fn(|req, next| {
        dravr_tronc::server::auth::require_auth("MY_API_KEY", req, next)
    }));
```

## Modules

| Module | Purpose |
|--------|---------|
| `mcp::protocol` | JSON-RPC 2.0 types — requests, responses, errors, MCP initialize/tools/call |
| `mcp::server` | Generic `McpServer<S>` — dispatches initialize, tools/list, tools/call, ping |
| `mcp::tool` | `McpTool<S>` trait + `ToolRegistry<S>` — define and register tools |
| `mcp::transport::stdio` | Newline-delimited JSON over stdin/stdout |
| `mcp::transport::http` | Axum POST `/mcp` handler with SSE (Streamable HTTP) |
| `server::auth` | Bearer token middleware — env-var driven, constant-time comparison |
| `server::health` | `HealthResponse` builder with HTTP status codes |
| `server::cli` | `ServerArgs` / `McpArgs` — clap structs for `#[command(flatten)]` |
| `server::tracing_init` | Tracing subscriber — stderr for stdio, stdout for HTTP |
| `error` | `ErrorResponse` for REST APIs + JSON-RPC error code constants |

## Auth middleware

Reads the API key from the environment variable you specify. If the variable is unset, all requests pass through (development mode). Uses `subtle::ConstantTimeEq` to prevent timing attacks.

```rust
// Only enforced when MY_API_KEY env var is set
dravr_tronc::server::auth::require_auth("MY_API_KEY", request, next).await
```

## Health checks

```rust
use dravr_tronc::server::health::HealthResponse;

let resp = HealthResponse::ok("my-service", "1.0.0")
    .with_detail("database", "connected")
    .with_detail("cache", "warm");
// Returns 200 for "ok", 503 for "degraded"
```

## CLI args

Flatten shared args into your project's CLI struct:

```rust
use clap::Parser;
use dravr_tronc::server::cli::McpArgs;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    server: McpArgs,  // adds --transport, --host, --port

    #[arg(long)]
    my_custom_flag: bool,
}
```

## Recommended project layout

```
my-project/
├── src/                          # Core domain library
├── crates/my-project-mcp/        # MCP server (library + binary)
│   ├── src/state.rs              # Your ServerState
│   ├── src/tools/                # Your McpTool<ServerState> implementations
│   └── src/main.rs               # Thin entry point using dravr-tronc
└── crates/my-project-server/     # REST API + MCP unified server (binary)
    ├── src/router.rs             # Axum routes + mcp_router() merge
    └── src/main.rs               # CLI + transport dispatch
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
