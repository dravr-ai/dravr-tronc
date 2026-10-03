// ABOUTME: Conformance tests for the io.modelcontextprotocol/tasks extension
// ABOUTME: Pins the flat wire shapes, the per-request opt-in gate, and owner isolation
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use dravr_tronc::mcp::tasks::CreateTaskResult;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::host::{CallToolOutcome, ToolDispatcher};
use dravr_tronc::mcp::schema::{TaskSupport, Tool, ToolExecution, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tasks::{
    DetailedTask, InMemoryTaskStore, Task, TaskError, TaskId, TaskManager, TaskOwner, TaskPayload,
    TaskRun, TaskStatus,
};
use dravr_tronc::mcp::tool::{ToolContext, ToolRegistry};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;
use tokio::time::timeout;

/// State for the test server.
struct TestState;

/// A dispatcher that always answers asynchronously, so the engine's task-handle
/// path is exercised end to end.
struct TaskingDispatcher {
    manager: Arc<TaskManager>,
    /// Runs kept alive the way a host's spawned operation would hold them.
    runs: Arc<Mutex<Vec<TaskRun>>>,
}

#[async_trait]
impl ToolDispatcher<TestState> for TaskingDispatcher {
    async fn list_tools(&self, _state: &Arc<TestState>, _ctx: &ToolContext) -> Vec<Tool> {
        vec![Tool {
            name: "slow".to_owned(),
            description: "Takes a while".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }]
    }

    async fn call_tool(
        &self,
        _name: &str,
        _state: &Arc<TestState>,
        ctx: &ToolContext,
        _arguments: Value,
    ) -> CallToolOutcome {
        let owner = TaskOwner {
            user_id: ctx.user_id.clone(),
            tenant_id: ctx.tenant_id.clone(),
        };
        match self.manager.create(&owner).await {
            Ok(run) => {
                let task = run.task().clone();
                self.runs.lock().await.push(run);
                CallToolOutcome::Task(Box::new(task))
            }
            Err(e) => CallToolOutcome::Immediate(Box::new(ToolResponse::error(e.to_string()))),
        }
    }
}

fn manager() -> Arc<TaskManager> {
    Arc::new(TaskManager::new(Arc::new(InMemoryTaskStore::new())))
}

fn server_with_tasks(manager: Arc<TaskManager>) -> McpServer<TestState> {
    server_with_runs(manager, Arc::new(Mutex::new(Vec::new())))
}

/// A tasking server whose dispatcher parks each run in `runs`, so a test can
/// drive the operation side.
fn server_with_runs(
    manager: Arc<TaskManager>,
    runs: Arc<Mutex<Vec<TaskRun>>>,
) -> McpServer<TestState> {
    McpServer::new(
        "test-server",
        "0.1.0",
        ToolRegistry::new(),
        Arc::new(TestState),
    )
    .with_tool_dispatcher(Arc::new(TaskingDispatcher {
        manager: Arc::clone(&manager),
        runs,
    }))
    .with_task_manager(manager)
}

/// A modern `_meta` block, optionally declaring the tasks extension.
fn modern_meta(declare_tasks: bool) -> Value {
    let extensions = if declare_tasks {
        json!({ "io.modelcontextprotocol/tasks": {} })
    } else {
        json!({})
    };
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": { "extensions": extensions }
    })
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

#[test]
fn create_task_result_is_flat_not_nested() {
    // The spec defines CreateTaskResult as `Result & Task` — flat. A nested
    // `{"task": {...}}` would be schema-invalid and is the shape an
    // earlier-draft SDK would have produced.
    let task = Task::new(TaskId::new("abc"), Some(1_000), Some(500));
    let value = serde_json::to_value(CreateTaskResult::new(task)).expect("serializes");

    assert_eq!(value["resultType"], "task");
    assert_eq!(value["taskId"], "abc");
    assert_eq!(value["status"], "working");
    assert!(
        value.get("task").is_none(),
        "task fields must be flat, not nested under `task`: {value}"
    );
}

