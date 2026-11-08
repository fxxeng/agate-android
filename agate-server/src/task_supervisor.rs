//! Process-scoped task ownership for the Rust Core.
//!
//! Tasks are registered by scope and stable name; the name may carry caller-level
//! identity (e.g. `checkin-fire:<schedule-id>`) so the same idempotency rule
//! doubles as per-entity re-entry protection. Registration is idempotent for a
//! live task; shutdown is explicit, scoped, bounded, and safe to call repeatedly.
//! This module deliberately does not own Tokio runtimes or host lifecycle hooks.
//!
//! Completed entries are reclaimed at the next registration (T1.2). Their
//! outcome is read out before the entry is dropped: successes and cancellations
//! are released, panics are archived into a bounded in-memory history
//! ([`CoreTaskSupervisor::recent_task_failures`]). The shutdown report is
//! untouched — every entry still registered at shutdown is cancelled, joined,
//! and reported exactly as before, so a panic that was never reclaimed still
//! reaches `ShutdownReport.failed_tasks`.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
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
    name: Arc<str>,
    cancel: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

/// 失败历史的容量上限（T1.2）。历史只回答「最近有没有 panic、是谁」——是
/// 排查窗口，不是审计日志；超出即丢最旧。fire 节奏 30s/条目，64 条已远超
/// 排查所需。
const FAILURE_HISTORY_CAP: usize = 64;

/// 一条已回收任务的失败记录（panic 结局）。scope + 名字即可定位来源任务。
struct FailureRecord {
    scope: TaskScope,
    name: Arc<str>,
}

/// Owns task handles; it does not own the Tokio runtime that executes them.
pub struct CoreTaskSupervisor {
    tasks: Mutex<Vec<TaskEntry>>,
    stopped_scopes: Mutex<HashSet<TaskScope>>,
    failures: Mutex<VecDeque<FailureRecord>>,
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
            failures: Mutex::new(VecDeque::new()),
        }
    }

    /// Register a task. A duplicate live `(scope, name)` is ignored; a completed
    /// task may be registered again. Tasks decide their own safe cancellation points.
    pub fn spawn<F, Fut>(
        &self,
        scope: TaskScope,
        name: impl Into<Arc<str>>,
        task: F,
    ) -> Result<bool, TaskSpawnError>
    where
        F: FnOnce(TaskCancellation) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let name = name.into();
        let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.reclaim_finished(&mut tasks);
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
                        failed_tasks.push(selected[index].name.clone());
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

    /// 最近的任务失败记录（T1.2 有限历史快照，最新在尾部）。
    ///
    /// 停机报告只覆盖停机时**仍在册**的条目；运行期被回收的 panic 落在这里。
    /// 当前无生产消费方（停机排查/后续接线用），读它不影响任何状态。
    pub fn recent_task_failures(&self) -> Vec<(TaskScope, Arc<str>)> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|record| (record.scope, record.name.clone()))
            .collect()
    }

    /// 回收已完成条目（T1.2，在 spawn 持 tasks 锁时执行）。
    ///
    /// 不是 `retain(!finished)` 一刀切：已结束条目的 JoinHandle 里存着结局，
    /// 直接丢会把 panic 连同证据一起丢掉，停机报告与排查都失去依据。这里先
    /// 把结局读出来再删——成功/被取消直接释放；panic 写入有限长度历史。
    /// `is_finished() == true` 时结局必然就绪，`Pending` 分支是不可达的防御。
    fn reclaim_finished(&self, tasks: &mut Vec<TaskEntry>) {
        tasks.retain_mut(|entry| {
            if !entry.handle.is_finished() {
                return true;
            }
            match ready_join_result(&mut entry.handle) {
                // 成功完成：结局无保留价值，条目回收。
                Some(Ok(())) => false,
                // 被取消：同上。
                Some(Err(error)) if error.is_cancelled() => false,
                // panic：唯一有归档价值的结局——进有限历史，条目回收。
                Some(Err(error)) if error.is_panic() => {
                    self.record_failure(entry.scope, entry.name.clone());
                    false
                }
                // JoinError 只有取消/panic 两类，此臂纯为穷尽性兜底。
                Some(Err(_)) => false,
                // 不可达（结局未就绪不会 is_finished）；防御性保留，交给下一
                // 次回收或停机流程。
                None => true,
            }
        });
    }

    fn record_failure(&self, scope: TaskScope, name: Arc<str>) {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while failures.len() >= FAILURE_HISTORY_CAP {
            failures.pop_front();
        }
        failures.push_back(FailureRecord { scope, name });
    }

    /// 仅测试：内部条目数（含已完成的待回收条目），用于钉「列表有界」。
    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// 同步读取一个已完成任务的结局。调用前提：`handle.is_finished()` 为真——
