// ABOUTME: Tracing subscriber initialization shared across all dravr-xxx server binaries
// ABOUTME: Routes logs to stderr for stdio transport (keeps stdout clean for JSON-RPC)

/// Initialize the tracing subscriber based on the transport mode
///
/// - `"stdio"` transport: logs go to **stderr** (stdout is reserved for JSON-RPC)
/// - Any other transport: logs go to **stdout**
///
/// Reads `RUST_LOG` env var for filter directives, defaults to `"info"`.
pub fn init(transport: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if transport == "stdio" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}