#[test]
fn ttl_ms_is_always_present_and_null_means_unlimited() {
    // `ttlMs` is REQUIRED but nullable. Omitting it entirely is invalid, so it
    // must serialize as an explicit null rather than being skipped.
    let task = Task::new(TaskId::new("abc"), None, None);
    let value = serde_json::to_value(&task).expect("serializes");

    assert!(
        value.as_object().is_some_and(|o| o.contains_key("ttlMs")),
        "ttlMs must always be present: {value}"
    );
    assert_eq!(value["ttlMs"], Value::Null);
    // pollIntervalMs is genuinely optional and should be skipped when unset.
    assert!(value.get("pollIntervalMs").is_none());
}

#[test]
fn task_status_uses_snake_case_wire_values() {
    assert_eq!(
        serde_json::to_value(TaskStatus::InputRequired).expect("serializes"),
        json!("input_required")
    );
    assert!(TaskStatus::Completed.is_terminal());
    assert!(TaskStatus::Failed.is_terminal());
    assert!(TaskStatus::Cancelled.is_terminal());
    assert!(!TaskStatus::Working.is_terminal());
    assert!(!TaskStatus::InputRequired.is_terminal());
}

#[test]
fn detailed_task_inlines_status_specific_payload() {
    let mut result = Map::new();
    result.insert("content".to_owned(), json!([]));

    let completed = DetailedTask::new(
        Task::new(TaskId::new("t"), None, None),
        TaskPayload::Completed {
            result: result.clone(),
        },
    );
    let value = serde_json::to_value(&completed).expect("serializes");
    assert_eq!(value["status"], "completed");
    assert_eq!(value["result"]["content"], json!([]));
    assert!(value.get("error").is_none());

    let mut error = Map::new();
    error.insert("code".to_owned(), json!(-32_603));
    let failed = DetailedTask::new(
        Task::new(TaskId::new("t"), None, None),
        TaskPayload::Failed { error },
    );
    let value = serde_json::to_value(&failed).expect("serializes");
    assert_eq!(value["status"], "failed");
    assert_eq!(value["error"]["code"], -32_603);
    assert!(value.get("result").is_none());

    let input_required = DetailedTask::new(
        Task::new(TaskId::new("t"), None, None),
        TaskPayload::InputRequired {
            input_requests: result,
        },
    );
    let value = serde_json::to_value(&input_required).expect("serializes");
    assert_eq!(value["status"], "input_required");
    assert!(value.get("inputRequests").is_some());
}

// ---------------------------------------------------------------------------
// The opt-in gate — make the guard fire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tasks_get_without_declared_extension_is_refused_with_32021() {
    let server = server_with_tasks(manager());
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/get",
        "params": { "taskId": "task-1", "_meta": modern_meta(false) }
    })
    .to_string();

    let response = server.handle_raw(&raw).await.expect("response");
    let error = response.error.expect("must refuse a non-declaring client");
    assert_eq!(
        error.code, -32_021,
        "must be MissingRequiredClientCapability, not an implementation-defined code"
    );
    assert_eq!(
        error.data.expect("names the missing capability")["requiredCapabilities"][0],
        "io.modelcontextprotocol/tasks"
    );
}

#[tokio::test]
async fn tools_call_returns_a_task_handle_only_when_declared() {
    let server = server_with_tasks(manager());
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "slow", "arguments": {}, "_meta": modern_meta(true) }
    })
    .to_string();

    let response = server.handle_raw(&raw).await.expect("response");
    let result = response.result.expect("success");
    assert_eq!(result["resultType"], "task");
    let task_id = result["taskId"].as_str().expect("a string task id");
    assert_eq!(task_id.len(), 32, "128 bits of hex: {task_id}");
    assert!(task_id.bytes().all(|b| b.is_ascii_hexdigit()));
}

