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
dravr-tronc = "2.1"
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

`serve` serves only `/mcp`, so the server's `AuthHook` is the only thing that can authenticate a
request there. **With no hook it serves loopback only**: it resolves the host first and refuses —
with an `InsecureBindError`, before any socket opens — unless every address the host resolves to
is loopback. To serve a reachable interface, attach a hook; for a shared key that is
`mcp::auth::ApiKeyAuthHook` (see [Startup posture](#startup-posture-refuse-a-reachable-bind-nothing-gates)).

### 4. Merge into an existing Axum app

```rust
use dravr_tronc::mcp::transport::http::mcp_router;
use dravr_tronc::server::request_guard::guard_requests;

let app = axum::Router::new()
    .route("/health", axum::routing::get(health_handler))
    .route("/api/data", axum::routing::get(data_handler))
    .merge(mcp_router(server))  // adds POST /mcp
    .layer(axum::middleware::from_fn(|req, next| {
        dravr_tronc::server::auth::require_auth("MY_API_KEY", req, next)
    }))
    .layer(axum::middleware::from_fn(guard_requests));  // outermost, once
```

`serve` in step 3 layers `guard_requests` itself; an app that assembles its own router adds it
once, last. See [Request guard](#request-guard).

## Modules

| Module | Purpose |
|--------|---------|
| `mcp::protocol` | JSON-RPC 2.0 types — requests, responses, errors, MCP initialize/tools/call |
| `mcp::server` | Generic `McpServer<S>` — dispatches initialize, tools/list, tools/call, ping |
| `mcp::tool` | `McpTool<S>` trait + `ToolRegistry<S>` — define and register tools |
| `mcp::computation` *(feature `computation`)* | `Computation` — a tool stated as one typed operation: schema generated from its input type, result written at each number's own precision |
| `mcp::transport::stdio` | Newline-delimited JSON over stdin/stdout |
| `mcp::transport::http` | Axum POST `/mcp` handler with SSE (Streamable HTTP) |
| `mcp::auth` | `AuthHook` seam — the host turns a request into a per-call `ToolContext`; `ApiKeyAuthHook`, the shared-key hook |
| `server::auth` | Bearer token middleware — env-var driven, constant-time comparison; `startup_auth` / `resolve_startup_auth`, the startup posture check |
| `http_client` *(feature `http-client`)* | `describe_request_error` — a `reqwest::Error` as text without its URL, which can carry a credential |
| `iam` *(feature `google-iam`)* | Google ID tokens, both ends — `IdTokenSource` to call, `require_google_id_token` to be called; `GoogleKeySet`, the cache of Google's signing keys for one key-set URL, refetching for an unknown `kid` at most every 30s |
| `notifications::slack` *(feature `notifications`)* | Slack request-signature verification |
| `notifications::ResendClient` *(feature `notifications`)* | The one Resend send path — alert and transactional mail — retrying a `429` within the advertised reset |
| `server::request_guard` | Request ids, panic → JSON `500`, per-router deadline → JSON `504`, completion and dropped-request logs |
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
| our own binary, over the wire, with nowhere to put a Google identity | `server::auth::require_auth` for REST routes, `mcp::auth::ApiKeyAuthHook` for MCP routes | — |
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
The startup posture check is how you refuse that instead.

### Startup posture: refuse a reachable bind nothing gates

For a service gated by one shared key, `startup_auth` resolves the posture and logs it — INFO
when the key is set, WARN when it is serving unauthenticated on loopback — and refuses a
reachable bind without the key, naming the variable to set:

```rust
use dravr_tronc::server::auth::{startup_auth, AuthMode};
use dravr_tronc::mcp::auth::ApiKeyAuthHook;

let mode = startup_auth("MY_API_KEY", &args.host)?;

// MCP routes: the hook fails closed, so attach it only when the key is set.
let server = match mode {
    AuthMode::Enforced => server.with_auth_hook(Arc::new(ApiKeyAuthHook::new("MY_API_KEY", "my-service"))),
    AuthMode::LoopbackDev => server,
};
```

`ApiKeyAuthHook` answers a missing or wrong key with the MCP refusal — `401`,
`WWW-Authenticate: Bearer realm="my-service"` and a JSON-RPC error body — which a REST middleware
layered over `/mcp` does not. It reads the key per request and admits the holder with
`auth_method = "api_key"`, never as an admin.

A service gated by something other than one key calls `resolve_startup_auth` with its own gate:

```rust
use dravr_tronc::server::auth::resolve_startup_auth;

// gated by Google ID tokens — no key exists to check; the middleware is the gate
let mode = resolve_startup_auth(&args.host, true)?;
```

Its second argument is **whether anything at all authenticates requests on this bind** — not
whether a key is set. Read it as "is there a gate", and an identity-gated service that holds no
key still binds `0.0.0.0`, which every Cloud Run container must.

**What counts as loopback.** `127.0.0.0/8`, `::1` (bracketed or not) and `localhost` in any case —
RFC 6761 §6.3 reserves `localhost` for loopback, and browsers hard-wire it there. Everything else,
`0.0.0.0` and `::` included, is reachable, and so is any other name: the text check fails closed.
A name is still only a name when it is bound, because the system resolver answers it — `localhost`
with `127.0.0.1`, `::1` or both, and on a misconfigured machine with anything. So the text check is
a judgement made before any socket exists, and `http::serve` goes further: it judges every address
the host resolves to before binding.

## Request guard

A handler that panics, or that runs past a deadline, must answer the caller — never leave it with
a connection that closed before any response, which it cannot tell apart from a network fault.
`server::request_guard` holds the two middlewares that make that true:

```rust
use std::time::Duration;

use axum::middleware::from_fn;
use dravr_tronc::server::request_guard::{enforce_deadline, guard_requests};

let deadline = Duration::from_secs(300);
let app = rest_routes
    .layer(from_fn(move |req, next| enforce_deadline(deadline, req, next)))
    .merge(mcp_router(server))           // no deadline on /mcp
    .layer(from_fn(guard_requests));     // outermost, once
```

- **`guard_requests`** gives every request an id — the caller's `x-request-id` when it is 1–128
  characters of `[A-Za-z0-9-_.:]`, a minted one otherwise — echoes it on the response, and puts it
  in the request's extensions as `RequestId`. It logs one INFO line per request (`method`, route
  template as `path`, `status`, `latency_ms`, `request_id`; never the concrete path or its query,
  which carry ids). A handler panic becomes a `500` with
  `{"error":{"type":"handler_panic",…}}` and an ERROR line carrying the panic payload. A request
  dropped before it answered — the client closed the connection mid-request, or the server went
  down — is logged at WARN with how long it ran.
- **`enforce_deadline`** answers a `504` with `{"error":{"type":"request_timeout",…}}` when the
  handler is still running at the deadline, and drops the handler. Put it on the router that holds
  one-shot REST routes, sized above the slowest legitimate request and below the caller's own
  client timeout, so the caller gets this answer instead of giving up first. Never on `/mcp` (a
  tool call is dispatched whole before it answers) or on a WebSocket or streamed response.

**A contained panic needs `panic = "unwind"`.** Under `panic = "abort"` the first panic terminates
the process and every in-flight request with it; no middleware can answer anything there.

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
