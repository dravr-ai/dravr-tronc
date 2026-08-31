// ABOUTME: MCP Tasks extension (io.modelcontextprotocol/tasks) — wire types, store seam, manager
// ABOUTME: Durable task handles returned in lieu of a tool result and polled via tasks/get
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The `io.modelcontextprotocol/tasks` extension (SEP-2663, protocol revision
//! `2026-07-28`).
//!
//! A server may answer a supported request — currently `tools/call` — with a
//! [`CreateTaskResult`] (`resultType: "task"`) instead of the standard result.
//! The client then polls [`method_names::TASKS_GET`], answers in-task
//! server-to-client requests via [`method_names::TASKS_UPDATE`], and requests
//! cooperative cancellation via [`method_names::TASKS_CANCEL`].
//!
//! The extension is **opt-in per request**: a client declares it under
//! `_meta["io.modelcontextprotocol/clientCapabilities"].extensions`, and a
//! server MUST NOT return a task handle to a client that did not declare it.
//! [`crate::mcp::tool::ToolContext::supports_tasks`] reports that declaration.
//!
//! Retrieval is by polling. The spec also defines a `notifications/tasks`
//! push delivered over a `subscriptions/listen` stream, but servers are never
//! required to send it; this engine is deliberately poll-only, because its
//! Streamable HTTP transport has no long-lived server-to-client stream.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::RwLock;

/// Reverse-DNS identifier for the tasks extension, used both in a client's
/// declared capabilities and in the server's `server/discover` advertisement.
pub const TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

/// Default task lifetime in milliseconds when a caller does not set one.
pub const DEFAULT_TASK_TTL_MS: u64 = 300_000;

/// Default polling interval advertised to clients, in milliseconds.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

/// JSON-RPC method names the extension defines.
///
/// The revision removed `tasks/list` and `tasks/result`; results are carried
/// inline by [`method_names::TASKS_GET`]. Do not reintroduce either name.
pub mod method_names {
    /// Poll a task's current state, including its terminal result or error.
    pub const TASKS_GET: &str = "tasks/get";
    /// Supply responses to outstanding input requests on a task.
    pub const TASKS_UPDATE: &str = "tasks/update";
    /// Request cooperative cancellation of a task.
    pub const TASKS_CANCEL: &str = "tasks/cancel";
    /// Optional server-to-client status push (not emitted by this engine).
    pub const NOTIFICATIONS_TASKS: &str = "notifications/tasks";
}

/// Opaque, server-minted task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// Wrap an existing identifier string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for TaskId {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str(&self.0)
    }
}

/// Lifecycle state of a task. Wire values are `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// The request is being processed.
    Working,
    /// The task is blocked awaiting client input.
    InputRequired,
    /// The request completed and its result is available. A tool result whose
    /// `isError` is true still completes — `Failed` is reserved for JSON-RPC
    /// errors raised during execution.
    Completed,
    /// The request failed with a JSON-RPC error.
    Failed,
    /// The request was cancelled before completion.
    Cancelled,
}

impl TaskStatus {
    /// Whether this state is terminal. No transition leaves a terminal state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Operational metadata carried by every task-bearing message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Server-minted task identifier.
    pub task_id: TaskId,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// Optional human-readable description of the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 timestamp of the most recent state change.
    pub last_updated_at: String,
    /// Lifetime from `created_at` in integer milliseconds; `None` serializes as
    /// JSON `null` and means unlimited retention. The field is REQUIRED on the
    /// wire, so it deliberately carries no `skip_serializing_if`.
    pub ttl_ms: Option<u64>,
    /// Polling interval the client should honour, in integer milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_ms: Option<u64>,
}

impl Task {
    /// Seed a new working task with the given identifier and retention policy.
    #[must_use]
    pub fn new(task_id: TaskId, ttl_ms: Option<u64>, poll_interval_ms: Option<u64>) -> Self {
        let now = current_timestamp();
        Self {
            task_id,
            status: TaskStatus::Working,
            status_message: None,
            created_at: now.clone(),
            last_updated_at: now,
            ttl_ms,
            poll_interval_ms,
        }
    }
}

