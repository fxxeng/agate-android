//! 模型目录的**共用刷新闸门**：定时任务、客户端拉 `/v1/models` 的被动刷新、
//! 「获取模型」的手动刷新都从这里过。
//!
//! ── 为什么需要闸门（它解决什么）──────────────────────────────
//! 改造前这条链有三处「真打上游」的入口，而它们的节奏互不知情：
//!   1. 启动时刷一次（每次打开软件都算）；
//!   2. 客户端**每次**拉 `/v1/models` 都起一个后台刷新任务（各家只有自己的
//!      内存 TTL 挡着，重启即失效）；
//!   3. 「模型目录刷新」定时任务按间隔再刷一次。
//! 三者叠加的后果是「同一份清单在几分钟内被反复拉」，账号多、家数多时尤其明显。
//! 现在三者共用 `core::task_state` 的持久化排期（键 `modelRefresh:<scope>`）：
//! 间隔、在途占位、失败冷却都跨重启保留，重启不再重置节奏。
//!
//! ── 手动路径：提前排期、越过失败冷却，但不越过并发 ──────────────
//! `manual = true` 跳过**普通排期**（用户按下按钮的预期就是「现在真的拉一次」，
//! 小浣熊的 TTL 早退会让「点了没反应」与坏掉无法区分），并越过**失败冷却**
//! （`ManualBackoff::Bypass`）：这一家的冷却记的是「上一轮为什么没成功」，而
//! 用户按按钮往往正是因为刚把那个原因修好（重新导入登录态、换账号）—— 继续拿
//! 旧结论挡着，界面上只会留着上一次的错误文案，看起来就是按钮坏了。
//! 在途占位与最短请求间隔仍然生效：它们防的是并发与连点，与冷却不是一回事。
//!
//! ── Cline 的两个池只拉一次 ───────────────────────────────────
//! `cline-free` / `cline-pass` 是两个 provider、两份清单，但底层是**同一个接口**
//! （`/ai/cline/recommended-models` 一次返回两池，见 `cline::models` 的模块头）。
//! 因此它们共用同一个排期键（`modelRefresh:cline`），且本轮第一家的结论会被
//! 第二家原样沿用 —— 否则一轮刷新里会对着同一个接口打两次。
//!
//! ── 硬约束：判定与占用必须在同一个写事务里 ────────────────────
//! `task_state::claim` 内部完成「判到期 + 写占位 + 记尝试」，因此两个进程
//! （开发版与正式版共用同一个库）或同一进程的多个调用点不会同时发出请求。

use serde_json::{json, Value};

use crate::server::config;
use crate::server::core::account_store::AccountStore;
use crate::server::core::task_state::{self, Claim};
use crate::server::logging;

use super::adapter::{adapter_for, implemented_kinds};
use super::{catalog, kind_id, meta, ProviderKind};

