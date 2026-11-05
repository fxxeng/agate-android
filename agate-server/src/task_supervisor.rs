//! Process-scoped task ownership for the Rust Core.
//!
//! Tasks are registered by scope and stable name. Registration is idempotent for a
//! live task; shutdown is explicit, scoped, bounded, and safe to call repeatedly.
//! This module deliberately does not own Tokio runtimes or host lifecycle hooks.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{timeout_at, Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TaskScope {
    Core,
    Gateway,
    Request,
    Sidecar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskSpawnError {
    ScopeStopped,
}

impl std::fmt::Display for TaskSpawnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScopeStopped => formatter.write_str("task scope is already stopped"),
        }
    }
}

/// Cooperative cancellation handle passed into each managed task.
pub struct TaskCancellation {
    receiver: oneshot::Receiver<()>,
}

impl TaskCancellation {
    /// Resolve when the owning scope requests shutdown.
    pub async fn cancelled(&mut self) {
        let _ = (&mut self.receiver).await;
    }

    /// Wait for either cancellation or a delay. Returns false on cancellation.
    pub async fn wait_or_cancel(&mut self, delay: Duration) -> bool {
        tokio::select! {
            _ = self.cancelled() => false,
            _ = tokio::time::sleep(delay) => true,
        }
    }

    /// Non-blocking cancellation check, intended for safe points between units of work.
    pub fn is_cancelled(&mut self) -> bool {
        match self.receiver.try_recv() {
            Ok(()) | Err(oneshot::error::TryRecvError::Closed) => true,
            Err(oneshot::error::TryRecvError::Empty) => false,
        }
    }
}

struct TaskEntry {
    scope: TaskScope,
    name: &'static str,
    cancel: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

/// Owns task handles; it does not own the Tokio runtime that executes them.
pub struct CoreTaskSupervisor {
    tasks: Mutex<Vec<TaskEntry>>,
    stopped_scopes: Mutex<HashSet<TaskScope>>,
}

impl Default for CoreTaskSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreTaskSupervisor {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            stopped_scopes: Mutex::new(HashSet::new()),
        }
    }

    /// Register a task. A duplicate live `(scope, name)` is ignored; a completed
    /// task may be registered again. Tasks decide their own safe cancellation points.
    pub fn spawn<F, Fut>(
        &self,
        scope: TaskScope,
        name: &'static str,
        task: F,
    ) -> Result<bool, TaskSpawnError>
    where
        F: FnOnce(TaskCancellation) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let stopped = self
            .stopped_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stopped.contains(&scope) {
            return Err(TaskSpawnError::ScopeStopped);
        }
        if tasks.iter().any(|entry| {
            entry.scope == scope && entry.name == name && !entry.handle.is_finished()
        }) {
            return Ok(false);
        }
        drop(stopped);

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let handle = crate::spawn_task(task(TaskCancellation { receiver: cancel_rx }));
        tasks.push(TaskEntry {
            scope,
            name,
            cancel: Some(cancel_tx),
            handle,
        });
        Ok(true)
    }

    /// Cancel and join every task in a scope. The timeout is a total scope budget,
    /// not a per-task budget. Timed-out tasks are aborted, then given up to one
    /// additional second for their futures to drop before returning.
    pub async fn shutdown_scope(
        &self,
        scope: TaskScope,
        timeout: Duration,
    ) -> ShutdownReport {
        let mut selected = Vec::new();
        {
            // Match spawn() lock order: tasks -> stopped_scopes. Holding tasks while
            // marking the scope stopped makes the stop gate and task drain one
            // serialized operation relative to concurrent registration.
            let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.stopped_scopes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(scope);
            let mut retained = Vec::with_capacity(tasks.len());
            for entry in tasks.drain(..) {
                if entry.scope == scope {
                    selected.push(entry);
                } else {
                    retained.push(entry);
                }
            }
            *tasks = retained;
        }

        let count = selected.len();
        for entry in &mut selected {
            if let Some(cancel) = entry.cancel.take() {
                let _ = cancel.send(());
            }
        }

        let deadline = Instant::now() + timeout;
        let mut failed_tasks = Vec::new();
        for index in 0..selected.len() {
            match timeout_at(deadline, &mut selected[index].handle).await {
                Ok(Ok(())) => {},
                Ok(Err(error)) => {
                    if error.is_panic() {
                        failed_tasks.push(selected[index].name);
                    }
                }
                Err(_) => {
                    let remaining = selected.len().saturating_sub(index);
                    for entry in selected.iter().skip(index) {
                        entry.handle.abort();
                    }
                    // abort() 只提交取消请求，不代表 Future 已经 Drop。等待一个
                    // 短的清理宽限，确保资源 owner（尤其 Sidecar worker）退出后，
                    // 调用方再执行 Child/文件等外部资源的最终回收。
                    let abort_join_deadline = Instant::now() + Duration::from_secs(1);
                    for entry in selected.iter_mut().skip(index) {
                        let _ = timeout_at(abort_join_deadline, &mut entry.handle).await;
                    }
                    return ShutdownReport {
                        tasks_joined: index,
                        tasks_aborted: remaining,
                        timed_out: true,
                        failed_tasks,
                    };
                }
            }
        }

        ShutdownReport {
            tasks_joined: count,
            tasks_aborted: 0,
            timed_out: false,
            failed_tasks,
        }
    }

    pub fn active_count(&self, scope: TaskScope) -> usize {
        let tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.iter().filter(|entry| entry.scope == scope && !entry.handle.is_finished()).count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    pub tasks_joined: usize,
    /// Number of tasks for which abort was requested after the scope deadline.
    pub tasks_aborted: usize,
    pub timed_out: bool,
    /// Names of tasks that terminated with a panic before/during shutdown in
    /// unwind builds. Release profiles using `panic=abort` cannot report panics here.
    pub failed_tasks: Vec<&'static str>,
}