/// The current ISO 8601 timestamp, millisecond precision, in UTC.
#[must_use]
pub fn current_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Status-specific payload inlined alongside the base [`Task`] fields.
///
/// Mirrors the spec's `WorkingTask` / `InputRequiredTask` / `CompletedTask` /
/// `FailedTask` / `CancelledTask` union. On the wire the payload fields sit at
/// the top level next to the base fields and `status` discriminates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPayload {
    /// `status: "working"` — no additional fields.
    Working,
    /// `status: "input_required"` — outstanding server-to-client requests,
    /// keyed by an identifier the client echoes back in `tasks/update`.
    InputRequired {
        /// Outstanding requests awaiting client responses.
        input_requests: Map<String, Value>,
    },
    /// `status: "completed"` — the original request's result shape.
    Completed {
        /// Final result, shaped like the original request's result.
        result: Map<String, Value>,
    },
    /// `status: "failed"` — the JSON-RPC error that ended the task.
    Failed {
        /// JSON-RPC error object.
        error: Map<String, Value>,
    },
    /// `status: "cancelled"` — no additional fields.
    Cancelled,
}

impl TaskPayload {
    /// The status this payload corresponds to.
    #[must_use]
    pub const fn status(&self) -> TaskStatus {
        match self {
            Self::Working => TaskStatus::Working,
            Self::InputRequired { .. } => TaskStatus::InputRequired,
            Self::Completed { .. } => TaskStatus::Completed,
            Self::Failed { .. } => TaskStatus::Failed,
            Self::Cancelled => TaskStatus::Cancelled,
        }
    }
}

/// A task with its status-specific payload inlined — the spec's `DetailedTask`,
/// returned by `tasks/get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailedTask {
    /// Base metadata. Its `status` always agrees with `payload`.
    pub task: Task,
    /// Status-specific payload.
    pub payload: TaskPayload,
}

impl DetailedTask {
    /// Pair a task with a payload, forcing `task.status` to match the payload.
    #[must_use]
    pub fn new(mut task: Task, payload: TaskPayload) -> Self {
        task.status = payload.status();
        Self { task, payload }
    }

    /// The current status.
    #[must_use]
    pub const fn status(&self) -> TaskStatus {
        self.task.status
    }
}

/// Flat wire projection: base fields plus the optional payload fields.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailedTaskWire {
    #[serde(flatten)]
    task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_requests: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Map<String, Value>>,
}

impl From<&DetailedTask> for DetailedTaskWire {
    fn from(value: &DetailedTask) -> Self {
        let (input_requests, result, error) = match &value.payload {
            TaskPayload::Working | TaskPayload::Cancelled => (None, None, None),
            TaskPayload::InputRequired { input_requests } => {
                (Some(input_requests.clone()), None, None)
            }
            TaskPayload::Completed { result } => (None, Some(result.clone()), None),
            TaskPayload::Failed { error } => (None, None, Some(error.clone())),
        };
        Self {
            task: value.task.clone(),
            input_requests,
            result,
            error,
        }
    }
}

impl Serialize for DetailedTask {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DetailedTaskWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DetailedTask {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = DetailedTaskWire::deserialize(deserializer)?;
        let payload = match wire.task.status {
            TaskStatus::Working => TaskPayload::Working,
            TaskStatus::Cancelled => TaskPayload::Cancelled,
            TaskStatus::InputRequired => TaskPayload::InputRequired {
                input_requests: wire.input_requests.ok_or_else(|| {
                    DeError::custom("input_required task is missing `inputRequests`")
                })?,
            },
            TaskStatus::Completed => TaskPayload::Completed {
                result: wire
                    .result
                    .ok_or_else(|| DeError::custom("completed task is missing `result`"))?,
            },
            TaskStatus::Failed => TaskPayload::Failed {
                error: wire
                    .error
                    .ok_or_else(|| DeError::custom("failed task is missing `error`"))?,
            },
        };
        Ok(Self {
            task: wire.task,
            payload,
        })
    }
}

/// A task handle returned in lieu of a standard result. Serializes flat:
/// `resultType` sits beside the base [`Task`] fields, per `Result & Task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskResult {
    /// Always `"task"` — the discriminator distinguishing a handle from a result.
    pub result_type: &'static str,
    /// Seed state of the new task, flattened to the top level.
    #[serde(flatten)]
    pub task: Task,
}

impl CreateTaskResult {
    /// Wrap a seed task as a `resultType: "task"` handle.
    #[must_use]
    pub const fn new(task: Task) -> Self {
        Self {
            result_type: "task",
            task,
        }
    }
}

/// Response to `tasks/get` — a [`DetailedTask`] flattened beside
/// `resultType: "complete"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskResult {
    /// Always `"complete"`; `tasks/get` returns a standard result, not a handle.
    pub result_type: &'static str,
    /// The task with its status-specific payload inlined.
    #[serde(flatten)]
    pub task: DetailedTask,
}

impl GetTaskResult {
    /// Wrap a detailed task as the standard `tasks/get` result.
    #[must_use]
    pub const fn new(task: DetailedTask) -> Self {
        Self {
            result_type: "complete",
            task,
        }
    }
}

