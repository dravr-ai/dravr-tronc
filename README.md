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
dravr-tronc = "1.0"
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
| `mcp::auth` | `AuthHook` seam — the host turns a request into a per-call `ToolContext` |
| `server::auth` | Bearer token middleware — env-var driven, constant-time comparison |
| `iam` *(feature `google-iam`)* | Google ID tokens, both ends — `IdTokenSource` to call, `require_google_id_token` to be called |
| `notifications::slack` *(feature `notifications`)* | Slack request-signature verification |
| `server::health` | `HealthResponse` builder with HTTP status codes |
| `server::cli` | `ServerArgs` / `McpArgs` — clap structs for `#[command(flatten)]` |
| `server::tracing_init` | Tracing subscriber — stderr for stdio, stdout for HTTP |
| `error` | `ErrorResponse` for REST APIs + JSON-RPC error code constants |

## Choosing an auth mechanism

There are five, and picking between them is the decision agents get wrong here — usually by
reaching for the shared bearer key because it is the one with no feature flag. **Pick by what
the caller can present, and pick per route.** A deployment changes; a caller's credential does
not. Two of the five are behind cargo features, and `default = []`, so out of the box you have
`server::auth::require_auth` and the `mcp::auth::AuthHook` seam and nothing else — that
availability is not an argument for using them.

| The caller is… | Use | Feature |
|---|---|---|
| another dravr workload on Google infrastructure | `iam::require_google_id_token` (callee) / `iam::IdTokenSource` (caller) | `google-iam` |
| a human, through a browser or an app | the host's own session auth; `mcp::auth::AuthHook` for MCP routes | — |
| a third-party webhook | that vendor's signature — e.g. `notifications::slack::SlackClient::verify_signature` — never a bearer | `notifications` |
| our own binary, over the wire, with nowhere to put a Google identity | `server::auth::require_auth` | — |
| on stdio, or a linked crate in the same process | nothing | — |

Mechanisms compose, so the question is never *which one* for a whole service. A server can verify
Google ID tokens on its service-to-service routes, run an `AuthHook` on `/mcp`, and leave
`/health` open, all at once.

Two rules regardless of which you pick:

- **`/health` stays unauthenticated.** A liveness probe cannot present a credential; gating it
  breaks the deployment the moment the credential is provisioned.
- **Put the gate on a router, not on each route.** `mcp_routes.layer(from_fn(require_auth))`
  covers the next route someone adds; a per-route layer does not, and four services in this
  fleet shipped a middleware that guarded nothing because it was attached where no request
  reached it.

### `server::auth::require_auth` fails open, deliberately

It reads the key from the environment variable you name, and **when that variable is unset every
request passes through**. That is development mode, and it is load-bearing for stdio and local
runs — two tests pin the behaviour so nobody "fixes" it into a refusal and breaks them.

```rust
// Enforced only when MY_API_KEY is set. Constant-time comparison via subtle::ConstantTimeEq.
dravr_tronc::server::auth::require_auth("MY_API_KEY", request, next).await
```

The cost is that a config slip turns a private service public and looks like a healthy boot.
`resolve_startup_auth` is how you refuse that at startup instead:

```rust
use dravr_tronc::server::auth::{api_key_configured, resolve_startup_auth};

// gated by a shared key
let mode = resolve_startup_auth(&args.host, api_key_configured("MY_API_KEY"))?;
// gated by Google ID tokens — no key exists to check; the middleware is the gate
let mode = resolve_startup_auth(&args.host, true)?;
```

Its second argument is **whether anything at all authenticates requests on this bind** — not
whether a key is set. Read it as "is there a gate", and an identity-gated service that holds no
key still binds `0.0.0.0`, which every Cloud Run container must.

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
