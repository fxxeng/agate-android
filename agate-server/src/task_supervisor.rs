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
    /// not a per-task budget. Timed-out tasks are aborted before returning.
    pub async fn shutdown_scope(
        &self,
        scope: TaskScope,
        timeout: Duration,
    ) -> ShutdownReport {
        self.stopped_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(scope);

        let mut selected = Vec::new();
        {
            let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    use super::{CoreTaskSupervisor, TaskScope};
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
        tokio::task::yield_now().await;
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
}
