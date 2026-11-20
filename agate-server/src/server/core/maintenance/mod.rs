//! 凭证维护与模型目录更新的**执行记录**编排层。
//!
//! 上游 3.5.0 `core/maintenance/mod.rs` 的本地化实现：职责是「把每次执行落账 + 提供
//! 手动跑/重试入口」，真正的刷新动作仍调用本地已有的 `credential_maintenance` /
//! `providers::adapter` / `model_meta`（它们的执行逻辑含 disabled 黑名单、失败隔离，
//! 不在本层重复）。
//!
//! 接线：`core::scheduled_tasks::run_backend` 对这两个任务在 claim 之前转到
//! [`execute`]，于是**自动排期与手动跑走同一条记录链**。`api::maintenance` 的
//! `/run` 走 [`launch`]（异步、立即返回起始 Detail），`/retry` 走 [`retry`]。
//!
//! 与 `task_state` 的分工：那里是「下次何时跑 + 执行占位」，本层是「这次跑得怎样、
//! 逐条结果是什么」。两者都要 claim —— 本层用同一个 `task_state::claim`，不会双重
//! 占位（run_backend 对这两个任务提前返回，不再走到它自己的 claim）。

pub mod storage;
pub mod types;

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::server::config;
use crate::server::core::{
    account_store::AccountStore, credential_maintenance, model_meta, providers, task_state,
};
use crate::server::logging;
use types::{Detail, Item};

pub const CREDENTIAL: &str = config::KEY_CREDENTIAL_MAINTENANCE;
pub const MODEL: &str = config::KEY_MODEL_REFRESH;

/// 一次维护请求。全量跑用 [`Request::full`]；重试时由 [`retry`] 填充失败项。
pub struct Request {
    pub task_id: String,
    pub source: &'static str,
    pub accounts: Map<String, Value>,
    pub providers: Option<Vec<String>>,
    pub account_ids: Option<Vec<String>>,
    pub forced_account_ids: Vec<String>,
    pub metadata: bool,
    pub retry_of: Option<String>,
}

impl Request {
    pub fn full(task_id: &str, manual: bool) -> Self {
        Self {
            task_id: task_id.to_string(),
            source: if manual { "manual" } else { "automatic" },
            accounts: Map::new(),
            providers: None,
            account_ids: None,
            forced_account_ids: Vec::new(),
            metadata: task_id == MODEL,
            retry_of: None,
        }
    }
}

fn interval(task_id: &str) -> i64 {
    let settings = config::scheduled_settings();
    let minutes = if task_id == CREDENTIAL {
        settings.credential_maintenance.interval
    } else {
        settings.model_refresh.interval
    };
    minutes.max(1) * 60_000
}

/// 给日志与 scheduled_tasks 的一句话摘要（成功/跳过/失败 + models.dev 补充）。
pub fn summary(detail: &Detail) -> String {
    let run = &detail.run;
    let unit = if run.task_id == CREDENTIAL {
        "个账号"
    } else {
        "家提供商"
    };
    let mut text = format!(
        "成功 {} {unit}，跳过 {}，失败 {}",
        run.refreshed, run.skipped, run.failed
    );
    if let Some(metadata) = &run.metadata {
        match metadata.status.as_str() {
            "refreshed" => text.push_str(&format!(
                "；models.dev 已更新 {} 条",
                metadata.count.unwrap_or(0)
            )),
            "failed" => text.push_str(&format!(
                "；models.dev 更新失败：{}",
                metadata.message.as_deref().unwrap_or("未知原因")
            )),
            _ => (),
        }
    }
    text
}

/// 占位 + 起记录。手动跑越过失败冷却（用户往往刚修好那个原因）。
fn prepare(
    task_id: &str,
    manual: bool,
    retry_of: Option<&str>,
) -> Result<(task_state::RunGuard, storage::Recorder), String> {
    if !matches!(task_id, CREDENTIAL | MODEL) {
        return Err("未知的维护任务".to_string());
    }
    let backoff = if manual {
        task_state::ManualBackoff::Bypass
    } else {
        task_state::ManualBackoff::Respect
    };
    let guard = match task_state::claim(task_id, interval(task_id), manual, backoff, 1000)? {
        task_state::Claim::Acquired(guard) => guard,
        // 本地 task_state 只有 waiting_message()（上游另有 bypassing 变体），
        // 文案同样给出「还要等多久」，手动/自动共用即可。
        task_state::Claim::Deferred(state) => return Err(state.waiting_message()),
    };
    let recorder = storage::Recorder::start(
        task_id,
        if manual { "manual" } else { "automatic" },
        retry_of,
    )?;
    Ok((guard, recorder))
}