/// 此时输出已就绪，noop waker 下一次 poll 必定 Ready。
fn ready_join_result(handle: &mut JoinHandle<()>) -> Option<Result<(), tokio::task::JoinError>> {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    match std::pin::Pin::new(handle).poll(&mut cx) {
        Poll::Ready(result) => Some(result),
        Poll::Pending => None,
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
    pub failed_tasks: Vec<Arc<str>>,
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
    use super::{CoreTaskSupervisor, FAILURE_HISTORY_CAP, TaskScope, TaskSpawnError};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    /// 等到 scope 内没有任何在途任务（`active_count == 0` ⟺ 所有条目已结束）。
    /// `crate::spawn_task` 落在全局多线程运行时（lib.rs:73-79），测试线程的
    /// `yield_now` 对它没有任何调度保证——完成判定只能轮询 supervisor 自身
    /// 状态，不能猜时序。
    async fn settle_finished(supervisor: &CoreTaskSupervisor, scope: TaskScope) {
        let mut waited = 0u32;
        while supervisor.active_count(scope) != 0 {
            assert!(waited < 2000, "任务完成滞后：active_count 未归零");
            tokio::time::sleep(Duration::from_millis(1)).await;
            waited += 1;
        }
    }

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
        assert_eq!(report.failed_tasks, vec![Arc::from("panicking")]);
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

    /// R47 T1.1 饥饿回归锚点：条目级身份（动态名）不得互相阻挡 —— 旧全局
    /// 固定名曾让整天 due 的条目锁死其后所有计划（见 auto_checkin 模块头）。
    /// 同名防重入语义保持不变。
    #[tokio::test]
    async fn distinct_entry_names_do_not_block_each_other() {
        let supervisor = CoreTaskSupervisor::new();
        let (release_a, wait_a) = oneshot::channel::<()>();
        let (release_b, wait_b) = oneshot::channel::<()>();
        assert!(supervisor
            .spawn(TaskScope::Core, "checkin-fire:entry-a", move |mut cancellation| async move {
                tokio::select! {
                    _ = wait_a => {}
                    _ = cancellation.cancelled() => {}
                }
            })
            .unwrap());
        assert!(supervisor
            .spawn(TaskScope::Core, "checkin-fire:entry-b", move |mut cancellation| async move {
                tokio::select! {
                    _ = wait_b => {}
                    _ = cancellation.cancelled() => {}
                }
            })
            .unwrap());
        assert_eq!(supervisor.active_count(TaskScope::Core), 2);
        assert!(!supervisor
            .spawn(TaskScope::Core, "checkin-fire:entry-a", |_| async {})
            .unwrap());
        let _ = release_a.send(());
        let _ = release_b.send(());
        let _ = supervisor
            .shutdown_scope(TaskScope::Core, Duration::from_secs(1))
            .await;
    }

    /// R47 T1.1：动态任务名原样穿透到停机报告（报告须能定位到具体条目）。
    #[tokio::test]
    async fn failed_tasks_carry_dynamic_names() {
        let supervisor = CoreTaskSupervisor::new();
        let entry_id = "entry-42";
        let name = format!("checkin-fire:{entry_id}");
        supervisor
            .spawn(TaskScope::Core, name.clone(), |_| async {
                panic!("test task panic");
            })
            .unwrap();
        tokio::task::yield_now().await;
        let report = supervisor
            .shutdown_scope(TaskScope::Core, Duration::from_secs(1))
            .await;
        assert_eq!(report.failed_tasks, vec![Arc::from(name.as_str())]);
    }

    /// T1.2：同名任务连续注册完成 N 次，内部列表有界——回收发生在 spawn 时，
    /// 列表里至多留一条「刚完成、还没被下一次 spawn 碰到」的条目。
    #[tokio::test]
    async fn finished_entries_are_reclaimed_at_spawn() {
        let supervisor = CoreTaskSupervisor::new();
        supervisor
            .spawn(TaskScope::Core, "churn", |_| async {})
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;
        for _ in 0..10_000 {
            // 上一轮任务已确认结束；本次 spawn 的回收扫掉它
            assert!(supervisor
                .spawn(TaskScope::Core, "churn", |_| async {})
                .unwrap());
            settle_finished(&supervisor, TaskScope::Core).await;
        }
        assert_eq!(supervisor.entry_count(), 1);
        assert_eq!(supervisor.active_count(TaskScope::Core), 0);
        assert!(supervisor.recent_task_failures().is_empty());
    }

    /// T1.2：panic 条目被后续 spawn 回收时，panic 进有限历史（成功条目不留痕、
    /// 条目本身被回收，Arc 名字随之释放）。
    #[tokio::test]
    async fn reclaimed_panics_land_in_bounded_history() {
        let supervisor = CoreTaskSupervisor::new();
        supervisor
            .spawn(TaskScope::Core, "boom-1", |_| async {
                panic!("test task panic");
            })
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;
        supervisor
            .spawn(TaskScope::Core, "ok-1", |_| async {})
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;
        supervisor
            .spawn(TaskScope::Core, "boom-2", |_| async {
                panic!("test task panic");
            })
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;
        supervisor
            .spawn(TaskScope::Core, "ok-2", |_| async {})
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;

        assert_eq!(
            supervisor.recent_task_failures(),
            vec![
                (TaskScope::Core, Arc::from("boom-1")),
                (TaskScope::Core, Arc::from("boom-2")),
            ]
        );
        assert_eq!(supervisor.entry_count(), 1);
    }

    /// T1.2：历史有界——超出容量的最旧记录被挤掉，保留最近的 CAP 条。
    #[tokio::test]
    async fn failure_history_is_bounded() {
        let supervisor = CoreTaskSupervisor::new();
        for index in 0..(FAILURE_HISTORY_CAP + 16) {
            supervisor
                .spawn(TaskScope::Core, format!("boom-{index}"), |_| async {
                    panic!("test task panic");
                })
                .unwrap();
            settle_finished(&supervisor, TaskScope::Core).await;
            supervisor
                .spawn(TaskScope::Core, format!("ok-{index}"), |_| async {})
                .unwrap();
            settle_finished(&supervisor, TaskScope::Core).await;
        }

        let failures = supervisor.recent_task_failures();
        assert_eq!(failures.len(), FAILURE_HISTORY_CAP);
        assert_eq!(
            failures.first(),
            Some(&(TaskScope::Core, Arc::from(format!("boom-{}", 16))))
        );
        assert_eq!(
            failures.last(),
            Some(&(
                TaskScope::Core,
                Arc::from(format!("boom-{}", FAILURE_HISTORY_CAP + 15))
            ))
        );
    }

    /// T1.2 定案红线：回收只发生在 spawn 时——panic 条目若未被下一次 spawn
    /// 碰到，停机报告必须照旧报出（不得吞掉未回收任务的失败）；停机路径不写
    /// 历史（报告本身就是归档，避免同一次失败记两笔）。
    #[tokio::test]
    async fn shutdown_still_reports_unreclaimed_failure() {
        let supervisor = CoreTaskSupervisor::new();
        supervisor
            .spawn(TaskScope::Core, "boom-late", |_| async {
                panic!("test task panic");
            })
            .unwrap();
        settle_finished(&supervisor, TaskScope::Core).await;

        assert!(
            supervisor.recent_task_failures().is_empty(),
            "未被回收的失败不进历史"
        );
        let report = supervisor
            .shutdown_scope(TaskScope::Core, Duration::from_secs(1))
            .await;
        assert_eq!(
            report.failed_tasks,
            vec![Arc::from("boom-late")],
            "停机报告不得吞掉未回收任务的失败"
        );
        assert!(supervisor.recent_task_failures().is_empty());
    }
}
