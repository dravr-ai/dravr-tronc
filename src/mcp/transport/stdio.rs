// ABOUTME: Stdio transport reading newline-delimited JSON-RPC from stdin and writing to stdout
// ABOUTME: Interleaves a call's notifications and server requests with responses; routes the client's answers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::mcp::client_channel::{CallerKey, ClientChannel, ClientConnection, PendingRequests};
use crate::mcp::protocol::{JsonRpcMessage, PROTOCOL_VERSION};
use crate::mcp::server::McpServer;
use crate::mcp::session::Session;
use crate::mcp::tool::ToolContext;

/// How long [`run`] waits, once stdin closes, for the requests it already
/// read to be answered.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the MCP server over stdin/stdout using newline-delimited JSON-RPC
///
/// Each line on stdin is expected to be a complete JSON-RPC message.
/// Responses are written as single lines to stdout. Logs must go to stderr
/// (configure tracing accordingly) to avoid polluting the protocol channel.
///
/// Every message is served on its own task, so a long `tools/call` does not
/// hold up the next line — which is what lets a `notifications/cancelled` for
/// that call arrive while it runs, and the client's answer to a request the
/// call sent it. Responses are written as they complete, in whatever order
/// that is; JSON-RPC pairs them with requests by id. A running call's
/// notifications and requests (see
/// [`ClientChannel`]) go out on the
/// same stream, between them.
///
/// The connection is one session: what the client declares in `initialize`
/// and sets with `logging/setLevel` holds for every later request on it.
///
/// Returns once stdin is closed, or on an I/O error. Closing stdin fails
/// every server request still awaiting the client's answer, since none can
/// arrive, then waits up to 10 seconds for the requests read by then to be
/// answered; one still running after that is dropped unanswered. Stdout then
/// takes nothing more, and the connection's cancellation fires: a job a tool
/// left running with a clone of its context sees it on
/// [`ToolContext::cancellation`], and holds the transport open no longer.
pub async fn run<S: Send + Sync + ?Sized + 'static>(
    server: Arc<McpServer<S>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    debug!(
        protocol_version = PROTOCOL_VERSION,
        "Stdio transport ready, waiting for JSON-RPC messages on stdin"
    );
    serve_lines(
        server,
        BufReader::new(io::stdin()),
        io::stdout(),
        DRAIN_TIMEOUT,
    )
    .await
}

/// [`run`] over any line reader and writer, waiting up to `drain` at the end
/// of input for the requests in flight.
pub(crate) async fn serve_lines<S, R, W>(
    server: Arc<McpServer<S>>,
    reader: R,
    writer: W,
    drain: Duration,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    S: Send + Sync + ?Sized + 'static,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    let (outbound, outbox) = mpsc::unbounded_channel::<JsonRpcMessage>();
    let stop_writing = CancellationToken::new();
    let mut writer = tokio::spawn(write_lines(writer, outbox, stop_writing.clone()));

    // Everyone on a stdio connection is the one anonymous caller, in the
    // one session the connection is.
    let caller = ToolContext::default();
    let connection_closed = caller.cancellation.clone();
    let session = Session::connection();
    let pending = Arc::new(PendingRequests::new());
    let answers_from = CallerKey::connection(&caller);
    let connection = ClientConnection::new(
        Some(outbound.clone()),
        Arc::clone(&pending),
        Some(Arc::clone(&session)),
        answers_from.clone(),
        server.client_request_timeout(),
    );
    let ctx = ToolContext {
        client: ClientChannel::connected(connection),
        ..caller
    };

    let mut requests = JoinSet::new();
    let read = loop {
        let line = tokio::select! {
            // A writer that stopped early hit a stdout error; surface it now
            // rather than reading requests nobody can be answered on.
            written = &mut writer => {
                pending.close();
                connection_closed.cancel();
                requests.shutdown().await;
                return finish(written);
            }
            Some(finished) = requests.join_next() => {
                if let Err(e) = finished {
                    error!(error = %e, "A stdio request task failed");
                }
                continue;
            }
            line = lines.next_line() => line,
        };
        match line {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match JsonRpcMessage::parse(&line) {
                    Ok(JsonRpcMessage::Response(response)) => {
                        let delivered = pending.deliver(&answers_from, response);
                        debug!(delivered, "Client answered a server request");
                    }
                    Ok(JsonRpcMessage::Request(request)) => {
                        let server = Arc::clone(&server);
                        let outbound = outbound.clone();
                        let ctx = ctx.clone();
                        requests.spawn(async move {
                            if let Some(response) =
                                server.handle_request_with_context(request, &ctx).await
                            {
                                if outbound.send(JsonRpcMessage::Response(response)).is_err() {
                                    debug!("Stdout writer gone, dropping a response");
                                }
                            }
                        });
                    }
                    Err(refusal) => {
                        if outbound.send(JsonRpcMessage::Response(*refusal)).is_err() {
                            debug!("Stdout writer gone, dropping a refusal");
                        }
                    }
                }
            }
            Ok(None) => {
                debug!("Stdin closed, shutting down stdio transport");
                break Ok(());
            }
            Err(e) => {
                error!(error = %e, "Stdin read error, shutting down stdio transport");
                break Err(format!("stdin read error: {e}"));
            }
        }
    };
    // No answer can arrive any more: a call still waiting for one fails now
    // instead of holding the shutdown until its timeout.
    pending.close();
    // The requests already read are answered, within the bound; whatever a
    // tool left running with a clone of its context is not waited for.
    let drained = timeout(drain, async {
        while requests.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        warn!(
            unanswered = requests.len(),
            "Stdin closed; dropping the requests still running after the drain"
        );
        requests.shutdown().await;
    }
    // Stdout takes what is queued and nothing after, then the connection's
    // cancellation reaches whatever still holds a context.
    stop_writing.cancel();
    let written = writer.await;
    connection_closed.cancel();
    read?;
    finish(written)
}