/// 逐家刷新一次（闸门见模块头）。返回**每家一行**的结果，形状见
/// `adapter::refresh_implemented_forced` 的文档注释（前后端契约）。
///
/// `providers` 是本次要刷的范围白名单（`None` = 全部已实现的家）：名单外的家
/// 既不打网络也不进结果。
pub async fn refresh(
    store: &AccountStore,
    accounts: &serde_json::Map<String, Value>,
    providers: Option<&[String]>,
    manual: bool,
) -> Vec<Value> {
    let mut results = Vec::new();
    let mut cline_result: Option<Value> = None;
    for kind in implemented_kinds() {
        let provider = kind_id(kind);
        if providers.is_some_and(|allowed| !allowed.iter().any(|id| id == provider)) {
            continue;
        }
        let adapter = adapter_for(kind);
        let requested = accounts.get(provider).and_then(Value::as_str).unwrap_or("").trim();
        let mut item = json!({
            "provider": provider,
            "providerLabel": meta(kind).label,
            "refreshedAt": catalog::refresh_meta(kind).1,
        });
        if adapter.refresh_uses_account() {
            let used = if requested.is_empty() {
                store.current_entry_for_provider(provider).map(|entry| entry.id)
            } else {
                Some(requested.to_string())
            };
            if let Some(id) = used { item["accountId"] = json!(id); }
        }
        if !adapter.supports_model_refresh() {
            item["status"] = json!("skipped");
            item["fixed"] = json!(true);
            item["message"] = json!("该提供商使用固定模型清单");
            results.push(item);
            continue;
        }
        let cline = matches!(kind, ProviderKind::ClineFree | ProviderKind::ClinePass);
        if cline {
            if let Some(previous) = &cline_result {
                // 两个池共用同一次远程拉取（接口一次返回两池）：第二家直接沿用
                // 第一家那一行的结论，不再打第二次上游。
                if let (Value::Object(target), Value::Object(source)) = (&mut item, previous) {
                    for field in ["status", "count", "message", "refreshedAt"] {
                        if let Some(value) = source.get(field) {
                            target.insert(field.to_string(), value.clone());
                        }
                    }
                }
                results.push(item);
                continue;
            }
        }
        let scope = if cline { "cline" } else { provider };
        let key = format!("modelRefresh:{scope}");
        let interval = config::scheduled_settings().model_refresh.interval * 60_000;
        // 手动越过失败冷却（见 ManualBackoff）：用户点「获取模型」就是要现在真打一次。
        // 自动路径照旧受它约束（`backoff` 在 `manual = false` 时不参与判定）。
        let backoff = if manual {
            task_state::ManualBackoff::Bypass
        } else {
            task_state::ManualBackoff::Respect
        };
        let guard = match task_state::claim(&key, interval, manual, backoff, 1_000) {
            Ok(Claim::Acquired(guard)) => guard,
            Ok(Claim::Deferred(state)) => {
                item["status"] = json!("skipped");
                item["message"] = json!(if state.retry_at > logging::now_ms() || state.running() {
                    state.waiting_message()
                } else { "沿用缓存，尚未到刷新时间".to_string() });
                item["retryAt"] = json!(state.retry_at);
                results.push(item);
                continue;
            }
            Err(error) => {
                item["status"] = json!("failed");
                item["message"] = json!(error);
                results.push(item);
                continue;
            }
        };
        let outcome = adapter.refresh_models(store, requested, manual).await;
        if outcome.refreshed {
            item["status"] = json!("refreshed");
            item["count"] = json!(outcome.count);
        } else if let Some(message) = outcome.message {
            item["status"] = json!("failed");
            item["message"] = json!(message);
        } else {
            item["status"] = json!("skipped");
            item["message"] = json!("沿用缓存或当前没有可用的目录来源");
        }
        item["refreshedAt"] = json!(catalog::refresh_meta(kind).1);
        let success = item["status"] != "failed";
        let summary = item.get("message").and_then(Value::as_str).unwrap_or("模型清单已刷新").to_string();
        // 间隔在这里**现读一次**（而不是复用开跑前那份）：这一轮可能跨几十秒的
        // 网络请求，期间用户完全可能把间隔改掉；用旧值排期会让界面显示「已生效」
        // 而实际按旧节奏跑（与 `scheduled_tasks::finish` 同一取舍）。
        let interval = config::scheduled_settings().model_refresh.interval * 60_000;
        if let Err(error) = guard.finish(success, summary, None, 0, interval) {
            item["status"] = json!("failed");
            item["message"] = json!(error);
        }
        if cline { cline_result = Some(item.clone()); }
        results.push(item);
    }
    results
}

