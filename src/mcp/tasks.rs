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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::INTERNAL_ERROR;
use crate::mcp::random_id::random_hex_id;
use tokio::sync::{mpsc, RwLock};
use tokio::task::AbortHandle;
use tokio::time::{interval, MissedTickBehavior};
use tracing::warn;

/// The token a [`TaskRun`] carries, re-exported so a host names it without a
/// direct `tokio-util` dependency.
pub use tokio_util::sync::CancellationToken;

/// Reverse-DNS identifier for the tasks extension, used both in a client's
/// declared capabilities and in the server's `server/discover` advertisement.
pub const TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

/// Default task lifetime in milliseconds when a caller does not set one.
pub const DEFAULT_TASK_TTL_MS: u64 = 300_000;

/// Default polling interval advertised to clients, in milliseconds.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

/// Default period of a manager's expired-task sweep.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

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
}

/// Opaque, server-minted task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// Wrap an existing identifier string — one read off the wire or out of a
    /// store. New tasks get theirs from [`Self::generate`].
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Mint a fresh identifier from the operating system's CSPRNG.
    ///
    /// A task id is a bearer capability: every caller the auth hook cannot
    /// tell apart — all anonymous callers of an unauthenticated server — maps
    /// to the same [`TaskOwner`], so for them the id is the only thing between
    /// one caller's task and another's `tasks/get` or `tasks/cancel`. That is
    /// why the engine mints it rather than taking one from the host.
    pub fn generate() -> Result<Self, TaskError> {
        random_hex_id()
            .map(Self)
            .map_err(|e| TaskError::Store(format!("no OS randomness for a task id: {e}")))
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
///
/// Callers the auth hook resolves to no identity all share the default owner,
/// so between them the boundary is the id alone — which is why ids come from
/// [`TaskId::generate`] and never from the host.
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
    /// The client's input responses do not answer the task's outstanding
    /// input requests: a key is missing, or names no outstanding request.
    InvalidInput {
        /// The task in question.
        task_id: TaskId,
        /// What is wrong with the responses.
        reason: String,
    },
    /// The task is visible but no operation in this process is running it,
    /// so there is nothing to hand client input to. Seen when the operation
    /// ended without settling the task, or ran on another instance.
    // LIMITATION(registre#761): Detached — task input and cancel signals reach only an operation in this process
    Detached(TaskId),
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
            Self::InvalidInput { task_id, reason } => {
                write!(f, "input for task '{task_id}' rejected: {reason}")
            }
            Self::Detached(id) => {
                write!(f, "task '{id}' has no running operation to receive input")
            }
            Self::Store(reason) => write!(f, "task store failure: {reason}"),
        }
    }
}

impl StdError for TaskError {}

/// The state change a [`TaskStore::update`] applies to the stored task.
///
/// Called with the current state while the store holds whatever lock or
/// transaction makes the read-then-write atomic; returning an error leaves the
/// task untouched.
pub type TaskUpdate<'a> =
    &'a (dyn Fn(&DetailedTask) -> Result<DetailedTask, TaskError> + Send + Sync);

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

    /// Atomically read a task visible to `owner`, pass it to `apply`, and store
    /// what `apply` returns.
    ///
    /// The read and the write MUST be one atomic step (a lock, a transaction,
    /// a compare-and-set): the manager's rule that no transition leaves a
    /// terminal state is decided inside `apply`, and two writers that both read
    /// `working` before either wrote would otherwise let a late `complete`
    /// overwrite a `cancelled`. Returns [`TaskError::NotFound`] when the task
    /// is absent, expired, or owned by someone else.
    async fn update(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        apply: TaskUpdate<'_>,
    ) -> Result<DetailedTask, TaskError>;

    /// Drop tasks whose TTL has elapsed, returning the ids removed.
    async fn sweep_expired(&self) -> Result<Vec<TaskId>, TaskError>;
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

    async fn update(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        apply: TaskUpdate<'_>,
    ) -> Result<DetailedTask, TaskError> {
        let mut entries = self.entries.write().await;
        let (_, stored) = entries
            .get_mut(id)
            .filter(|(task_owner, task)| task_owner == owner && !is_expired(&task.task))
            .ok_or_else(|| TaskError::NotFound(id.clone()))?;
        let updated = apply(stored)?;
        stored.clone_from(&updated);
        Ok(updated)
    }

    async fn sweep_expired(&self) -> Result<Vec<TaskId>, TaskError> {
        let mut entries = self.entries.write().await;
        let mut removed = Vec::new();
        entries.retain(|id, (_, task)| {
            let expired = is_expired(&task.task);
            if expired {
                removed.push(id.clone());
            }
            !expired
        });
        Ok(removed)
    }
}