/// Write each message `outbox` carries as a line on `writer` until
/// `stop` fires, then the ones already queued, and nothing after them.
async fn write_lines<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut outbox: mpsc::UnboundedReceiver<JsonRpcMessage>,
    stop: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    loop {
        let message = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            message = outbox.recv() => message,
        };
        let Some(message) = message else {
            return Ok(());
        };
        write_message(&mut writer, &message).await?;
    }
    outbox.close();
    while let Some(message) = outbox.recv().await {
        write_message(&mut writer, &message).await?;
    }
    Ok(())
}

/// The stdout writer's outcome, with a panic or abort reported as an error.
fn finish(
    written: Result<Result<(), Box<dyn Error + Send + Sync>>, JoinError>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    written.map_err(|e| format!("stdout writer failed: {e}"))?
}

/// Serialize and write a JSON-RPC message as a single line
async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &JsonRpcMessage,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let json =
        serde_json::to_string(message).map_err(|e| format!("JSON serialization failed: {e}"))?;

    writer
        .write_all(json.as_bytes())
        .await
        .map_err(|e| format!("stdout write failed: {e}"))?;

    writer
        .write_all(b"\n")
        .await
        .map_err(|e| format!("stdout newline write failed: {e}"))?;

    writer
        .flush()
        .await
        .map_err(|e| format!("stdout flush failed: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::elicitation::{ElicitAction, ElicitRequest, ElicitationSchema};
    use crate::mcp::logging::LogLevel;
    use crate::mcp::schema::{LoggingCapability, ServerCapabilities, Tool, ToolResponse};
    use crate::mcp::tool::{McpTool, ToolRegistry};
    use futures::future::BoxFuture;
    use serde_json::{json, Value};
    use std::future::pending;
    use std::sync::{Mutex, PoisonError};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, DuplexStream, Lines};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio::time::sleep;
    use tokio::time::timeout;

    /// Reports progress, logs, then asks the person who they are.
    struct Chatty;

    #[async_trait::async_trait]
    impl McpTool<()> for Chatty {
        fn definition(&self) -> Tool {
            Tool {
                name: "chatty".to_owned(),
                description: "Talks to its client while it runs".to_owned(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(&self, _state: &Arc<()>, ctx: &ToolContext, _args: Value) -> ToolResponse {
            ctx.client.progress(1.0, Some(2.0), None);
            ctx.client
                .log(LogLevel::Info, Some("chatty"), json!("asking"));
            let request = ElicitRequest {
                message: "Who are you?".to_owned(),
                requested_schema: ElicitationSchema::default(),
            };
            match ctx.client.elicit(&request).await {
                Ok(answer) if answer.action == ElicitAction::Accept => {
                    ToolResponse::text(format!("hello {:?}", answer.content))
                }
                Ok(answer) => ToolResponse::text(format!("{:?}", answer.action)),
                Err(e) => ToolResponse::error(e.to_string()),
            }
        }
    }

    /// A tool whose behaviour is a closure over its context.
    struct Fixture {
        name: &'static str,
        run: fn(ToolContext) -> BoxFuture<'static, ToolResponse>,
    }

    #[async_trait::async_trait]
    impl McpTool<()> for Fixture {
        fn definition(&self) -> Tool {
            Tool {
                name: self.name.to_owned(),
                description: self.name.to_owned(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(&self, _state: &Arc<()>, ctx: &ToolContext, _args: Value) -> ToolResponse {
            (self.run)(ctx.clone()).await
        }
    }

    fn server() -> Arc<McpServer<()>> {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(Chatty));
        registry.register(Box::new(Fixture {
            name: "slow",
            run: |_ctx| {
                Box::pin(async {
                    sleep(Duration::from_millis(50)).await;
                    ToolResponse::text("slow done".to_owned())
                })
            },
        }));
        registry.register(Box::new(Fixture {
            name: "stuck",
            run: |_ctx| Box::pin(pending()),
        }));
        let capabilities = ServerCapabilities {
            logging: Some(LoggingCapability {}),
            ..ServerCapabilities::tools_only()
        };
        Arc::new(
            McpServer::new("stdio-test", "0.1.0", registry, Arc::new(()))
                .with_capabilities(capabilities),
        )
    }

    /// The transport's outcome.
    type Served = JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>;

    /// The client's ends of a stdio connection served on a task.
    struct Client {
        to_server: DuplexStream,
        from_server: Lines<BufReader<DuplexStream>>,
        served: Served,
    }

    impl Client {
        fn connect() -> Self {
            Self::serving(server(), DRAIN_TIMEOUT)
        }

        /// A connection to `server`, draining for `drain` at the end of input.
        fn serving(server: Arc<McpServer<()>>, drain: Duration) -> Self {
            let (to_server, server_in) = duplex(64 * 1024);
            let (server_out, from_server) = duplex(64 * 1024);
            let transport = tokio::spawn(serve_lines(
                server,
                BufReader::new(server_in),
                server_out,
                drain,
            ));
            Self {
                to_server,
                from_server: BufReader::new(from_server).lines(),
                served: transport,
            }
        }

        /// Close stdin and wait for the transport to return.
        async fn close(&mut self) {
            self.to_server.shutdown().await.expect("close stdin"); // Safe: test assertion
            timeout(Duration::from_secs(5), &mut self.served)
                .await
                .expect("the transport returns") // Safe: test assertion
                .expect("joined") // Safe: test assertion
                .expect("served without error"); // Safe: test assertion
        }

        /// The next line, or `None` once stdout is closed.
        async fn next_line(&mut self) -> Option<Value> {
            let line = self.from_server.next_line().await.expect("read")?; // Safe: test assertion
            Some(serde_json::from_str(&line).expect("json")) // Safe: test assertion
        }

        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.to_server
                .write_all(line.as_bytes())
                .await
                .expect("write"); // Safe: test assertion
        }

        async fn next(&mut self) -> Value {
            let line = self
                .from_server
                .next_line()
                .await
                .expect("read") // Safe: test assertion
                .expect("a line"); // Safe: test assertion
            serde_json::from_str(&line).expect("json") // Safe: test assertion
        }
    }

    #[tokio::test]
    async fn a_call_interleaves_notifications_and_a_request_the_client_answers() {
        let mut client = Client::connect();
        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "elicitation": {} },
                    "clientInfo": { "name": "t", "version": "1" }
                }}),
            )
            .await;
        assert_eq!(client.next().await["id"], 1);
        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 2, "method": "logging/setLevel",
                          "params": { "level": "debug" } }),
            )
            .await;
        assert_eq!(client.next().await["result"], json!({}));

        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                    "name": "chatty", "arguments": {}, "_meta": { "progressToken": "p" }
                }}),
            )
            .await;
        let progress = client.next().await;
        assert_eq!(progress["method"], "notifications/progress");
        assert_eq!(progress["params"]["progressToken"], "p");
        let log = client.next().await;
        assert_eq!(log["method"], "notifications/message");
        assert_eq!(log["params"]["logger"], "chatty");
        let ask = client.next().await;
        assert_eq!(ask["method"], "elicitation/create");
        assert_eq!(ask["params"]["message"], "Who are you?");

        client
            .send(json!({ "jsonrpc": "2.0", "id": ask["id"].clone(),
                          "result": { "action": "accept", "content": { "name": "ada" } } }))
            .await;
        let answer = client.next().await;
        assert_eq!(answer["id"], 3);
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("ada"), "{answer}");
    }

    #[tokio::test]
    async fn without_initialize_a_call_cannot_elicit_and_sends_no_logs() {
        let mut client = Client::connect();
        client
            .send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": { "name": "chatty", "arguments": {} } }))
            .await;
        let answer = client.next().await;
        assert_eq!(
            answer["id"], 1,
            "no progress token, no level: nothing before the answer"
        );
        assert_eq!(answer["result"]["isError"], true);
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("elicitation"), "{answer}");
    }

    /// The connection is one client: its own cancellation reaches its call,
    /// which withdraws the request it was waiting on and is never answered.
    #[tokio::test]
    async fn a_cancellation_on_the_connection_reaches_its_call() {
        let mut client = Client::connect();
        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "elicitation": {} },
                    "clientInfo": { "name": "t", "version": "1" }
                }}),
            )
            .await;
        client.next().await;
        client
            .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                          "params": { "name": "chatty", "arguments": {} } }))
            .await;
        let ask = client.next().await;
        assert_eq!(ask["method"], "elicitation/create");
        client
            .send(
                json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
                          "params": { "requestId": 2 } }),
            )
            .await;
        let withdrawn = client.next().await;
        assert_eq!(withdrawn["method"], "notifications/cancelled");
        assert_eq!(withdrawn["params"]["requestId"], ask["id"]);

        client
            .send(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }))
            .await;
        assert_eq!(client.next().await["id"], 3, "nothing answers the call");
    }

    #[tokio::test]
    async fn closing_stdin_fails_a_call_waiting_for_its_answer() {
        let mut client = Client::connect();
        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "elicitation": {} },
                    "clientInfo": { "name": "t", "version": "1" }
                }}),
            )
            .await;
        client.next().await;
        client
            .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                          "params": { "name": "chatty", "arguments": {} } }))
            .await;
        assert_eq!(client.next().await["method"], "elicitation/create");
        client.to_server.shutdown().await.expect("close stdin"); // Safe: test assertion
        let answer = client.next().await;
        assert_eq!(answer["id"], 2);
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("connection closed"), "{answer}");
    }

    /// Hands a clone of its context to a job that outlives the call; the job
    /// reports when the connection's cancellation reaches it, then holds the
    /// context — and the connection's sender in it — on forever.
    struct Lingers {
        noticed: Mutex<Option<oneshot::Sender<()>>>,
    }

    #[async_trait::async_trait]
    impl McpTool<()> for Lingers {
        fn definition(&self) -> Tool {
            Tool {
                name: "lingers".to_owned(),
                description: "Leaves a job holding its context".to_owned(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
                annotations: None,
                execution: None,
            }
        }

        async fn execute(&self, _state: &Arc<()>, ctx: &ToolContext, _args: Value) -> ToolResponse {
            let noticed = self
                .noticed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let ctx = ctx.clone();
            tokio::spawn(async move {
                ctx.cancellation.cancelled().await;
                ctx.client
                    .log(LogLevel::Emergency, None, json!("still here"));
                if let Some(noticed) = noticed {
                    noticed.send(()).ok();
                }
                pending::<()>().await;
            });
            ToolResponse::text("started".to_owned())
        }
    }

    /// A tool holding its context past its call does not hold the
    /// transport open: closing stdin returns, the job sees the connection's
    /// cancellation, and nothing it sends after is written.
    #[tokio::test]
    async fn a_job_holding_its_context_does_not_hang_the_exit() {
        let (noticed, notice) = oneshot::channel();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(Lingers {
            noticed: Mutex::new(Some(noticed)),
        }));
        let capabilities = ServerCapabilities {
            logging: Some(LoggingCapability {}),
            ..ServerCapabilities::tools_only()
        };
        let server = Arc::new(
            McpServer::new("stdio-test", "0.1.0", registry, Arc::new(()))
                .with_capabilities(capabilities),
        );
        let mut client = Client::serving(server, DRAIN_TIMEOUT);
        client
            .send(
                json!({ "jsonrpc": "2.0", "id": 0, "method": "logging/setLevel",
                          "params": { "level": "debug" } }),
            )
            .await;
        assert_eq!(client.next().await["id"], 0);
        client
            .send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": { "name": "lingers", "arguments": {} } }))
            .await;
        assert_eq!(client.next().await["id"], 1);

        client.close().await;
        timeout(Duration::from_secs(1), notice)
            .await
            .expect("the job sees the connection close") // Safe: test assertion
            .expect("noticed"); // Safe: test assertion
        assert_eq!(
            client.next_line().await,
            None,
            "stdout closed before the job's log"
        );
    }

    /// The requests read before stdin closed are answered before the
    /// transport returns.
    #[tokio::test]
    async fn closing_stdin_answers_the_requests_already_read() {
        let mut client = Client::connect();
        client
            .send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": { "name": "slow", "arguments": {} } }))
            .await;
        client.close().await;
        let answer = client.next_line().await.expect("the answer"); // Safe: test assertion
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["content"][0]["text"], "slow done");
        assert_eq!(client.next_line().await, None);
    }

    /// A request still running when the drain runs out is dropped unanswered.
    #[tokio::test]
    async fn the_drain_is_bounded() {
        let mut client = Client::serving(server(), Duration::from_millis(50));
        client
            .send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": { "name": "stuck", "arguments": {} } }))
            .await;
        client.close().await;
        assert_eq!(client.next_line().await, None, "never answered");
    }
}