/// 按新间隔重排各家的刷新（用户改了「模型目录刷新」的间隔时调用）。
///
/// 逐家都要改：真正的排期住在 `modelRefresh:<scope>` 上，任务级的那个键只决定
/// 「定时任务什么时候跑一轮」—— 只改任务级键会让各家的实际间隔留在旧值上。
pub fn reschedule(interval_ms: i64) -> Result<(), String> {
    for kind in implemented_kinds() {
        if !adapter_for(kind).supports_model_refresh() { continue; }
        let scope = if matches!(kind, ProviderKind::ClineFree | ProviderKind::ClinePass) {
            "cline"
        } else { kind_id(kind) };
        task_state::reschedule(&format!("modelRefresh:{scope}"), interval_ms)?;
    }
    // models.dev 元数据搭同一条任务、同一个间隔（见 `scheduled_tasks` 的
    // TASK_MODEL_REFRESH 分支）：它的排期键与各家同一命名空间，改间隔时若不
    // 一并重排，它会继续按旧节奏跑到下一轮自己领到新值为止。
    task_state::reschedule(
        &format!("modelRefresh:{}", crate::server::core::model_meta::SCHEDULE_SCOPE),
        interval_ms,
    )?;
    Ok(())
}

/// 账号**刚加上**时补拉一次这家的目录（后台，不阻塞响应；清单非空时什么都不做）。
///
/// ── 这一步解决的是「刚授权成功、模型页里却找不到这家」（真实故障）──
/// 有一批家**没有内置兜底清单**（ModelScope / Loomy / AtomCode / Trae 等：清单
/// 只可能来自远程，编一份静态表会让用户看到自己其实调不通的模型）。此前「加
/// 账号」只落库、不拉清单，于是这些家在两处**同时消失**：
///   · 模型管理页没有它的行 —— `catalog::active_manifests` 把**空清单**的家整个
///     滤掉（那里的判据是「有清单才算一家」）；
///   · 「拉取模型」弹层的候选 = 页面上的家 ∪ 自定义家 ⇒ 它也不在候选里，用户
///     连「对这家拉一次」都点不到。
/// 两件事合成一个死循环：**要有模型才能拉模型**。补拉一次即解开 —— 清单落地后
/// 页面有行，之后的手动刷新与定时任务照旧（自定义家早就有同样的处理，见客户端
/// `PullModelsDialog` 对零模型家的候选保底）。
///
/// ── 为什么只在「清单为空」时打这一次 ────────────────────────
/// 清单非空的家在页面上已经看得见、随时能手动刷新；在添加路径上无条件打一次
/// 上游只会让「加账号」凭空多一次往返（批量导入时更明显）。空清单才是死循环
/// 的那一半。
///
/// ── 为什么不看「模型目录刷新」定时任务的开关 ────────────────
/// 那是**周期任务**的偏好（`scheduled_settings().model_refresh`）。用户刚按下
/// 「添加账号」、界面上却什么都没有，是当次操作的直接后果，不该被一个周期开关
/// 挡掉。
///
/// 自定义家（`custom-…`）不在这里补：它们不在 `ProviderKind` 里，目录走
/// `custom_providers` 自己的拉取路径。
pub fn spawn_initial_refresh(store: &AccountStore, provider_id: &str, account_id: &str) {
    let provider = provider_id.trim();
    let Some(kind) = super::kind_from_id(provider) else { return; };
    let adapter = adapter_for(kind);
    if !adapter.supports_model_refresh() || !adapter.list_models().is_empty() {
        return;
    }
    // `AccountStore` 是 `Arc<Inner>` 的轻量句柄（见 `store.rs` 的说明），clone 只是
    // 抬一次引用计数 —— 后台任务要独占一个句柄，不能借用请求里的那个。
    let store = store.clone();
    let account = account_id.trim().to_string();
    let label = provider.to_string();
    crate::spawn_task(async move {
        let outcome = adapter.refresh_models(&store, &account, true).await;
        if outcome.refreshed {
            let count = outcome.count;
            logging::log("[Models]", &format!("{label} 添加账号后补拉目录：落地 {count} 个模型"));
        } else if let Some(reason) = outcome.message {
            // 失败不往上抛（添加本身已经成功）：这条日志是用户唯一的线索 ——
            // 清单仍为空时模型页会把这家藏起来，只有这里说得清为什么。
            logging::log(
                "[Models]",
                &format!("{label} 添加账号后补拉目录失败（该家仍无模型可展示）：{reason}"),
            );
        } else {
            logging::verbose(
                "[Models]",
                &format!("{label} 添加账号后补拉目录：本次没有新清单"),
            );
        }
    });
}