/// Retention and pacing applied to newly created tasks, and who sweeps the
/// expired ones.
///
/// [`Self::default`] has the manager sweep on its own, every
/// [`DEFAULT_SWEEP_INTERVAL`]. A host that already runs a sweeper of its own
/// — a scheduled job calling [`TaskManager::sweep_expired`], a database
/// sweep over its [`TaskStore`] — starts from [`Self::host_swept`] instead,
/// or every instance runs two sweeps over the same store.
///
/// Built with [`Self::default`] or [`Self::host_swept`] and the `with_`
/// methods: it is `#[non_exhaustive]`, so a setting added later is not a
/// breaking change. The fields stay public to read.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TaskOptions {
    /// Lifetime in milliseconds; `None` means unlimited retention.
    pub ttl_ms: Option<u64>,
    /// Polling interval advertised to the client, in milliseconds.
    pub poll_interval_ms: u64,
    /// How often the manager drops expired tasks on its own. `None`, or a
    /// zero duration, leaves sweeping to a host that calls
    /// [`TaskManager::sweep_expired`] on its own schedule.
    pub sweep_interval: Option<Duration>,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            ttl_ms: Some(DEFAULT_TASK_TTL_MS),
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            sweep_interval: Some(DEFAULT_SWEEP_INTERVAL),
        }
    }
}

impl TaskOptions {
    /// The default retention and pacing, with the manager's own sweep off:
    /// for a host that sweeps expired tasks itself, by calling
    /// [`TaskManager::sweep_expired`] or through its store, on its own
    /// schedule.
    #[must_use]
    pub fn host_swept() -> Self {
        Self {
            sweep_interval: None,
            ..Self::default()
        }
    }

    /// These options with a task lifetime of `ttl_ms` milliseconds, or
    /// unlimited retention for `None`.
    #[must_use]
    pub const fn with_ttl_ms(mut self, ttl_ms: Option<u64>) -> Self {
        self.ttl_ms = ttl_ms;
        self
    }

    /// These options advertising a polling interval of `poll_interval_ms`
    /// milliseconds.
    #[must_use]
    pub const fn with_poll_interval_ms(mut self, poll_interval_ms: u64) -> Self {
        self.poll_interval_ms = poll_interval_ms;
        self
    }

    /// These options with the manager sweeping every `sweep_interval`, or
    /// not at all for `None` (see [`Self::host_swept`]).
    #[must_use]
    pub const fn with_sweep_interval(mut self, sweep_interval: Option<Duration>) -> Self {
        self.sweep_interval = sweep_interval;
        self
    }
}

/// Client answers to a task's outstanding input requests, keyed like the
/// `inputRequests` they answer.
pub type InputResponses = Map<String, Value>;

/// What the manager holds for a task whose operation is running in this
/// process: the signal [`TaskManager::cancel`] fires, and where
/// [`TaskManager::apply_input`] delivers the client's answers.
#[derive(Debug)]
struct LiveRun {
    cancel: CancellationToken,
    inputs: mpsc::UnboundedSender<InputResponses>,
}

