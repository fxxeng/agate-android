//! GET/PUT /api/routing + GET /api/routing/queue —— 账号路由的设置与**队列可见化**。
//!
//! ── /api/routing（与 /api/timeouts 同一模式）─────────────────
//! 独立端点、允许部分字段、返回生效后的全量值、无副作用（不重启进程）。
//! 保存后对下一个请求立即生效（转发层逐请求读 `config::routing_settings()`
//! 内存快照，不读盘）。
//!
//! ── /api/routing/queue（队列可见化，账号页面板的数据源）────────
//! 回答「按当前策略，这个模型的候选账号会怎么排、谁被跳过、为什么」。
//! **排序实现与转发选路是同一个函数**（`routing::plan_route`）—— 转发那次
//! 怎么排，这里就怎么画；两处若各写一份，界面标的顺序迟早和实际转发分叉
//! （这正是「候选队列可见化」要消灭的那类问题）。
//!
//! 与 `/api/session` 的 `routedAccountId` 的关系：那个字段只回答「队首是谁」
//! （★ 的数据源，保持不变），本端点给的是**整张清单 + 原因句** —— 面板要
//! 展示每一位，顺带把被跳过者也摆出来（禁用 / 冷却 / 并发满 / 限制器 /
//! 无凭证各有各的原因，用户不用再猜）。
//!
//! `model` 查询参数可省：省略（或空）时按「不限模型」的全局队列展开
//! （不收窄到「提供该模型的家」）—— 面板在没有模型上下文时也有得画。

use std::collections::{HashMap, HashSet};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::config::{self, RoutingPatch, RoutingSettings};
use crate::server::core::providers::router;
use crate::server::core::routing;
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::ServerState;

/// GET /api/routing
pub async fn get_routing(State(_state): State<ServerState>) -> Response {
    ok_json(routing_json(config::routing_settings()))
}

