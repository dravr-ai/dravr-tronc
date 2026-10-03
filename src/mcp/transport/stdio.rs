// ABOUTME: Stdio transport reading newline-delimited JSON-RPC from stdin and writing to stdout
// ABOUTME: Standard MCP transport for integration with editors and CLI tool wrappers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::sync::Arc;

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tracing::{debug, error};

use crate::mcp::protocol::{JsonRpcResponse, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;

/// Run the MCP server over stdin/stdout using newline-delimited JSON-RPC
///
/// Each line on stdin is expected to be a complete JSON-RPC message.
/// Responses are written as single lines to stdout. Logs must go to stderr
/// (configure tracing accordingly) to avoid polluting the protocol channel.
///
/// Every message is served on its own task, so a long `tools/call` does not
/// hold up the next line — which is what lets a `notifications/cancelled` for
/// that call arrive while it runs. Responses are written as they complete, in
/// whatever order that is; JSON-RPC pairs them with requests by id.
///
/// Blocks until stdin is closed — and every request read by then is answered —
/// or an I/O error occurs.
pub async fn run<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    let (responses, mut outbox) = mpsc::unbounded_channel::<JsonRpcResponse>();
    let mut writer = tokio::spawn(async move {
        let mut stdout = io::stdout();
        while let Some(response) = outbox.recv().await {
            write_response(&mut stdout, &response).await?;
        }
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    });

    debug!(
        protocol_version = PROTOCOL_VERSION,
        "Stdio transport ready, waiting for JSON-RPC messages on stdin"
    );

    loop {
        let line = tokio::select! {
            // A writer that stopped early hit a stdout error; surface it now
            // rather than reading requests nobody can be answered on.
            written = &mut writer => return finish(written),
            line = lines.next_line() => line,
        };
        match line {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let server = Arc::clone(&server);
                let responses = responses.clone();
                tokio::spawn(async move {
                    if let Some(response) = server.handle_raw(&line).await {
                        if responses.send(response).is_err() {
                            debug!("Stdout writer gone, dropping a response");
                        }
                    }
                });
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
    // The writer drains until the last in-flight request drops its sender.
    drop(responses);
    finish(writer.await)
}

/// The stdout writer's outcome, with a panic or abort reported as an error.
fn finish(
    written: Result<Result<(), Box<dyn Error + Send + Sync>>, JoinError>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    written.map_err(|e| format!("stdout writer failed: {e}"))?
}

/// Serialize and write a JSON-RPC response as a single line to stdout
async fn write_response(
    stdout: &mut Stdout,
    response: &JsonRpcResponse,
) -> Result<(), Box<dyn Error + Send + Sync>> {
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