/// Empty acknowledgement returned by `tasks/update` and `tasks/cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAck {
    /// Always `"complete"`.
    pub result_type: &'static str,
}

impl Default for TaskAck {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskAck {
    /// The acknowledgement value.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            result_type: "complete",
        }
    }
}

/// Who a task belongs to.
///
/// The revision deleted `tasks/list` so a server cannot leak the existence of
/// one caller's tasks to another; this engine enforces the same boundary on
/// every lookup, so a task id guessed or leaked across tenants still reads as
/// absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TaskOwner {
    /// Authenticated caller id, if any.
    pub user_id: Option<String>,
    /// Resolved tenant id, if any.
    pub tenant_id: Option<String>,
}

/// Failure modes of a task operation.
#[derive(Debug, Clone)]
pub enum TaskError {
    /// No task with that id is visible to this owner. A task belonging to a
    /// different owner is reported as absent, never as forbidden, so the
    /// response cannot confirm that someone else's task id exists.
    NotFound(TaskId),
    /// The task exists but is in a state that forbids the operation.
    InvalidState {
        /// The task in question.
        task_id: TaskId,
        /// Its current status.
        status: TaskStatus,
    },
    /// The backing store failed.
    Store(String),
}

impl Display for TaskError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::NotFound(id) => write!(f, "task '{id}' not found"),
            Self::InvalidState { task_id, status } => write!(
                f,
                "task '{task_id}' is {status:?} and cannot accept this operation"
            ),
            Self::Store(reason) => write!(f, "task store failure: {reason}"),
        }
    }
}

impl StdError for TaskError {}

/// Persistence seam for tasks.
///
/// The engine ships [`InMemoryTaskStore`]; a host that needs tasks to survive a
/// restart implements this over its own database. Every method takes the
/// [`TaskOwner`] so isolation is enforced by the store, not by its callers.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Persist a newly created task.
    async fn create(&self, owner: &TaskOwner, task: DetailedTask) -> Result<(), TaskError>;

    /// Fetch a task visible to `owner`, or `None` when absent or expired.
    async fn get(&self, owner: &TaskOwner, id: &TaskId) -> Result<Option<DetailedTask>, TaskError>;

    /// Overwrite a task's state.
    async fn put(&self, owner: &TaskOwner, task: DetailedTask) -> Result<(), TaskError>;

    /// Drop tasks whose TTL has elapsed, returning how many were removed.
    async fn sweep_expired(&self) -> Result<usize, TaskError>;
}

/// Process-local [`TaskStore`]. Tasks vanish on restart, which is the right
/// default for a stdio server and wrong for a durable service.
#[derive(Debug, Default)]
pub struct InMemoryTaskStore {
    entries: RwLock<HashMap<TaskId, (TaskOwner, DetailedTask)>>,
}

impl InMemoryTaskStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Whether a task's TTL has elapsed relative to now.
fn is_expired(task: &Task) -> bool {
    let Some(ttl_ms) = task.ttl_ms else {
        return false;
    };
    let Ok(created) = chrono::DateTime::parse_from_rfc3339(&task.created_at) else {
        return false;
    };
    let elapsed = Utc::now().signed_duration_since(created.with_timezone(&Utc));
    elapsed.num_milliseconds() > i64::try_from(ttl_ms).unwrap_or(i64::MAX)
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create(&self, owner: &TaskOwner, task: DetailedTask) -> Result<(), TaskError> {
        let mut entries = self.entries.write().await;
        entries.insert(task.task.task_id.clone(), (owner.clone(), task));
        Ok(())
    }

    async fn get(&self, owner: &TaskOwner, id: &TaskId) -> Result<Option<DetailedTask>, TaskError> {
        let entries = self.entries.read().await;
        Ok(entries
            .get(id)
            .filter(|(task_owner, _)| task_owner == owner)
            .filter(|(_, task)| !is_expired(&task.task))
            .map(|(_, task)| task.clone()))
    }

    async fn put(&self, owner: &TaskOwner, task: DetailedTask) -> Result<(), TaskError> {
        let mut entries = self.entries.write().await;
        entries.insert(task.task.task_id.clone(), (owner.clone(), task));
        Ok(())
    }

    async fn sweep_expired(&self) -> Result<usize, TaskError> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|_, (_, task)| !is_expired(&task.task));
        Ok(before - entries.len())
    }
}

/// Retention and pacing applied to newly created tasks.
#[derive(Debug, Clone, Copy)]
pub struct TaskOptions {
    /// Lifetime in milliseconds; `None` means unlimited retention.
    pub ttl_ms: Option<u64>,
    /// Polling interval advertised to the client, in milliseconds.
    pub poll_interval_ms: u64,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            ttl_ms: Some(DEFAULT_TASK_TTL_MS),
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        }
    }
}

