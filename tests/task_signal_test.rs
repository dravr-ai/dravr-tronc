// ABOUTME: Tests for TaskSignalBus, the optional seam carrying task cancels and input across instances
// ABOUTME: Two managers over one store stand in for two instances of a service behind a load balancer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::tasks::{
    InMemoryTaskStore, TaskError, TaskManager, TaskOptions, TaskOwner, TaskSignal, TaskSignalBus,
    TaskStatus, TaskStore,
};
use serde_json::{json, Map, Value};
use tokio::task::yield_now;
use tokio::time::{sleep, timeout};

/// An in-process carrier standing in for the host's (Postgres `LISTEN/NOTIFY`,
/// Redis pub/sub): `publish` hands the signal to every subscribed manager's
/// `deliver`, as the host's listener on each instance would. It also records
/// what it carried so a test can assert the signal itself, not just its effect.
/// Holds the managers weakly: each manager holds the bus, so a strong
/// reference back would leak both.
#[derive(Default)]
struct LoopbackBus {
    peers: Mutex<Vec<Weak<TaskManager>>>,
    carried: Mutex<Vec<TaskSignal>>,
}

impl LoopbackBus {
    fn subscribe(&self, manager: &Arc<TaskManager>) {
        self.peers.lock().unwrap().push(Arc::downgrade(manager));
    }

    fn carried(&self) -> Vec<TaskSignal> {
        self.carried.lock().unwrap().clone()
    }
}

#[async_trait]
impl TaskSignalBus for LoopbackBus {
    async fn publish(&self, signal: TaskSignal) -> Result<(), TaskError> {
        self.carried.lock().unwrap().push(signal.clone());
        let peers: Vec<Arc<TaskManager>> = self
            .peers
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for peer in peers {
            peer.deliver(signal.clone());
        }
        Ok(())
    }
}

/// A bus whose carrier is down.
struct BrokenBus;

#[async_trait]
impl TaskSignalBus for BrokenBus {
    async fn publish(&self, _signal: TaskSignal) -> Result<(), TaskError> {
        Err(TaskError::Store("carrier unreachable".to_owned()))
    }
}

fn owner() -> TaskOwner {
    TaskOwner {
        user_id: Some("user-1".to_owned()),
        tenant_id: Some("tenant-1".to_owned()),
    }
}

/// Two instances of one service: separate managers, one shared store, one bus.
fn two_instances(
    options: TaskOptions,
) -> (
    Arc<TaskManager>,
    Arc<TaskManager>,
    Arc<LoopbackBus>,
    Arc<dyn TaskStore>,
) {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let bus = Arc::new(LoopbackBus::default());
    let a = Arc::new(
        TaskManager::with_options(Arc::clone(&store), options).with_signal_bus(bus.clone()),
    );
    let b = Arc::new(
        TaskManager::with_options(Arc::clone(&store), options).with_signal_bus(bus.clone()),
    );
    bus.subscribe(&a);
    bus.subscribe(&b);
    (a, b, bus, store)
}

#[tokio::test]
async fn a_cancel_taken_by_one_instance_fires_the_run_on_another() {
    let (a, b, bus, _) = two_instances(TaskOptions::host_swept());
    let run = a.create(&owner()).await.unwrap();
    let token = run.cancellation();

    let cancelled = b.cancel(&owner(), run.id()).await.unwrap();

    assert_eq!(cancelled.status(), TaskStatus::Cancelled);
    assert!(
        token.is_cancelled(),
        "the run on instance A must see B's cancel"
    );
    assert_eq!(
        bus.carried(),
        vec![TaskSignal::Cancelled {
            task_id: run.id().clone()
        }]
    );
}

#[tokio::test]
async fn a_cancel_for_a_local_run_is_not_published() {
    let (a, _b, bus, _) = two_instances(TaskOptions::host_swept());
    let run = a.create(&owner()).await.unwrap();

    a.cancel(&owner(), run.id()).await.unwrap();

    assert!(run.is_cancelled());
    assert!(bus.carried().is_empty(), "a local run needs no bus");
}

#[tokio::test]
async fn input_taken_by_one_instance_reaches_the_operation_on_another() {
    let (a, b, bus, _) = two_instances(TaskOptions::host_swept());
    let mut run = a.create(&owner()).await.unwrap();
    let id = run.id().clone();

    let mut requests = Map::new();
    requests.insert(
        "confirm".to_owned(),
        json!({ "method": "elicitation/create" }),
    );
    let operation = tokio::spawn(async move {
        let answers = run.request_input(requests).await;
        (run, answers)
    });
    // Wait until the operation has recorded its input request.
    timeout(Duration::from_secs(5), async {
        while b.get(&owner(), &id).await.unwrap().status() != TaskStatus::InputRequired {
            yield_now().await;
        }
    })
    .await
    .expect("the operation asks for input");

    let mut responses = Map::new();
    responses.insert("confirm".to_owned(), json!({ "action": "accept" }));
    let working = b
        .apply_input(&owner(), &id, responses.clone())
        .await
        .unwrap();
    assert_eq!(working.status(), TaskStatus::Working);

    let (run, answers) = timeout(Duration::from_secs(5), operation)
        .await
        .expect("the operation receives B's input")
        .unwrap();
    assert_eq!(answers.unwrap(), responses);
    assert_eq!(
        bus.carried(),
        vec![TaskSignal::Input {
            task_id: id.clone(),
            responses
        }]
    );

    let mut result = Map::new();
    result.insert("content".to_owned(), Value::Array(Vec::new()));
    run.complete(result).await.unwrap();
    assert_eq!(
        b.get(&owner(), &id).await.unwrap().status(),
        TaskStatus::Completed
    );
}