static CORE_TASK_SUPERVISOR: OnceLock<CoreTaskSupervisor> = OnceLock::new();

/// Process-wide supervisor shared by Desktop, headless, and Android hosts.
pub fn core_task_supervisor() -> &'static CoreTaskSupervisor {
    CORE_TASK_SUPERVISOR.get_or_init(CoreTaskSupervisor::new)
}

/// Stop the process-lifetime Core loops. Host wiring is intentionally a later batch.
pub async fn shutdown_core_tasks(timeout: Duration) -> ShutdownReport {
    core_task_supervisor().shutdown_scope(TaskScope::Core, timeout).await
}

#[cfg(test)]
mod tests {
    use super::{CoreTaskSupervisor, TaskScope, TaskSpawnError};
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn duplicate_live_registration_is_ignored() {
        let supervisor = CoreTaskSupervisor::new();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        assert!(supervisor.spawn(TaskScope::Core, "loop", move |mut cancellation| async move {
            tokio::select! {
                _ = release_rx => {},
                _ = cancellation.cancelled() => {},
            }
        }).unwrap());
        assert!(!supervisor.spawn(TaskScope::Core, "loop", |_| async {}).unwrap());
        assert_eq!(supervisor.active_count(TaskScope::Core), 1);
        let _ = release_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), async {
            while supervisor.active_count(TaskScope::Core) != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("released task should leave the active set");
        assert_eq!(supervisor.active_count(TaskScope::Core), 0);
    }