/// Owns the task store and applies the lifecycle rules on top of it.
///
/// The manager does not execute work; a host spawns its own operation and
/// reports progress through [`TaskManager::complete`], [`TaskManager::fail`],
/// or [`TaskManager::request_input`]. Keeping execution out of the engine is
/// what lets a host use its own runtime, database and cancellation model.
pub struct TaskManager {
    store: Arc<dyn TaskStore>,
    options: TaskOptions,
}

impl Debug for TaskManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("TaskManager")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl TaskManager {
    /// Build a manager over the given store, with default retention.
    #[must_use]
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self {
            store,
            options: TaskOptions::default(),
        }
    }

    /// Build a manager with explicit retention and pacing.
    #[must_use]
    pub fn with_options(store: Arc<dyn TaskStore>, options: TaskOptions) -> Self {
        Self { store, options }
    }

    /// The retention and pacing this manager applies.
    #[must_use]
    pub const fn options(&self) -> TaskOptions {
        self.options
    }

    /// Borrow the backing store.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn TaskStore> {
        &self.store
    }

    /// Create a working task owned by `owner` and return its seed state.
    ///
    /// The identifier must be unguessable; callers pass one minted from a
    /// cryptographic source (the engine does not choose a generator so a host
    /// can align ids with its own database keys).
    pub async fn create(&self, owner: &TaskOwner, task_id: TaskId) -> Result<Task, TaskError> {
        let task = Task::new(
            task_id,
            self.options.ttl_ms,
            Some(self.options.poll_interval_ms),
        );
        let detailed = DetailedTask::new(task.clone(), TaskPayload::Working);
        self.store.create(owner, detailed).await?;
        Ok(task)
    }

    /// Fetch a task visible to `owner`.
    pub async fn get(&self, owner: &TaskOwner, id: &TaskId) -> Result<DetailedTask, TaskError> {
        self.store
            .get(owner, id)
            .await?
            .ok_or_else(|| TaskError::NotFound(id.clone()))
    }

    /// Move a task to a new payload, refusing any transition out of a terminal
    /// state. Returns the updated task.
    pub async fn transition(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        payload: TaskPayload,
    ) -> Result<DetailedTask, TaskError> {
        let current = self.get(owner, id).await?;
        if current.status().is_terminal() {
            return Err(TaskError::InvalidState {
                task_id: id.clone(),
                status: current.status(),
            });
        }
        let mut task = current.task;
        task.last_updated_at = current_timestamp();
        let updated = DetailedTask::new(task, payload);
        self.store.put(owner, updated.clone()).await?;
        Ok(updated)
    }

    /// Complete a task with the original request's result shape.
    ///
    /// A tool result whose `isError` is true still completes here; `failed` is
    /// reserved for JSON-RPC errors raised during execution.
    pub async fn complete(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        result: Map<String, Value>,
    ) -> Result<DetailedTask, TaskError> {
        self.transition(owner, id, TaskPayload::Completed { result })
            .await
    }

    /// Fail a task with a JSON-RPC error object.
    pub async fn fail(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        error: Map<String, Value>,
    ) -> Result<DetailedTask, TaskError> {
        self.transition(owner, id, TaskPayload::Failed { error })
            .await
    }

    /// Block a task on outstanding client input.
    pub async fn request_input(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        input_requests: Map<String, Value>,
    ) -> Result<DetailedTask, TaskError> {
        self.transition(owner, id, TaskPayload::InputRequired { input_requests })
            .await
    }

    /// Apply client input responses, returning the task to `working`.
    ///
    /// Rejects a task that is not currently awaiting input, so a stray
    /// `tasks/update` cannot resurrect a settled task.
    pub async fn apply_input(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
    ) -> Result<DetailedTask, TaskError> {
        let current = self.get(owner, id).await?;
        if current.status() != TaskStatus::InputRequired {
            return Err(TaskError::InvalidState {
                task_id: id.clone(),
                status: current.status(),
            });
        }
        self.transition(owner, id, TaskPayload::Working).await
    }

    /// Cancel a task. Cancellation is cooperative and eventually consistent:
    /// this records the request, and the host's own operation observes it.
    pub async fn cancel(&self, owner: &TaskOwner, id: &TaskId) -> Result<DetailedTask, TaskError> {
        self.transition(owner, id, TaskPayload::Cancelled).await
    }

    /// Drop tasks whose TTL elapsed.
    pub async fn sweep_expired(&self) -> Result<usize, TaskError> {
        self.store.sweep_expired().await
    }
}