/// Check that `responses` answers exactly the outstanding `requests`: the spec
/// requires every key of `inputRequests` to appear in `inputResponses`, and a
/// key naming no request answers nothing the operation asked.
fn validate_input_keys(
    task_id: &TaskId,
    requests: &Map<String, Value>,
    responses: &InputResponses,
) -> Result<(), TaskError> {
    let invalid = |reason: String| TaskError::InvalidInput {
        task_id: task_id.clone(),
        reason,
    };
    if let Some(missing) = requests.keys().find(|key| !responses.contains_key(*key)) {
        return Err(invalid(format!(
            "no response for input request '{missing}'"
        )));
    }
    if let Some(unknown) = responses.keys().find(|key| !requests.contains_key(*key)) {
        return Err(invalid(format!(
            "'{unknown}' names no outstanding input request"
        )));
    }
    Ok(())
}

/// Owns the task store and applies the lifecycle rules on top of it.
///
/// The manager does not execute work: [`TaskManager::create`] hands the host a
/// [`TaskRun`], the host spawns its own operation around it, and the operation
/// settles the task through the run. Keeping execution out of the engine is
/// what lets a host use its own runtime and database; the run is how the
/// engine still reaches that operation — `tasks/cancel` fires its
/// [`TaskRun::cancellation`] token.
///
/// The manager also drops expired tasks on its own, every
/// [`TaskOptions::sweep_interval`], from the first [`Self::create`]. A host
/// that runs its own sweeper over the same store builds the manager with
/// [`TaskOptions::host_swept`], so each instance runs one sweep, not two.
pub struct TaskManager {
    store: Arc<dyn TaskStore>,
    options: TaskOptions,
    runs: Mutex<HashMap<TaskId, LiveRun>>,
    sweeper: OnceLock<AbortHandle>,
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
        Self::with_options(store, TaskOptions::default())
    }

    /// Build a manager with explicit retention and pacing.
    #[must_use]
    pub fn with_options(store: Arc<dyn TaskStore>, options: TaskOptions) -> Self {
        Self {
            store,
            options,
            runs: Mutex::new(HashMap::new()),
            sweeper: OnceLock::new(),
        }
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

    /// Start the periodic expired-task sweep, once per manager.
    ///
    /// Started from [`Self::create`] rather than the constructor because a
    /// spawn needs a runtime and `create` is the first call guaranteed to run
    /// on one; nothing expires before a task exists. The loop holds the
    /// manager weakly, so it ends with the manager, and the manager's drop
    /// aborts it without waiting for the next tick.
    fn ensure_sweeper(self: &Arc<Self>) {
        let Some(every) = self.options.sweep_interval.filter(|d| !d.is_zero()) else {
            return;
        };
        self.sweeper.get_or_init(|| {
            let manager = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut ticks = interval(every);
                ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
                // The first tick completes immediately; sweep a period later.
                ticks.tick().await;
                loop {
                    ticks.tick().await;
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    if let Err(e) = manager.sweep_expired().await {
                        warn!(error = %e, "Expired-task sweep failed");
                    }
                }
            })
            .abort_handle()
        });
    }

    /// The live-run table, recovered from a poisoned lock: every critical
    /// section is a single map insert, remove or lookup, so a panic elsewhere
    /// cannot leave it half-written.
    fn runs(&self) -> MutexGuard<'_, HashMap<TaskId, LiveRun>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create a working task owned by `owner` and return the run that drives
    /// it.
    ///
    /// The engine mints the id with [`TaskId::generate`]; a host that keys its
    /// own records on it reads it back from [`TaskRun::task`]. Answer the call
    /// with that seed task, then move the run into the operation that does the
    /// work: it carries the cancellation token and settles the task.
    pub async fn create(self: &Arc<Self>, owner: &TaskOwner) -> Result<TaskRun, TaskError> {
        self.ensure_sweeper();
        let task = Task::new(
            TaskId::generate()?,
            self.options.ttl_ms,
            Some(self.options.poll_interval_ms),
        );
        let detailed = DetailedTask::new(task.clone(), TaskPayload::Working);
        self.store.create(owner, detailed).await?;
        let cancel = CancellationToken::new();
        let (inputs_tx, inputs) = mpsc::unbounded_channel();
        self.runs().insert(
            task.task_id.clone(),
            LiveRun {
                cancel: cancel.clone(),
                inputs: inputs_tx,
            },
        );
        Ok(TaskRun {
            manager: Arc::clone(self),
            owner: owner.clone(),
            task,
            cancel,
            inputs,
        })
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
    ///
    /// The check and the write are one atomic store update, so a terminal
    /// state is final even against a concurrent writer.
    pub async fn transition(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        payload: TaskPayload,
    ) -> Result<DetailedTask, TaskError> {
        self.store
            .update(owner, id, &|current| {
                if current.status().is_terminal() {
                    return Err(TaskError::InvalidState {
                        task_id: id.clone(),
                        status: current.status(),
                    });
                }
                let mut task = current.task.clone();
                task.last_updated_at = current_timestamp();
                Ok(DetailedTask::new(task, payload.clone()))
            })
            .await
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

    /// Apply a client's `tasks/update`: check `responses` against the task's
    /// outstanding input requests, return the task to `working`, and hand the
    /// responses to the operation waiting in [`TaskRun::request_input`].
    ///
    /// Rejects a task that is not currently awaiting input, so a stray
    /// `tasks/update` cannot resurrect a settled task, and responses whose keys
    /// differ from the outstanding requests'. A task no operation in this
    /// process is running is refused with [`TaskError::Detached`] and left
    /// awaiting input, rather than moved to a `working` nothing will finish.
    pub async fn apply_input(
        &self,
        owner: &TaskOwner,
        id: &TaskId,
        responses: InputResponses,
    ) -> Result<DetailedTask, TaskError> {
        // Ownership first, so another owner's task reads as absent rather than
        // as detached.
        self.get(owner, id).await?;
        let inputs = self
            .runs()
            .get(id)
            .map(|run| run.inputs.clone())
            .ok_or_else(|| TaskError::Detached(id.clone()))?;

        let working = self
            .store
            .update(owner, id, &|current| {
                let TaskPayload::InputRequired { input_requests } = &current.payload else {
                    return Err(TaskError::InvalidState {
                        task_id: id.clone(),
                        status: current.status(),
                    });
                };
                validate_input_keys(id, input_requests, &responses)?;
                let mut task = current.task.clone();
                task.last_updated_at = current_timestamp();
                Ok(DetailedTask::new(task, TaskPayload::Working))
            })
            .await?;

        if inputs.send(responses).is_err() {
            // The run was dropped between the lookup and the send: the task is
            // `working` with nobody working it, so settle it as failed.
            let mut error = Map::new();
            error.insert("code".to_owned(), Value::from(INTERNAL_ERROR));
            error.insert(
                "message".to_owned(),
                Value::String("task operation ended before receiving its input".to_owned()),
            );
            self.fail(owner, id, error).await?;
            return Err(TaskError::Detached(id.clone()));
        }
        Ok(working)
    }

    /// Cancel a task: record `cancelled`, then fire the cancellation token of
    /// the operation running it in this process.
    ///
    /// The state is written first, so once the token fires a late
    /// [`TaskRun::complete`] from the operation is refused rather than
    /// overwriting the cancellation. The operation itself stops cooperatively,
    /// at its next look at the token.
    pub async fn cancel(&self, owner: &TaskOwner, id: &TaskId) -> Result<DetailedTask, TaskError> {
        let cancelled = self.transition(owner, id, TaskPayload::Cancelled).await?;
        if let Some(run) = self.runs().get(id) {
            run.cancel.cancel();
        }
        Ok(cancelled)
    }

    /// Drop tasks whose TTL elapsed, returning how many were removed.
    ///
    /// The manager runs this every [`TaskOptions::sweep_interval`]; a host
    /// calls it directly only when it turned that off, with
    /// [`TaskOptions::host_swept`]. An expired task's result can no longer
    /// be retrieved, so the operation still running it is cancelled too.
    pub async fn sweep_expired(&self) -> Result<usize, TaskError> {
        let removed = self.store.sweep_expired().await?;
        let runs = self.runs();
        for id in &removed {
            if let Some(run) = runs.get(id) {
                run.cancel.cancel();
            }
        }
        Ok(removed.len())
    }

    /// Forget a run whose [`TaskRun`] was dropped.
    fn release(&self, id: &TaskId) {
        self.runs().remove(id);
    }
}

impl Drop for TaskManager {
    fn drop(&mut self) {
        if let Some(sweeper) = self.sweeper.get() {
            sweeper.abort();
        }
    }
}

/// The host's handle on a running task, returned by [`TaskManager::create`].
///
/// Move it into the operation doing the work. The operation watches
/// [`Self::cancellation`] — fired by `tasks/cancel` and by TTL expiry — and
/// settles the task with [`Self::complete`] or [`Self::fail`]. Dropping the run
/// unregisters it; the task's stored state is untouched, so a host that
/// settles from elsewhere by id through [`TaskManager::complete`] still can.
pub struct TaskRun {
    manager: Arc<TaskManager>,
    owner: TaskOwner,
    task: Task,
    cancel: CancellationToken,
    inputs: mpsc::UnboundedReceiver<InputResponses>,
}

impl Debug for TaskRun {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("TaskRun")
            .field("task_id", &self.task.task_id)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl TaskRun {
    /// The seed state, to answer the originating request with as a
    /// [`CreateTaskResult`] handle.
    #[must_use]
    pub const fn task(&self) -> &Task {
        &self.task
    }

    /// The minted task id.
    #[must_use]
    pub const fn id(&self) -> &TaskId {
        &self.task.task_id
    }

    /// The owner the task is bound to.
    #[must_use]
    pub const fn owner(&self) -> &TaskOwner {
        &self.owner
    }

    /// The token `tasks/cancel` and TTL expiry fire. Clone it into whatever
    /// part of the operation needs to stop.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Whether the task has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Ask the client for input and wait for its answers.
    ///
    /// Moves the task to `input_required` carrying `input_requests` (keyed by
    /// identifiers the client echoes back), then waits for the `tasks/update`
    /// that answers every one of them, by which point the task is `working`
    /// again. Returns [`TaskError::InvalidState`] with status `cancelled` if
    /// the task is cancelled or expires while waiting.
    pub async fn request_input(
        &mut self,
        input_requests: Map<String, Value>,
    ) -> Result<InputResponses, TaskError> {
        self.manager
            .request_input(&self.owner, &self.task.task_id, input_requests)
            .await?;
        let cancelled = || TaskError::InvalidState {
            task_id: self.task.task_id.clone(),
            status: TaskStatus::Cancelled,
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(cancelled()),
            responses = self.inputs.recv() => {
                responses.ok_or_else(|| TaskError::Detached(self.task.task_id.clone()))
            }
        }
    }

    /// Complete the task with the original request's result shape.
    ///
    /// Refused with [`TaskError::InvalidState`] when the task is already
    /// terminal — cancelled while the operation was finishing, typically.
    pub async fn complete(self, result: Map<String, Value>) -> Result<DetailedTask, TaskError> {
        self.manager
            .complete(&self.owner, &self.task.task_id, result)
            .await
    }

    /// Fail the task with a JSON-RPC error object.
    pub async fn fail(self, error: Map<String, Value>) -> Result<DetailedTask, TaskError> {
        self.manager
            .fail(&self.owner, &self.task.task_id, error)
            .await
    }
}

impl Drop for TaskRun {
    fn drop(&mut self) {
        self.manager.release(&self.task.task_id);
    }
}