    #[tokio::test]
    async fn shutdown_cancels_and_joins_only_requested_scope() {
        let supervisor = CoreTaskSupervisor::new();
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (other_tx, other_rx) = oneshot::channel::<()>();
        supervisor.spawn(TaskScope::Core, "core", move |mut cancellation| async move {
            let _ = started_tx.send(());
            cancellation.cancelled().await;
        }).unwrap();
        supervisor.spawn(TaskScope::Gateway, "gateway", move |mut cancellation| async move {
            let _ = other_tx.send(());
            cancellation.cancelled().await;
        }).unwrap();
        let _ = started_rx.await;
        let _ = other_rx.await;

        let report = supervisor.shutdown_scope(TaskScope::Core, Duration::from_secs(1)).await;
        assert_eq!(report.tasks_joined, 1);
        assert!(!report.timed_out);
        assert_eq!(supervisor.active_count(TaskScope::Core), 0);
        assert_eq!(supervisor.active_count(TaskScope::Gateway), 1);
        assert!(supervisor.spawn(TaskScope::Core, "late", |_| async {}).is_err());
        let _ = supervisor.shutdown_scope(TaskScope::Gateway, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let supervisor = CoreTaskSupervisor::new();
        supervisor.spawn(TaskScope::Core, "loop", |mut cancellation| async move {
            cancellation.cancelled().await;
        }).unwrap();
        let first = supervisor.shutdown_scope(TaskScope::Core, Duration::from_secs(1)).await;
        let second = supervisor.shutdown_scope(TaskScope::Core, Duration::from_secs(1)).await;
        assert_eq!(first.tasks_joined, 1);
        assert_eq!(second.tasks_joined, 0);
        assert!(!second.timed_out);
    }

    #[tokio::test]
    async fn shutdown_reports_panicked_tasks() {
        let supervisor = CoreTaskSupervisor::new();
        supervisor.spawn(TaskScope::Core, "panicking", |_| async {
            panic!("test task panic");
        }).unwrap();
        tokio::task::yield_now().await;
        let report = supervisor.shutdown_scope(TaskScope::Core, Duration::from_secs(1)).await;
        assert_eq!(report.tasks_joined, 1);
        assert_eq!(report.failed_tasks, vec!["panicking"]);
        assert!(!report.timed_out);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_racing_shutdown_has_consistent_lock_order() {
        use std::sync::{Arc, Barrier};

        let supervisor = Arc::new(CoreTaskSupervisor::new());
        let barrier = Arc::new(Barrier::new(2));
        let spawn_supervisor = Arc::clone(&supervisor);
        let spawn_barrier = Arc::clone(&barrier);
        let registration = tokio::task::spawn_blocking(move || {
            spawn_barrier.wait();
            spawn_supervisor.spawn(
                TaskScope::Core,
                "racing-task",
                |mut cancellation| async move {
                    cancellation.cancelled().await;
                },
            )
        });

        barrier.wait();
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            supervisor.shutdown_scope(TaskScope::Core, Duration::from_secs(1)),
        )
        .await
        .expect("shutdown must not deadlock with concurrent spawn");
        let registration = tokio::time::timeout(Duration::from_secs(2), registration)
            .await
            .expect("registration thread must finish")
            .expect("registration task must join");

        assert_eq!(supervisor.active_count(TaskScope::Core), 0);
        match registration {
            Ok(true) => assert_eq!(report.tasks_joined, 1),
            Err(TaskSpawnError::ScopeStopped) => assert_eq!(report.tasks_joined, 0),
            Ok(false) => panic!("unexpected duplicate registration"),
        }
    }

    #[tokio::test]
    async fn timeout_abort_waits_for_task_future_drop() {
        struct DropNotice(Option<oneshot::Sender<()>>);
        impl Drop for DropNotice {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let supervisor = CoreTaskSupervisor::new();
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        supervisor
            .spawn(TaskScope::Sidecar, "stuck-worker", move |_| async move {
                let _notice = DropNotice(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            })
            .unwrap();
        started_rx.await.expect("worker should start before timeout");

        let report = supervisor
            .shutdown_scope(TaskScope::Sidecar, Duration::from_millis(10))
            .await;
        assert!(report.timed_out);
        assert_eq!(report.tasks_aborted, 1);
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("aborted task should be dropped before shutdown returns")
            .expect("drop notification should be delivered");
    }
}