/// 真正干活：调本地刷新链，把逐条结果归一化后落记录。
async fn complete(
    store: &AccountStore,
    request: Request,
    recorder: storage::Recorder,
) -> Result<Detail, String> {
    let manual = request.source == "manual";
    let task_id = request.task_id.clone();
    let snapshots = types::accounts(store);
    let raw = if task_id == CREDENTIAL {
        // 本地凭证刷新是全量临期扫描（含 disabled 黑名单、失败隔离）。重试时
        // `account_ids` 非空 → 走 forced 强刷（不看临期，失败账号可能仍未临期）；
        // `forced_account_ids` 是同一批里显式标过 forced 的子集，一并纳入强刷。
        let mut selected = request.account_ids.clone().unwrap_or_default();
        for id in &request.forced_account_ids {
            if !selected.contains(id) {
                selected.push(id.clone());
            }
        }
        if selected.is_empty() {
            credential_maintenance::refresh_expiring_accounts(store).await
        } else {
            refresh_forced(store, &selected).await
        }
    } else if manual {
        providers::adapter::refresh_implemented_forced(
            store,
            &request.accounts,
            request.providers.as_deref(),
        )
        .await
    } else {
        providers::adapter::refresh_implemented(store).await
    };
    let mut items = raw
        .iter()
        .enumerate()
        .map(|(index, row)| types::item(row, index, &snapshots))
        .collect::<Vec<_>>();
    complete_missing_targets(&request, &snapshots, &mut items);
    let metadata = if task_id == MODEL && request.metadata {
        let started = Instant::now();
        let mut metadata = types::item(
            &model_meta::refresh(manual, interval(MODEL)).await,
            0,
            &HashMap::new(),
        );
        metadata.duration_ms = Some(started.elapsed().as_millis().min(i64::MAX as u128) as i64);
        Some(metadata)
    } else {
        None
    };
    recorder.finish(items, metadata)
}

/// 重试指定账号：逐个走适配器的强制刷新入口，形状与凭证维护结果一致。
/// 本地缺少上游 `refresh_accounts(store, selected, forced)`，这里就地补上等价动作，
/// 不改动原全量入口（自动排期仍走它）。
async fn refresh_forced(store: &AccountStore, ids: &[String]) -> Vec<Value> {
    let mut slots: Vec<Option<Value>> = vec![None; ids.len()];
    for (index, id) in ids.iter().enumerate() {
        let provider = store.list_accounts()["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|account| account.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .and_then(|account| account.get("provider").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let result = if let Some(kind) = providers::kind_from_id(&provider) {
            providers::adapter::adapter_for(kind)
                .refresh_access_token(store, id)
                .await
        } else {
            Err(crate::server::errors::GatewayError::with_status(
                400,
                "未注册的提供商",
            ))
        };
        slots[index] = Some(match result {
            Ok(_) => json!({"id": id, "provider": provider, "status": "refreshed"}),
            Err(error) => json!({
                "id": id,
                "provider": provider,
                "status": "failed",
                "message": types::safe_message(&error.message),
            }),
        });
    }
    slots.into_iter().flatten().collect()
}

fn complete_missing_targets(
    request: &Request,
    snapshots: &HashMap<String, types::AccountSnapshot>,
    items: &mut Vec<Item>,
) {
    if let Some(ids) = &request.account_ids {
        for id in ids {
            if items
                .iter()
                .any(|item| item.account_id.as_ref() == Some(id))
            {
                continue;
            }
            let raw =
                json!({ "id": id, "status": "skipped", "message": "账号已不存在，未执行维护" });
            items.push(types::item(&raw, items.len(), snapshots));
        }
    }
    if let Some(providers) = &request.providers {
        for provider in providers {
            if items.iter().any(|item| &item.provider == provider) {
                continue;
            }
            let raw = json!({ "provider": provider, "status": "skipped", "message": "提供商当前不可用，未执行目录更新" });
            items.push(types::item(&raw, items.len(), snapshots));
        }
    }
}

