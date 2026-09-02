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

use async_trait::async_trait;
use dravr_tronc::mcp::host::{CallToolOutcome, ToolDispatcher};
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{TaskSupport, Tool, ToolExecution, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tasks::{
    DetailedTask, InMemoryTaskStore, Task, TaskId, TaskManager, TaskOptions, TaskOwner,
    TaskPayload, TaskStatus,
};
use dravr_tronc::mcp::tool::{ToolContext, ToolRegistry};
use serde_json::{json, Map, Value};
use std::time::Duration;
use tokio::time::timeout;

/// State for the test server.
struct TestState;

/// A dispatcher that always answers asynchronously, so the engine's task-handle
/// path is exercised end to end.
struct TaskingDispatcher {
    manager: Arc<TaskManager>,
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
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        ToolResponse::text("sync fallback".to_owned())
    }

    async fn call_tool_outcome(
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
        match self.manager.create(&owner, TaskId::new("task-1")).await {
            Ok(task) => CallToolOutcome::Task(Box::new(task)),
            Err(e) => CallToolOutcome::Immediate(Box::new(ToolResponse::error(e.to_string()))),
        }
    }
}

fn manager() -> Arc<TaskManager> {
    Arc::new(TaskManager::new(Arc::new(InMemoryTaskStore::new())))
}

fn parse_request(value: Value) -> JsonRpcRequest {
    serde_json::from_value(value).expect("request json")
}

fn server_with_tasks(manager: Arc<TaskManager>) -> McpServer<TestState> {
    McpServer::new(
        "test-server",
        "0.1.0",
        ToolRegistry::new(),
        Arc::new(TestState),
    )
    .with_tool_dispatcher(Arc::new(TaskingDispatcher {
        manager: Arc::clone(&manager),
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
    assert_eq!(result["taskId"], "task-1");
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

    manager
        .create(&alice, TaskId::new("secret"))
        .await
        .expect("created");

    assert!(
        manager.get(&alice, &TaskId::new("secret")).await.is_ok(),
        "the owner can read their own task"
    );
    assert!(
        manager.get(&mallory, &TaskId::new("secret")).await.is_err(),
        "a guessed task id must not resolve for another owner"
    );
}

#[tokio::test]
async fn a_terminal_task_refuses_further_transitions() {
    let manager = manager();
    let owner = TaskOwner::default();
    let id = TaskId::new("done");
    manager.create(&owner, id.clone()).await.expect("created");
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
    let id = TaskId::new("working");
    manager.create(&owner, id.clone()).await.expect("created");

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
    server.handle_raw(&call).await.expect("created");

    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        json!([{"type": "text", "text": "done"}]),
    );
    manager
        .complete(&TaskOwner::default(), &TaskId::new("task-1"), result)
        .await
        .expect("completes");

    let get = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/get",
        "params": { "taskId": "task-1", "_meta": modern_meta(true) }
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
    server.handle_raw(&call).await.expect("created");

    let cancel = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/cancel",
        "params": { "taskId": "task-1", "_meta": modern_meta(true) }
    })
    .to_string();
    let response = server.handle_raw(&cancel).await.expect("response");
    assert_eq!(response.result.expect("success")["resultType"], "complete");

    assert_eq!(
        manager
            .get(&TaskOwner::default(), &TaskId::new("task-1"))
            .await
            .expect("still readable")
            .status(),
        TaskStatus::Cancelled
    );
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

/// A subscription pushes each observed transition for its owner — and only
/// its owner: a second owner's stream stays silent through the whole
/// lifecycle.
#[tokio::test]
async fn subscription_pushes_transitions_for_the_owner_only() {
    let store = Arc::new(InMemoryTaskStore::new());
    let manager = TaskManager::with_options(
        store,
        TaskOptions {
            ttl_ms: Some(60_000),
            poll_interval_ms: 10,
        },
    );
    let owner = TaskOwner {
        user_id: Some("user-a".to_owned()),
        tenant_id: Some("tenant-a".to_owned()),
    };
    let stranger = TaskOwner {
        user_id: Some("user-b".to_owned()),
        tenant_id: Some("tenant-b".to_owned()),
    };

    let mut mine = manager.subscribe(&owner);
    let mut theirs = manager.subscribe(&stranger);

    let task_id = TaskId::new("pushed-task");
    manager
        .create(&owner, task_id.clone())
        .await
        .expect("create");

    let frame = timeout(Duration::from_secs(2), mine.next())
        .await
        .expect("a frame within the window")
        .expect("stream open");
    assert_eq!(frame.method, "notifications/tasks");
    let params = frame.params.expect("params");
    assert_eq!(params["taskId"], "pushed-task");
    assert_eq!(params["status"], "working");

    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        json!([{"type": "text", "text": "ok"}]),
    );
    manager
        .complete(&owner, &task_id, result)
        .await
        .expect("complete");

    let frame = timeout(Duration::from_secs(2), mine.next())
        .await
        .expect("terminal frame within the window")
        .expect("stream open");
    assert_eq!(frame.params.expect("params")["status"], "completed");

    // The stranger's stream saw nothing through the whole lifecycle.
    let silent = timeout(Duration::from_millis(100), theirs.next()).await;
    assert!(silent.is_err(), "another owner's stream must stay silent");
}

/// The unary dispatch path cannot stream, and says so — never method-not-found
/// while a `TaskManager` is installed.
#[tokio::test]
async fn listen_through_the_unary_path_is_refused_with_guidance() {
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
    assert!(
        error.message.contains("streaming transport"),
        "got: {}",
        error.message
    );
}

/// `open_task_subscription` enforces the same gates as the unary tasks methods:
/// an undeclared client gets -32021, a legacy-shaped request invalid-params.
#[tokio::test]
async fn open_task_subscription_gates_on_declaration_and_era() {
    let manager = Arc::new(TaskManager::new(Arc::new(InMemoryTaskStore::new())));
    let server = server_with_tasks(manager);
    let ctx = ToolContext::new().with_user("u1").with_tenant("t1");

    let undeclared = parse_request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "subscriptions/listen",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    }));
    let err = server
        .open_task_subscription(&undeclared, &ctx)
        .expect_err("undeclared must be refused");
    assert_eq!(err.error.expect("error").code, -32021);

    let legacy = parse_request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "subscriptions/listen",
        "params": {}
    }));
    let err = server
        .open_task_subscription(&legacy, &ctx)
        .expect_err("legacy shape must be refused");
    assert_eq!(err.error.expect("error").code, -32602);

    let declared = parse_request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "subscriptions/listen",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": { "io.modelcontextprotocol/tasks": {} }
                }
            }
        }
    }));
    assert!(server.open_task_subscription(&declared, &ctx).is_ok());
}