/// Anonymous callers all share the default owner, so the id is the only thing
/// between one caller's task and another's: two tasks never share one, and an
/// id is never a predictable sequence.
#[tokio::test]
async fn task_ids_are_minted_unguessable_by_the_engine() {
    let manager = manager();
    let owner = TaskOwner::default();
    let first = manager.create(&owner).await.expect("created").id().clone();
    let second = manager.create(&owner).await.expect("created").id().clone();
    assert_ne!(first, second);
    assert_eq!(first.as_str().len(), 32);
    assert_ne!(first.as_str(), "0".repeat(32));
}

#[tokio::test]
async fn tools_call_refuses_a_task_handle_when_extension_undeclared() {
    // The dispatcher always mints a task; the engine must refuse to frame one
    // for a client that never declared the extension rather than emit a shape
    // the client has no contract for.
    let server = server_with_tasks(manager());
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "slow", "arguments": {}, "_meta": modern_meta(false) }
    })
    .to_string();

    let response = server.handle_raw(&raw).await.expect("response");
    assert!(
        response.error.is_some(),
        "a task handle must not reach a non-declaring client"
    );
}

#[tokio::test]
async fn legacy_request_never_receives_a_task_handle() {
    // A legacy (initialize-era) client has no `_meta`, so it cannot declare the
    // extension and its response would not even carry `resultType`.
    let server = server_with_tasks(manager());
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "slow", "arguments": {} }
    })
    .to_string();

    let response = server.handle_raw(&raw).await.expect("response");
    assert!(
        response.error.is_some(),
        "legacy era must not be answered with a task handle"
    );
}

// ---------------------------------------------------------------------------
// Advertisement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_advertises_the_extension_only_when_a_manager_is_installed() {
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": { "_meta": modern_meta(true) }
    })
    .to_string();

    let with_tasks = server_with_tasks(manager());
    let response = with_tasks.handle_raw(&raw).await.expect("response");
    let result = response.result.expect("success");
    assert!(
        result["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"].is_object(),
        "must advertise the extension: {result}"
    );

    let without_tasks: McpServer<TestState> = McpServer::new(
        "test-server",
        "0.1.0",
        ToolRegistry::new(),
        Arc::new(TestState),
    );
    let response = without_tasks.handle_raw(&raw).await.expect("response");
    let result = response.result.expect("success");
    assert!(
        result["capabilities"].get("extensions").is_none(),
        "must not advertise a capability it cannot serve: {result}"
    );
}

// ---------------------------------------------------------------------------
// Lifecycle and isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_task_is_invisible_to_a_different_owner() {
    let manager = manager();
    let alice = TaskOwner {
        user_id: Some("alice".to_owned()),
        tenant_id: Some("t1".to_owned()),
    };
    let mallory = TaskOwner {
        user_id: Some("mallory".to_owned()),
        tenant_id: Some("t2".to_owned()),
    };

    let secret = manager.create(&alice).await.expect("created").id().clone();

    assert!(
        manager.get(&alice, &secret).await.is_ok(),
        "the owner can read their own task"
    );
    assert!(
        manager.get(&mallory, &secret).await.is_err(),
        "a guessed task id must not resolve for another owner"
    );
}

#[tokio::test]
async fn a_terminal_task_refuses_further_transitions() {
    let manager = manager();
    let owner = TaskOwner::default();
    let id = manager.create(&owner).await.expect("created").id().clone();
    manager
        .complete(&owner, &id, Map::new())
        .await
        .expect("completes");

    assert!(
        manager.cancel(&owner, &id).await.is_err(),
        "no transition may leave a terminal state"
    );
    assert!(manager.complete(&owner, &id, Map::new()).await.is_err());
}