/// 同步执行（自动排期入口）：占位 → 记录 → 刷新 → 收尾排期。
pub async fn execute(store: &AccountStore, request: Request) -> Result<Detail, String> {
    let task_id = request.task_id.clone();
    let manual = request.source == "manual";
    let (guard, recorder) = prepare(&task_id, manual, request.retry_of.as_deref())?;
    let detail = complete(store, request, recorder).await;
    let note = detail
        .as_ref()
        .map(summary)
        .unwrap_or_else(|error| error.clone());
    let succeeded = detail
        .as_ref()
        .map(|detail| detail.run.failed == 0 || detail.run.refreshed > 0)
        .unwrap_or(false);
    let value = detail
        .as_ref()
        .ok()
        .map(|detail| json!({ "runId": detail.run.id }));
    let schedule = guard.finish(succeeded, note.clone(), value, 0, interval(&task_id));
    let task_name = if task_id == CREDENTIAL {
        "凭证维护"
    } else {
        "模型目录更新"
    };
    logging::log_with_level(
        "[Maintenance]",
        &format!("{task_name}：{}", types::safe_message(&note)),
        if detail.as_ref().map(|d| d.run.failed > 0).unwrap_or(true) {
            "error"
        } else {
            "info"
        },
    );
    let detail = detail?;
    if let Err(error) = schedule {
        return Err(format!("维护结果已保存，但排期更新失败：{error}"));
    }
    Ok(detail)
}

/// 手动跑（API 入口）：起异步任务，立即返回 `running` 的起始 Detail，界面随后轮询详情。
pub fn launch(store: AccountStore, request: Request) -> Result<Detail, String> {
    let task_id = request.task_id.clone();
    let manual = request.source == "manual";
    let (guard, recorder) = prepare(&task_id, manual, request.retry_of.as_deref())?;
    let started = Detail {
        run: recorder.run.clone(),
        results: Vec::new(),
    };
    crate::spawn_task(async move {
        let detail = complete(&store, request, recorder).await;
        let note = detail
            .as_ref()
            .map(summary)
            .unwrap_or_else(|error| error.clone());
        let succeeded = detail
            .as_ref()
            .map(|detail| detail.run.failed == 0 || detail.run.refreshed > 0)
            .unwrap_or(false);
        let value = detail
            .as_ref()
            .ok()
            .map(|detail| json!({ "runId": detail.run.id }));
        if let Err(error) = guard.finish(succeeded, note.clone(), value, 0, interval(&task_id)) {
            logging::log_with_level(
                "[Maintenance]",
                &types::safe_message(&format!("排期更新失败：{error}")),
                "error",
            );
        }
        if let Err(error) = detail {
            logging::log_with_level("[Maintenance]", &types::safe_message(&error), "error");
        }
    });
    Ok(started)
}

/// 重试：从历史详情里挑出失败项，构造一次新的手动跑。
///
/// 本地凭证刷新入口是全量临期扫描，重试会覆盖失败项（刷新幂等，不会重复扣费）；
/// 逐账号精确定位在 `complete` 里通过 `account_ids` 走 forced 路径。
pub fn retry(store: AccountStore, run_id: &str, item_id: Option<&str>) -> Result<Detail, String> {
    let previous = storage::detail(run_id)?;
    if previous.run.status == "running" {
        return Err("该次维护尚未结束".to_string());
    }
    let mut request = Request::full(&previous.run.task_id, true);
    request.retry_of = Some(previous.run.id.clone());
    request.metadata = previous
        .run
        .metadata
        .as_ref()
        .is_some_and(|item| item.status == "failed")
        && item_id.is_none();
    let failed = previous
        .results
        .iter()
        .filter(|item| item.status == "failed" && item_id.is_none_or(|id| item.id == id))
        .collect::<Vec<_>>();
    if failed.is_empty() && !request.metadata {
        return Err("没有可重试的失败项".to_string());
    }
    if request.task_id == CREDENTIAL {
        request.account_ids = Some(
            failed
                .iter()
                .filter_map(|item| item.account_id.clone())
                .collect(),
        );
        request.forced_account_ids = failed
            .iter()
            .filter(|item| item.forced)
            .filter_map(|item| item.account_id.clone())
            .collect();
    } else {
        request.providers = Some(failed.iter().map(|item| item.provider.clone()).collect());
        for item in &failed {
            if let Some(id) = &item.account_id {
                request.accounts.insert(item.provider.clone(), json!(id));
            }
        }
    }
    launch(store, request)
}