#[tokio::test]
async fn without_a_bus_input_for_another_instance_is_detached_and_left_waiting() {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let a = Arc::new(TaskManager::new(Arc::clone(&store)));
    let b = TaskManager::new(Arc::clone(&store));
    let run = a.create(&owner()).await.unwrap();
    let mut requests = Map::new();
    requests.insert("confirm".to_owned(), json!({}));
    a.request_input(&owner(), run.id(), requests).await.unwrap();

    let mut responses = Map::new();
    responses.insert("confirm".to_owned(), json!({ "action": "accept" }));
    let refused = b.apply_input(&owner(), run.id(), responses).await;

    assert!(matches!(refused, Err(TaskError::Detached(ref id)) if id == run.id()));
    assert_eq!(
        b.get(&owner(), run.id()).await.unwrap().status(),
        TaskStatus::InputRequired
    );
}

#[tokio::test]
async fn a_sweep_on_one_instance_cancels_an_expired_run_on_another() {
    let (a, b, bus, _) = two_instances(TaskOptions::host_swept().with_ttl_ms(Some(1)));
    let run = a.create(&owner()).await.unwrap();
    sleep(Duration::from_millis(20)).await;

    let removed = b.sweep_expired().await.unwrap();

    assert_eq!(removed, 1);
    assert!(run.is_cancelled(), "the expired run on A must be cancelled");
    assert_eq!(
        bus.carried(),
        vec![TaskSignal::Cancelled {
            task_id: run.id().clone()
        }]
    );
}

#[tokio::test]
async fn a_failing_bus_still_records_the_cancel() {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let a = Arc::new(TaskManager::new(Arc::clone(&store)));
    let b = TaskManager::new(Arc::clone(&store)).with_signal_bus(Arc::new(BrokenBus));
    let run = a.create(&owner()).await.unwrap();

    let cancelled = b.cancel(&owner(), run.id()).await.unwrap();

    assert_eq!(cancelled.status(), TaskStatus::Cancelled);
    assert!(!run.is_cancelled(), "the broken carrier never reached A");
}

#[tokio::test]
async fn a_failing_bus_fails_the_input_it_could_not_carry() {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let a = Arc::new(TaskManager::new(Arc::clone(&store)));
    let b = TaskManager::new(Arc::clone(&store)).with_signal_bus(Arc::new(BrokenBus));
    let run = a.create(&owner()).await.unwrap();
    let mut requests = Map::new();
    requests.insert("confirm".to_owned(), json!({}));
    a.request_input(&owner(), run.id(), requests).await.unwrap();

    let mut responses = Map::new();
    responses.insert("confirm".to_owned(), json!({ "action": "accept" }));
    let refused = b.apply_input(&owner(), run.id(), responses).await;

    assert!(matches!(refused, Err(TaskError::Detached(_))));
    assert_eq!(
        b.get(&owner(), run.id()).await.unwrap().status(),
        TaskStatus::Failed,
        "input that reached no operation must not leave the task working"
    );
}

#[test]
fn a_signal_crosses_the_wire_unchanged() {
    let mut responses = Map::new();
    responses.insert("confirm".to_owned(), json!({ "action": "accept" }));
    let signals = [
        TaskSignal::Cancelled {
            task_id: serde_json::from_value(json!("task-1")).unwrap(),
        },
        TaskSignal::Input {
            task_id: serde_json::from_value(json!("task-2")).unwrap(),
            responses,
        },
    ];
    let wire: Vec<Value> = signals
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
    assert_eq!(
        wire[0],
        json!({ "signal": "cancelled", "taskId": "task-1" })
    );
    assert_eq!(
        wire[1],
        json!({
            "signal": "input",
            "taskId": "task-2",
            "responses": { "confirm": { "action": "accept" } }
        })
    );
    for (signal, value) in signals.iter().zip(wire) {
        let back: TaskSignal = serde_json::from_value(value).unwrap();
        assert_eq!(&back, signal);
    }
}

#[tokio::test]
async fn deliver_reports_whether_this_instance_runs_the_task() {
    let (a, b, _bus, _) = two_instances(TaskOptions::host_swept());
    let run = a.create(&owner()).await.unwrap();
    let signal = TaskSignal::Cancelled {
        task_id: run.id().clone(),
    };

    assert!(!b.deliver(signal.clone()), "B runs nothing for it");
    assert!(a.deliver(signal));
    assert!(run.is_cancelled());
}