#[tokio::test]
async fn tasks_update_requires_the_task_to_be_awaiting_input() {
    let manager = manager();
    let owner = TaskOwner::default();
    let id = manager.create(&owner).await.expect("created").id().clone();

    assert!(
        manager.apply_input(&owner, &id).await.is_err(),
        "a working task has no outstanding input to answer"
    );

    manager
        .request_input(&owner, &id, Map::new())
        .await
        .expect("blocks on input");
    assert_eq!(
        manager
            .apply_input(&owner, &id)
            .await
            .expect("accepts input")
            .status(),
        TaskStatus::Working,
        "answering input returns the task to working"
    );
}

#[tokio::test]
async fn tasks_get_returns_the_result_inline_once_complete() {
    let manager = manager();
    let server = server_with_tasks(Arc::clone(&manager));

    // Create through the tool-call path so the owner matches the anonymous
    // context the engine resolves without an auth hook.
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "slow", "arguments": {}, "_meta": modern_meta(true) }
    })
    .to_string();
    let created = server.handle_raw(&call).await.expect("created");
    let task_id = created.result.expect("a task handle")["taskId"]
        .as_str()
        .expect("a string task id")
        .to_owned();

    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        json!([{"type": "text", "text": "done"}]),
    );
    manager
        .complete(&TaskOwner::default(), &TaskId::new(task_id.clone()), result)
        .await
        .expect("completes");

    let get = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/get",
        "params": { "taskId": task_id, "_meta": modern_meta(true) }
    })
    .to_string();
    let response = server.handle_raw(&get).await.expect("response");
    let value = response.result.expect("success");

    assert_eq!(value["resultType"], "complete");
    assert_eq!(value["status"], "completed");
    assert_eq!(
        value["result"]["content"][0]["text"], "done",
        "the result must come back inline — there is no tasks/result method"
    );
}

#[tokio::test]
async fn tasks_cancel_moves_the_task_to_cancelled() {
    let manager = manager();
    let server = server_with_tasks(Arc::clone(&manager));
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "slow", "arguments": {}, "_meta": modern_meta(true) }
    })
    .to_string();
    let created = server.handle_raw(&call).await.expect("created");
    let task_id = created.result.expect("a task handle")["taskId"]
        .as_str()
        .expect("a string task id")
        .to_owned();

    let cancel = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/cancel",
        "params": { "taskId": task_id, "_meta": modern_meta(true) }
    })
    .to_string();
    let response = server.handle_raw(&cancel).await.expect("response");
    assert_eq!(response.result.expect("success")["resultType"], "complete");

    assert_eq!(
        manager
            .get(&TaskOwner::default(), &TaskId::new(task_id))
            .await
            .expect("still readable")
            .status(),
        TaskStatus::Cancelled
    );
}

/// `tasks/cancel` must reach the running operation, not only the stored state:
/// the run's token fires, and the operation's late result cannot resurrect the
/// task.
#[tokio::test]
async fn tasks_cancel_signals_the_operation_and_the_cancellation_is_final() {
    let manager = manager();
    let runs = Arc::new(Mutex::new(Vec::new()));
    let server = server_with_runs(Arc::clone(&manager), Arc::clone(&runs));
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "slow", "arguments": {}, "_meta": modern_meta(true) }
    })
    .to_string();
    server.handle_raw(&call).await.expect("created");
    let run = runs
        .lock()
        .await
        .pop()
        .expect("the dispatcher parked its run");
    let token = run.cancellation();
    assert!(!token.is_cancelled());

    let cancel = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/cancel",
        "params": { "taskId": run.id().as_str(), "_meta": modern_meta(true) }
    })
    .to_string();
    let response = server.handle_raw(&cancel).await.expect("response");
    assert!(
        response.result.is_some(),
        "cancel acknowledged: {response:?}"
    );

    // The operation observes the signal without polling the store.
    timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("the run's token fires on tasks/cancel");

    let id = run.id().clone();
    let late = run.complete(Map::new()).await;
    assert!(
        matches!(
            late,
            Err(TaskError::InvalidState {
                status: TaskStatus::Cancelled,
                ..
            })
        ),
        "a late result must not overwrite the cancellation: {late:?}"
    );
    assert_eq!(
        manager
            .get(&TaskOwner::default(), &id)
            .await
            .expect("readable")
            .status(),
        TaskStatus::Cancelled
    );
}

