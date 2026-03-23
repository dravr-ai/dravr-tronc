// ABOUTME: Shared CLI argument definitions for dravr-xxx server binaries
// ABOUTME: Provides ServerArgs (transport, host, port) for use with clap flatten

use clap::Parser;

/// Common server CLI arguments shared across all dravr-xxx server binaries
///
/// Use with `#[command(flatten)]` in your project-specific CLI struct:
///
/// ```rust,ignore
/// use clap::Parser;
/// use dravr_tronc::server::cli::ServerArgs;
///
/// #[derive(Parser)]
/// struct Cli {
///     #[command(flatten)]
///     server: ServerArgs,
///
///     /// My project-specific flag
///     #[arg(long)]
///     my_flag: bool,
/// }
/// ```
#[derive(Parser, Clone, Debug)]
pub struct ServerArgs {
    /// Transport mode: "stdio" for stdin/stdout or "http" for HTTP+SSE
    #[arg(long, default_value = "http")]
    pub transport: String,

    /// HTTP listen host (only used with --transport http)
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// HTTP listen port (only used with --transport http)
    #[arg(long, default_value_t = 3000)]
    pub port: u16,
}

/// Common MCP server CLI arguments (defaults to stdio transport)
///
/// Same as `ServerArgs` but with `stdio` as the default transport,
/// suitable for standalone MCP binary entry points.
#[derive(Parser, Clone, Debug)]
pub struct McpArgs {
    /// Transport mode: "stdio" for stdin/stdout or "http" for HTTP+SSE
    #[arg(long, default_value = "stdio")]
    pub transport: String,

    /// HTTP listen host (only used with --transport http)
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// HTTP listen port (only used with --transport http)
    #[arg(long, default_value_t = 3001)]
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_args_defaults() {
        let args = ServerArgs::parse_from::<[&str; 0], &str>([]);
        assert_eq!(args.transport, "http");
        assert_eq!(args.host, "127.0.0.1");
        assert_eq!(args.port, 3000);
    }

    #[test]
    fn server_args_override() {
        let args = ServerArgs::parse_from([
            "test",
            "--transport",
            "stdio",
            "--host",
            "0.0.0.0",
            "--port",
            "8080",
        ]);
        assert_eq!(args.transport, "stdio");
        assert_eq!(args.host, "0.0.0.0");
        assert_eq!(args.port, 8080);
    }

    #[test]
    fn mcp_args_defaults_to_stdio() {
        let args = McpArgs::parse_from::<[&str; 0], &str>([]);
        assert_eq!(args.transport, "stdio");
        assert_eq!(args.port, 3001);
    }

    #[test]
    fn mcp_args_override() {
        let args = McpArgs::parse_from(["test", "--transport", "http", "--port", "4000"]);
        assert_eq!(args.transport, "http");
        assert_eq!(args.port, 4000);
    }

    #[test]
    fn server_args_clone() {
        let args = ServerArgs::parse_from::<[&str; 0], &str>([]);
        let cloned = args.clone();
        assert_eq!(args.transport, cloned.transport);
        assert_eq!(args.host, cloned.host);
        assert_eq!(args.port, cloned.port);
    }

    #[test]
    fn server_args_debug() {
        let args = ServerArgs::parse_from::<[&str; 0], &str>([]);
        let debug = format!("{args:?}");
        assert!(debug.contains("http"));
        assert!(debug.contains("127.0.0.1"));
    }
}
