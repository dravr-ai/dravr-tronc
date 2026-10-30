// ABOUTME: Transport module providing stdio and HTTP backends for MCP communication
// ABOUTME: run picks one from McpArgs; each reads JSON-RPC requests and dispatches via McpServer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod error;
pub mod http;
pub mod mirror;
pub mod stdio;

use std::sync::Arc;

pub use error::TransportError;

use crate::mcp::server::McpServer;
use crate::server::cli::McpArgs;

/// The `--transport` value [`run`] serves over stdin/stdout.
pub const STDIO_TRANSPORT: &str = "stdio";

/// The `--transport` value [`run`] serves over HTTP.
pub const HTTP_TRANSPORT: &str = "http";

/// Serve `server` on the transport `args` names, until it stops.
///
/// - [`STDIO_TRANSPORT`] runs [`stdio::run`]: no socket, no gate — the
///   process was started by the client that talks to it.
/// - [`HTTP_TRANSPORT`] runs [`http::serve`] on `args.host:args.port`, with
///   every check it makes: a reachable bind is refused unless the server has
///   an [`AuthHook`](crate::mcp::auth::AuthHook), a loopback bind serves a
///   loopback `Host` only, and `SIGINT`/`SIGTERM` stop it gracefully.
///
/// A transport it does not know is refused with a typed error the binary's
/// `main` returns, not an `eprintln!` and `exit(1)`.
///
/// ```rust,ignore
/// let cli = Cli::parse();
/// let server = Arc::new(McpServer::new("svc", "1.0.0", registry, state));
/// dravr_tronc::mcp::transport::run(server, &cli.server).await?;
/// ```
///
/// `server` arrives with its gate already installed, so resolve that gate
/// only when `args.transport` is [`HTTP_TRANSPORT`]. Stdio opens no socket:
/// resolving a key gate there logs a posture that does not apply, and refuses
/// a keyless `--host` that stdio never binds.
///
/// ```rust,ignore
/// let gate = if cli.server.transport == HTTP_TRANSPORT {
///     shared_key_gate("SVC_API_KEY", "svc", &cli.server.host)?
/// } else {
///     None
/// };
/// let server = Arc::new(
///     McpServer::new("svc", "1.0.0", registry, state)
///         .with_optional_auth_hook(gate.map(|hook| Arc::new(hook) as _)),
/// );
/// dravr_tronc::mcp::transport::run(server, &cli.server).await?;
/// ```
///
/// # Errors
///
/// [`TransportError::UnknownTransport`] when `args.transport` is neither,
/// before anything is served; otherwise whatever the chosen transport
/// returns.
pub async fn run<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
    args: &McpArgs,
) -> Result<(), TransportError> {
    match args.transport.as_str() {
        STDIO_TRANSPORT => stdio::run(server).await,
        HTTP_TRANSPORT => http::serve(server, &args.host, args.port).await,
        other => Err(TransportError::UnknownTransport(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tool::ToolRegistry;

    fn server() -> Arc<McpServer<()>> {
        Arc::new(McpServer::new(
            "transport-run",
            "0.1.0",
            ToolRegistry::new(),
            Arc::new(()),
        ))
    }

    fn args(transport: &str, host: &str) -> McpArgs {
        McpArgs {
            transport: transport.to_owned(),
            host: host.to_owned(),
            port: 0,
        }
    }

    #[tokio::test]
    async fn an_unknown_transport_is_refused_by_name() {
        let refused = run(server(), &args("carrier-pigeon", "127.0.0.1")).await;

        match refused {
            Err(TransportError::UnknownTransport(name)) => {
                assert_eq!(name, "carrier-pigeon", "the refusal names what was asked");
            }
            other => panic!("expected UnknownTransport, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_transport_says_which_ones_exist() {
        let message = TransportError::UnknownTransport("sse".to_owned()).to_string();

        assert_eq!(
            message, "unknown transport 'sse': expected 'stdio' or 'http'",
            "the message names the transport and the accepted ones"
        );
    }

    #[test]
    fn the_transport_names_match_the_cli_default() {
        let defaults = <McpArgs as clap::Parser>::parse_from(["test"]);

        assert_eq!(
            defaults.transport, STDIO_TRANSPORT,
            "McpArgs defaults to the stdio transport run serves"
        );
    }

    /// `http` reaches `http::serve`, with its posture check intact: a server
    /// with no hook is refused on a reachable bind before any socket opens.
    #[tokio::test]
    async fn http_keeps_the_reachable_bind_refusal() {
        let refused = run(server(), &args(HTTP_TRANSPORT, "0.0.0.0")).await;

        match refused {
            Err(TransportError::InsecureBind(refused)) => {
                assert_eq!(refused.host, "0.0.0.0", "the refusal names the bind");
            }
            other => panic!("expected InsecureBind, got {other:?}"),
        }
    }
}