/// PUT /api/routing —— body 各键全部可选（不给 = 不动）
pub async fn put_routing(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let mut patch = RoutingPatch::default();
    if let Some(value) = object.get(config::KEY_ROUTING_STRATEGY) {
        let Some(text) = value.as_str() else {
            return errors::management_error(400, "strategy 必须是字符串");
        };
        let known = [
            config::ROUTING_STRATEGY_ORDER,
            config::ROUTING_STRATEGY_ROTATE,
            config::ROUTING_STRATEGY_PROVIDER,
        ];
        if !known.contains(&text) {
            return errors::management_error(
                400,
                format!("未知的路由策略「{text}」（只支持 {}）", known.join(" / ")),
            );
        }
        patch.strategy = Some(config::RouteStrategy::from_id(text));
    }
    if let Some(value) = object.get(config::KEY_ROUTING_STICKY) {
        let Some(text) = value.as_str() else {
            return errors::management_error(400, "sticky 必须是字符串");
        };
        let known = [
            config::STICKY_AUTO,
            config::STICKY_SESSION,
            config::STICKY_TURN,
            config::STICKY_OFF,
        ];
        if !known.contains(&text) {
            return errors::management_error(
                400,
                format!("未知的会话粘性档位「{text}」（只支持 {}）", known.join(" / ")),
            );
        }
        patch.sticky = Some(config::StickyMode::from_id(text));
    }
    let bool_field = |key: &str| -> Result<Option<bool>, Response> {
        match object.get(key) {
            None => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(errors::management_error(400, format!("{key} 必须是布尔值"))),
        }
    };
    let failover_backoff = match bool_field(config::KEY_ROUTING_FAILOVER_BACKOFF) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let sink = match bool_field(config::KEY_ROUTING_SINK) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let breaker_enabled = match bool_field(config::KEY_ROUTING_BREAKER_ENABLED) {
        Ok(value) => value,
        Err(response) => return response,
    };
    patch.failover_backoff = failover_backoff;
    patch.sink = sink;
    patch.breaker_enabled = breaker_enabled;
    let int_field = |key: &str, min: i64, max: i64| -> Result<Option<i64>, Response> {
        match object.get(key) {
            None => Ok(None),
            Some(value) => match value.as_i64() {
                Some(count) if (min..=max).contains(&count) => Ok(Some(count)),
                _ => Err(errors::management_error(
                    400,
                    format!("{key} 必须是 {min}~{max} 的整数"),
                )),
            },
        }
    };
    patch.breaker_threshold = match int_field(
        config::KEY_ROUTING_BREAKER_THRESHOLD,
        config::ROUTING_BREAKER_MIN_THRESHOLD,
        config::ROUTING_BREAKER_MAX_THRESHOLD,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    patch.breaker_timeout_seconds = match int_field(
        config::KEY_ROUTING_BREAKER_TIMEOUT,
        config::ROUTING_BREAKER_MIN_TIMEOUT,
        config::ROUTING_BREAKER_MAX_TIMEOUT,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    patch.breaker_success_threshold = match int_field(
        config::KEY_ROUTING_BREAKER_SUCCESS,
        config::ROUTING_BREAKER_MIN_SUCCESS,
        config::ROUTING_BREAKER_MAX_SUCCESS,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !config::set_routing(patch) {
        return errors::management_error(500, "路由设置落盘失败");
    }
    ok_json(routing_json(config::routing_settings()))
}

fn routing_json(settings: RoutingSettings) -> Value {
    json!({
        config::KEY_ROUTING_STRATEGY: settings.strategy.as_str(),
        config::KEY_ROUTING_STICKY: settings.sticky.as_str(),
        config::KEY_ROUTING_FAILOVER_BACKOFF: settings.failover_backoff,
        config::KEY_ROUTING_SINK: settings.sink,
        config::KEY_ROUTING_BREAKER_ENABLED: settings.breaker_enabled,
        config::KEY_ROUTING_BREAKER_THRESHOLD: settings.breaker_threshold,
        config::KEY_ROUTING_BREAKER_TIMEOUT: settings.breaker_timeout_seconds,
        config::KEY_ROUTING_BREAKER_SUCCESS: settings.breaker_success_threshold,
    })
}

/// 「按模型算一次选路」的**共享事实**。
///
/// 与具体模型无关的那些（账号全集、连接数、限制器拦截、沉底/退避/熔断、
/// 策略、时间基准）只算一次；每个模型只多做一次 `plan_route`。
/// 批量问 N 个模型的可用账号数时，不能把连接快照与余额事实重算 N 遍。
///
/// ── 为什么抽出来而不是在两处各写一遍 ────────────────────────
/// 选路的判定有六道门（禁用 / 限制器 / 冷却 / 并发 / 无凭证 / 已试过），
/// 抄一份到新端点，将来转发层加第七道门时这里不会跟着变 —— 界面就会开始
/// 显示「3 个可用」而实际只有 2 个。这类分叉在真机上极难定位，
/// 所以宁可让两个端点共用同一个 `plan_route` 调用点。
struct PlanFacts {
    accounts: Vec<Value>,
    counts: HashMap<String, usize>,
    limiter_blocked: HashSet<String>,
    sunk: HashMap<String, i64>,
    resting: HashMap<String, i64>,
    open: HashSet<String>,
    strategy: crate::server::config::RouteStrategy,
    now: i64,
}

impl PlanFacts {
    fn collect(state: &ServerState) -> Self {
        let accounts = routing::accounts_of(&state.store().list_accounts());
        let now = crate::server::logging::now_ms();
        let counts: HashMap<String, usize> = state
            .upstream()
            .connections()
            .snapshot()
            .into_iter()
            .collect();
        // 限制器拦截与模型无关（判据是余额/周期用量），所以对**全部**账号算一次；
        // `plan_route` 只对传给它的 scoped 账号查这张表，多出来的是无害的。
        let balance = crate::server::core::usage_records::balance_facts();
        let token_facts = crate::server::core::limiter::token_facts();
        let limiter_blocked: HashSet<String> = accounts
            .iter()
            .filter(|account| {
                crate::server::core::usage_records::balance_blocked(account, &balance)
                    || crate::server::core::limiter::token_skip_blocked(
                        account, &token_facts, now,
                    )
            })
            .filter_map(|account| routing::account_id(account).map(str::to_string))
            .collect();
        let (sunk, resting, open) = state.upstream().route_queue_facts(now);
        Self {
            accounts,
            counts,
            limiter_blocked,
            sunk,
            resting,
            open,
            strategy: config::routing_settings().strategy,
            now,
        }
    }

    /// 某个模型的选路计划；`None` = 不限模型（全部账号）。
    ///
    /// 候选收窄与转发用同一个函数（`router::route_for_forward`）：界面看到的
    /// 「谁能接这个模型」必须就是转发时真的会去试的那些家。
    fn plan_for(&self, model: Option<&str>) -> routing::RoutePlan {
        let providers: Vec<String> = model.map(router::route_for_forward).unwrap_or_default();
        let scoped: Vec<Value> = match model {
            // 点名了模型但没有任何家认它 → 不收窄（与队列面板的既有口径一致：
            // 那种情况下「空列表」没有信息量，不如把全部账号按禁用/冷却讲一遍）
            Some(_) if providers.is_empty() => self.accounts.clone(),
            Some(_) => self
                .accounts
                .iter()
                .filter(|account| providers.contains(&routing::provider_of(account).to_string()))
                .cloned()
                .collect(),
            None => self.accounts.clone(),
        };
        let keys = routing::CooldownKeys::new(model.unwrap_or(""));
        // 预演不读也不写任何会推进状态的东西：粘性命中、轮转游标都传 None
        // （理由同队列面板：看一眼不该消耗一次轮转）。
        let meta = routing::PlanMeta {
            strategy: self.strategy,
            sunk: &self.sunk,
            resting: &self.resting,
            open: &self.open,
            limiter_blocked: &self.limiter_blocked,
            cursor: None,
            provider_cursors: None,
            sticky_account: None,
        };
        routing::plan_route(&scoped, &keys, &self.counts, &[], self.now, &meta)
    }
}

/// POST /api/routing/availability —— body `{models: ["a", "b", …]}`
///
/// 逐模型回答「**此刻**有几个账号真能接它」。数字来自 `routing::plan_route`
/// 的 `ordered`：通过全部硬门禁（启用 / 未被限制器拦 / 不在冷却 / 并发未满 /
/// 有凭证）的账号数 —— 也就是一次真实请求会依次尝试的候选个数。
///
/// ── 为什么要有这个端点（而不是客户端按家估）─────────────────
/// 客户端手里只有「哪家有哪些模型」和「每家几个就绪账号」两件事，乘一乘只能
/// 得到近似值：同名模型跨多家时要么算少（只取第一家），要么把「这家有 3 个
/// 账号」当成「这个模型有 3 个账号」（其实那 3 个账号未必都能接这个模型）。
/// 真实答案只有选路层知道，所以由它算。
///
/// ── 为什么必须收模型列表 ────────────────────────────────────
/// 每个模型一次 `plan_route`（会 clone 一遍候选账号）。全部模型全算一遍在
/// 几十上百个模型时是明显浪费，而界面一次最多也只铺 40 个胶囊。所以由调用方
/// 点名，且设上限 [AVAILABILITY_MAX_MODELS]。
pub async fn post_routing_availability(
    State(state): State<ServerState>,
    body: Bytes,
) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(raw) = payload.get("models").and_then(Value::as_array) else {
        return errors::management_error(400, "缺少 models 字段（模型名数组）");
    };
    // 去重保序：客户端可能把同一个模型既按裸名又问一次带前缀的形态
    let mut models: Vec<String> = Vec::new();
    for item in raw {
        let Some(name) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        if !models.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            models.push(name.to_string());
        }
    }
    if models.is_empty() {
        return errors::management_error(400, "models 不能为空");
    }
    if models.len() > AVAILABILITY_MAX_MODELS {
        return errors::management_error(
            400,
            format!("一次最多问 {AVAILABILITY_MAX_MODELS} 个模型（收到 {}）", models.len()),
        );
    }
    let facts = PlanFacts::collect(&state);
    let items: Vec<Value> = models
        .iter()
        .map(|model| {
            let plan = facts.plan_for(Some(model));
            json!({
                "model": model,
                // `ordered` 就是「会依次尝试的候选」。第一条是此刻真会被用的那个。
                "usable": plan.ordered.len(),
                "blocked": plan.blocked.len(),
                "picked": plan.ordered.first().map(|entry| entry.id.clone()).unwrap_or_default(),
            })
        })
        .collect();
    ok_json(json!({
        "strategy": facts.strategy.as_str(),
        "computedAt": facts.now,
        "items": items,
    }))
}

/// 一次最多问几个模型的可用性。界面一次铺 40 个胶囊，留一点余量。
const AVAILABILITY_MAX_MODELS: usize = 60;

/// GET /api/routing/queue?model=
pub async fn get_routing_queue(
    State(state): State<ServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let model = params
        .get("model")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let snapshot = config::current();
    let facts = PlanFacts::collect(&state);
    let plan = facts.plan_for(model.as_deref());
    let settings = config::routing_settings();
    let entry_json = |entry: &routing::PlanEntry| {
        json!({
            "id": entry.id,
            "name": entry.name,
            "provider": entry.provider,
            "providerLabel": crate::server::core::providers::kind_from_id(&entry.provider)
                .map(|kind| crate::server::core::providers::meta(kind).label.to_string())
                .unwrap_or_else(|| entry.provider.clone()),
            "state": entry.state,
            "reason": entry.reason,
            "resetAt": if entry.reset_at > 0 { Value::from(entry.reset_at) } else { Value::Null },
            "priority": entry.account.get("priority").cloned().unwrap_or(Value::Null),
        })
    };
    ok_json(json!({
        "strategy": settings.strategy.as_str(),
        "sticky": settings.sticky.as_str(),
        "model": model.clone().map(Value::String).unwrap_or(Value::Null),
        "lastRequestModel": snapshot.last_request_model(),
        "pickedId": plan.ordered.first().map(|entry| entry.id.clone()).unwrap_or_default(),
        "ordered": plan.ordered.iter().map(entry_json).collect::<Vec<_>>(),
        "blocked": plan.blocked.iter().map(entry_json).collect::<Vec<_>>(),
    }))
}
