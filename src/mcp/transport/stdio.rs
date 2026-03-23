// ABOUTME: Stdio transport reading newline-delimited JSON-RPC from stdin and writing to stdout
// ABOUTME: Standard MCP transport for integration with editors and CLI tool wrappers

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error};

use crate::mcp::protocol::{JsonRpcResponse, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;

/// Run the MCP server over stdin/stdout using newline-delimited JSON-RPC
///
/// Each line on stdin is expected to be a complete JSON-RPC message.
/// Responses are written as single lines to stdout. Logs must go to stderr
/// (configure tracing accordingly) to avoid polluting the protocol channel.
///
/// Blocks until stdin is closed or an I/O error occurs.
pub async fn run<S: Send + Sync + 'static>(
    server: Arc<McpServer<S>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    debug!(
        protocol_version = PROTOCOL_VERSION,
        "Stdio transport ready, waiting for JSON-RPC messages on stdin"
    );

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                if let Some(response) = server.handle_raw(&line).await {
                    write_response(&mut stdout, &response).await?;
                }
            }
            Ok(None) => {
                debug!("Stdin closed, shutting down stdio transport");
                break;
            }
            Err(e) => {
                error!(error = %e, "Stdin read error, shutting down stdio transport");
                return Err(format!("stdin read error: {e}").into());
            }
        }
    }
    Ok(())
}

/// Serialize and write a JSON-RPC response as a single line to stdout
async fn write_response(
    stdout: &mut tokio::io::Stdout,
    response: &JsonRpcResponse,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json =
        serde_json::to_string(response).map_err(|e| format!("JSON serialization failed: {e}"))?;

    stdout
        .write_all(json.as_bytes())
        .await
        .map_err(|e| format!("stdout write failed: {e}"))?;

    stdout
        .write_all(b"\n")
        .await
        .map_err(|e| format!("stdout newline write failed: {e}"))?;

    stdout
        .flush()
        .await
        .map_err(|e| format!("stdout flush failed: {e}"))?;

    Ok(())
}