/// The terminal check and the write are one atomic store step: racing a
/// completion against a cancellation settles exactly one of them, and the
/// stored state is the winner's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_writers_settle_a_task_exactly_once() {
    let manager = manager();
    let owner = TaskOwner::default();
    for _ in 0..64 {
        let id = manager.create(&owner).await.expect("created").id().clone();
        let completing = {
            let manager = Arc::clone(&manager);
            let owner = owner.clone();
            let id = id.clone();
            tokio::spawn(async move { manager.complete(&owner, &id, Map::new()).await })
        };
        let cancelling = {
            let manager = Arc::clone(&manager);
            let owner = owner.clone();
            let id = id.clone();
            tokio::spawn(async move { manager.cancel(&owner, &id).await })
        };
        let completed = completing.await.expect("joined");
        let cancelled = cancelling.await.expect("joined");
        assert!(
            completed.is_ok() != cancelled.is_ok(),
            "exactly one writer settles the task"
        );
        let winner = if completed.is_ok() {
            TaskStatus::Completed
        } else {
            TaskStatus::Cancelled
        };
        assert_eq!(
            manager.get(&owner, &id).await.expect("readable").status(),
            winner
        );
    }
}

#[tokio::test]
async fn unknown_task_id_is_reported_as_absent() {
    let server = server_with_tasks(manager());
    let raw = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tasks/get",
        "params": { "taskId": "never-existed", "_meta": modern_meta(true) }
    })
    .to_string();

    let response = server.handle_raw(&raw).await.expect("response");
    assert!(response.error.is_some());
}

/// SEP-2663: a tool's task-support declaration serializes under `execution`
/// with camelCase levels, and its absence round-trips as absence — the safe
/// reading for every pre-Tasks client.
#[test]
fn tool_execution_task_support_round_trips() {
    let tool = Tool {
        name: "long_tool".to_owned(),
        description: "may answer with a handle".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        annotations: None,
        execution: Some(ToolExecution {
            task_support: TaskSupport::Optional,
        }),
    };
    let wire = serde_json::to_value(&tool).expect("serialize");
    assert_eq!(wire["execution"]["taskSupport"], "optional");

    let back: Tool = serde_json::from_value(wire).expect("deserialize");
    assert_eq!(
        back.execution.expect("execution present").task_support,
        TaskSupport::Optional
    );

    // Absent stays absent on the wire and in the model.
    let bare = Tool {
        name: "sync_tool".to_owned(),
        description: "always inline".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        annotations: None,
        execution: None,
    };
    let wire = serde_json::to_value(&bare).expect("serialize");
    assert!(wire.get("execution").is_none());
    let back: Tool = serde_json::from_value(wire).expect("deserialize");
    assert!(back.execution.is_none());
}

/// The engine is poll-only: `subscriptions/listen` is not a method it serves,
/// so a declaring client learns that from the standard method-not-found error
/// and polls `tasks/get` instead.
#[tokio::test]
async fn listen_is_not_served_by_a_poll_only_engine() {
    let manager = Arc::new(TaskManager::new(Arc::new(InMemoryTaskStore::new())));
    let server = server_with_tasks(manager);
    let raw = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "subscriptions/listen",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": { "io.modelcontextprotocol/tasks": {} }
                }
            }
        }
    })
    .to_string();
    let response = server.handle_raw(&raw).await.expect("response");
    let error = response.error.expect("error");
    assert_eq!(error.code, -32601);
    assert_eq!(error.message, "Method not found: subscriptions/listen");
}
