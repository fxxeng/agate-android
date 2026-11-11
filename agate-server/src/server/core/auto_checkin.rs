//! 定时签到 —— 按定时条目（`core::checkin_schedules`）在各自时刻执行一次签到。
//!
//! 签到执行体与 /api/accounts/checkin 是同一段（`core::billing::checkin::run_checkin`），
//! 因此限额跳过、国际版排除、串行防风的规则完全一致，不存在两套行为。
//! 本模块只负责**什么时候跑、跑哪些**：总开关 + 每条条目自己的时刻与范围，
//! 条目的存储与 CRUD 在 `core::checkin_schedules`。
//!
//! ── 触发方式：轮询 + 当天去重（不是单个定时器）────────────────
//! Node 版用 `setInterval(tick, 30s)`：单个 setTimeout 在系统休眠、锁屏、时钟被改
//! 之后会漂移甚至整段错过，而每 30 秒比一次「当前是否已过今天的触发点」，
//! 唤醒后自然补上。Rust 侧用等价的 `sleep(30s)` 循环，每个 tick 都从**墙上时钟**
//! （北京时间，见 `core::beijing`）重新判定，因此与 Node 一样具备自愈能力：
//! libuv 与 tokio 的定时器都基于单调时钟，机器休眠期间都不推进，
//! 醒来的第一次 tick 会把错过的时点补上 —— 两者在这点上是同一套语义。
//! 多条目之后每个 tick **逐条**判定（各自到点 + 各自的当天去重），同一时刻
//! 到点的多条靠 fire 的全局「同时只跑一次」守卫串行：抢不到的那条下个 tick
//! 再试（整天 due 的自愈语义保证不丢）。fire 任务名携带条目 id（R47 T1.1），
//! supervisor 的同名去重是**条目级**防重入；旧实现的全局固定名会让列表里
//! 排在整天 due 条目（如 qoder-intl 待重试）之后的计划全天抢不到执行位。
//!
//! ── 时区口径：固定 UTC+8，不跟机器时区（issue #138）────────────
//! 「今天是否已触发」与触发时刻都按**北京时间**算（`core::beijing`）：
//! 签到的自然日是上游的自然日（北京时间零点重置），网关跑在 NAS / Docker /
//! 海外 VPS 上时跟机器时区走会让定时整体漂移 —— 美西机器上 00:01 的签到
//! 实际发生在北京时间 15:01，当天额度可能已被更早的调用用掉，面板的
//! 「今日已签到」也按错误的日期显示。移植自 Node 版（作者在国内，`Local`
//! 恰好等于 UTC+8）时没有显式钉住这个前提，这里补上。
//!
//! ── 补签（有意的行为，别「顺手优化」）────────────────────────
//! Node 版 `start()` 的条件是**「今天还没签过」**，并不判「时间点是否已过」：
//! 开着自动签到却在 00:01 时没开机、白天才启动 → 立刻补一次。
//! 签到的每日额度按自然日重置，用户开着开关却整天没签到才是反直觉的结果；
//! 上游签到接口幂等，重复调用只返回「已领取」。实测（2026-09-17）在 18:44
//! 把 enabled 设为 true、time 设为 23:00（未来时点）时，Node 版立即执行了
//! 一次 reason='启动补签' 的签到 —— 所以这里的条件同样只看日期键。
//! 多条目之后按条目各补各的：今天没跑过的**启用**条目每条补一次。
//!
//! ── 配置读写 ────────────────────────────────────────────────
//! 状态存在 config.json 的 `autoCheckin` 字段：`enabled` 是总开关，
//! 条目数组在 `schedules`（读写见 `core::checkin_schedules`）。Node 每次
//! getState/tick 都重读 config.json；Rust 侧读 `config::current()` 的内存快照
//! （config.rs 已经全量保留未知字段，写回也走它），于是「别人写进 config.json
//! 的字段不会被吃掉」这条契约继续成立。差异：Node 会看到用户手工改盘上的
//! config.json 后的新值，Rust 的内存快照只在启动与写入时刷新 —— 手工改文件
//! 需要重启（与 config.rs 的既有设计一致）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, TimeZone};
use serde_json::{json, Map, Value};

use crate::server::config;
use crate::server::core::account_store::AccountStore;
use crate::server::core::beijing;
use crate::server::core::billing::checkin;
use crate::server::core::billing::BillingService;
use crate::server::core::checkin_schedules::{self, Schedule};
use crate::server::logging;
use crate::task_supervisor::{core_task_supervisor, TaskScope};

/// 轮询间隔：30 秒足够精确到分钟，又不会让计时器显得忙（Node 版 TICK_MS）
pub const TICK_MS: u64 = 30_000;

/// 默认触发时刻：零点一分（Node 版 DEFAULT_TIME）
pub const DEFAULT_TIME: &str = "00:01";

/// 缺省触发时刻（`DEFAULT_TIME` 的 owned 形态，读侧兜底用）
pub fn default_time() -> String {
    DEFAULT_TIME.to_string()
}

/// 可勾选的签到提供商（界面上的复选框）。默认全选。
///
///   - **WorkBuddy 国内版**：腾讯的每日签到接口；
///   - **WorkBuddy 国际版**：每日活跃任务（活动探测 + 条件领取 + 免费模型保活，
///     `billing::activity` 的 `workbuddy_daily_activity`）。国际版没有国内版的
///     普通签到接口，但上游客户端把这条活跃链接在同一个每日调度上 —— 保活的
///     成功不落 `checkinAt`、单独统计在 `active`；免费模型链可在签到中心自定义
///     （`billing::keepalive`）。
///   - **小浣熊**：「桌面登录积分」链路（`providers::raccoon` 的每日积分发放）；
///     首次登录奖励（电脑端 / 手机端各一条端点）是一次性新手福利，不在这条
///     每日链路里（见 `providers::raccoon::onboarding`，挂新手任务分组）；
///   - **AutoClaw 国内版 / 国际版**：通用任务接口的 `daily_signin` 任务
///     （`providers::autoclaw::checkin`）。两个地区**都支持** —— 任务接口在
///     两地是同一套路径、同一套任务 id，只是站点不同（已实测），因此两家
///     都列进来；地区由 `billing::checkin` 从账号的 provider 反查。
///   - **Qoder 中国版**：活动（campaign）领取链路（`providers::qoder::checkin`）。
///     Free 套餐账号也可能没有被下发活动（实测如此），那种情况实现返回一条
///     中性结果（「当前没有可领取的签到活动」），不算失败。
///   - **Qoder 国际版**（2026-10，issue #140）：同一条 campaign 链路，但上游
///     **只对带设备风控头的请求**下发「每天领 100 Credits」—— 风控身份由本机
///     Qoder 客户端 / qodercli 的 UMID 组件生成（`providers::qoder::risk`），
///     Linux/Docker 可一键安装组件。缺组件时得到的是明确的说明而不是报错
///     （也不落当日台账，第二天组件就位后照常领）。
///   - **Trae**：SOLO 的 `checkin_credits` 领取（`providers::trae::checkin`）。
///     加它不是因为"别家有"，而是这条链**有钱**：SOLO 转积分制后，模型调用花的
///     正是签到钱包那份积分，不签就是每天白丢一笔额度。它的风险不在"该不该签"
///     而在"怎么签"：设备号每轮全新（复用是风控可疑项）、请求体恒空对象、
///     claim 的 `code:0` 要用同设备号回查确认（幂等假成功）。三条都在那个
///     模块里钉着。⚠️ 本清单同时是**缺省全选**的来源：老配置的 `autoCheckin`
///     里没有 `providers` 字段时，读出来就是"包括 trae"——上线后第二天零点
///     就会多发一轮签到请求。不想要就在签到中心「自动签到」里取消勾选。
///   - **Loomy**：**没有独立签到接口** —— 它的「每日赠送积分」由每日首次登录
///     触发刷新（`POST /api/v1/points/first-login`，见 `providers::loomy::checkin`）。
///     自动签到对它就是每天替账号打一次这个接口，长期不登录的账号也能把
///     赠送积分续上。
///   - **KukuAI**：「免费领积分」活动的每日任务（每日登录 / 完成一次对话，
///     `freepoint/taskComplete`，见 `providers::kuku::checkin`）。接口幂等，
///     重复领取只返回 reward_point=0；业务会话由换发的 genflowpro STOKEN
///     保障（`kuku::engine`）。
///   - **OfficeAce 果办**：**一次调用**领当天全部奖励（每日签到 + 一次性新人礼
///     是同一条上游动作，`POST /v1/subscription/bonus/claim`，见
///     `providers::officeace::checkin`）。幂等由上游保证：一天一个活动只能领一次，
///     重复领不报错、返回同一份列表 —— 所以它也没有「已领过」分支。
///   - **CodeArts**：当日**ops 福利**（`/v1/ops` 的 delivery → claim → confirm，
///     见 `providers::codearts::welfare`）。它与别家的形态差别最大（三个接口、
///     两套状态、一个幂等键），所以 2026-10 之前一直**不进**这条链、只在签到
///     中心留一颗手动「领福利」按钮；按用户要求纳入本链后，它就是那一家账号的
///     「签到」：列进每日签到清单、被批量与定时执行，收尾（落 `checkinAt`、
///     计 succeeded、记签到记录台账）与别家完全一致。
///     ⚠️ 两点与别家不同，改这块前先读 `welfare` 的模块头：
///     ① 它是**写请求有限流闸**的（每天 ≤6 次写、间隔 ≥10 分钟）—— 自动路径
///     受闸（`checkin_for` 传 manual=false），用户在面板上点不受闸；
///     ② 「暂无可领取活动 / 等待重试」是**中性未领取**（不落当日台账，定时链
///     稍后重试），不是失败 —— `claim_pending` 认这两个文案。
///
/// 这是「有签到或每日活跃任务」的清单，不是「有积分概念」的清单：CatPaw 有积分
/// 查询但没有签到，因此不在此列 —— 它的账号在批量签到里被算作 `skipped`。
/// 加一家之前先确认它的签到链路真的存在（一个点了必然报错的复选框比没有更糟）。
pub const CHECKIN_PROVIDERS: [&str; 12] = [
    "workbuddy",
    "workbuddy-intl",
    "raccoon",
    "autoclaw",
    "autoclaw-intl",
    "qoder",
    "qoder-intl",
    "trae",
    "loomy",
    "kuku",
    "officeace",
    // 追加在**末尾**：这个数组的顺序就是界面复选框与签到中心分组行的顺序，
    // 插在中间会让所有存量用户的清单顺序变一下（没有功能影响，但没必要动）。
    "codearts",
];

/// 缺省的签到提供商集合（全选）
pub fn default_providers() -> Vec<String> {
    CHECKIN_PROVIDERS.iter().map(|id| id.to_string()).collect()
}

/// 签到范围清单的**增量版本**：每给 [`CHECKIN_PROVIDERS`] 追加新家就 +1，
/// 并把新家连同它的版本登记进 [`PROVIDERS_ADDED_IN`]。
///
/// ── 为什么需要一个版本号（而不是读的时候无条件补齐）───────────
/// 「签到范围」是**每条定时条目自己勾的清单**（`checkin_schedules`）。新加进来的
/// 家不可能出现在存量条目的清单里，而用户的期望是**默认就在范围内** ——
/// 2026-10 用户明确要求 OfficeAce 果办与 CodeArts 默认开启。可是「每次读都补齐」
/// 会让这一家的勾选框再也取消不掉（读完就补回来，界面上表现为「取消勾选没反应」）。
/// 所以补齐只做一次，靠配置里的版本号记账；版本一旦写上去，用户之后再取消勾选
/// 就真的生效了。执行点见 `checkin_schedules::extend_providers_once`。
pub const PROVIDERS_VERSION: i64 = 2;

/// 「哪一版加入的家」——条目清单的版本低于它 = 这家还没被提供过 ⇒ 补进去。
/// 版本 1 = 多条目改造上线时的形态（9 家；`qoder-intl` 由 `normalize_providers`
/// 的存量兼容规则跟着 `qoder` 带上，不在这里重复登记）。
const PROVIDERS_ADDED_IN: [(&str, i64); 2] = [("officeace", 2), ("codearts", 2)];

/// 版本 `version` 之后才加入清单的家（按注册顺序）。空数组 = 这一版没有新家，
/// 或配置比代码还新（装过更新的版本又退回来）——两种情况都只记账、不动清单。
pub fn providers_added_after(version: i64) -> Vec<String> {
    CHECKIN_PROVIDERS
        .iter()
        .filter(|id| {
            PROVIDERS_ADDED_IN
                .iter()
                .any(|(added, at)| added == *id && *at > version)
        })
        .map(|id| id.to_string())
        .collect()
}

/// 提供商的展示名（从注册表查，查不到就原样回显 id）。
///
/// 这是**签到语境**的展示名（签到中心的分组、自动签到的提供商勾选共用这一份）。
/// WorkBuddy 两个地区在注册表里就叫「WorkBuddy 国内版 / 国际版」（拆家后的
/// 既定口径），签到语境沿用注册名即可 —— 国际版执行的是活跃任务而不是普通
/// 签到，这件事由签到中心 `PROVIDER_DESC` 的链路说明与它的分组描述讲清楚，
/// 不在标签里堆字。Qoder 拆家（2026-10）后注册名就是「Qoder 中国版 /
/// 国际版」，不再需要这里的覆盖。其余几家不需要后缀：小浣熊没有版本区分，
/// AutoClaw 两地的签到链路都存在且同形 —— 展示名已带「国内版 / 国际版」。
pub fn provider_label(id: &str) -> &str {
    crate::server::core::providers::PROVIDERS
        .iter()
        .find(|meta| meta.id == id)
        .map(|meta| meta.label)
        .unwrap_or(id)
}

/// 归一化配置里的提供商清单：只认 CHECKIN_PROVIDERS 里的 id（去重、保持顺序），
/// 缺失 / 空数组 / 全是非法值都回落到「全选」—— 旧配置文件里没有这个字段，
/// 读出来必须是合法的默认行为。
///
/// 存量兼容：Qoder 拆家（2026-10）前的老清单只有 9 项、没有 `qoder-intl`，
/// 而显式清单不会被「缺啥补啥」—— 不补的话，拆家前勾了 Qoder 的用户拆家后
/// 自动签到会静默漏掉国际版账号。这里按账号迁移的同一口径补齐：勾了
/// `qoder` 的，紧跟其后补上 `qoder-intl`（要签中国版就要签国际版，拆家只是
/// 同一套能力的地区分身）；两者都没勾的（明确不签 Qoder）不动。
pub fn normalize_providers(value: Option<&Value>) -> Vec<String> {
    let Some(list) = value.and_then(Value::as_array) else {
        return default_providers();
    };
    let picked: Vec<String> = CHECKIN_PROVIDERS
        .iter()
        .filter(|id| list.iter().any(|item| item.as_str() == Some(*id)))
        .map(|id| id.to_string())
        .collect();
    let mut picked = if picked.is_empty() {
        default_providers()
    } else {
        picked
    };
    if let Some(pos) = picked.iter().position(|id| id == "qoder") {
        if !picked.contains(&"qoder-intl".to_string()) {
            picked.insert(pos + 1, "qoder-intl".to_string());
        }
    }
    picked
}

/// config.json 里承载定时签到状态的键。
/// `pub(crate)`：多条目配置（`core::checkin_schedules`）在同一个键下读写。
pub(crate) const CONFIG_KEY: &str = "autoCheckin";

/// 配置类错误（对应 Node 版 AutoCheckinConfigError）。
///
/// **注意 statusCode 是 400 而不是 409**：Node 版 `AutoCheckinConfigError`
/// 构造里写死 `statusCode = 400`，于是 server.mjs 的 `throw new
/// AutoCheckinConfigError('签到正在执行中，请稍候')`（run 路由）经 errorPayload
/// 得到的是 **400 + OpenAI 风格 body**，而不是 409。实测（2026-09-17）用非法
/// time 打 POST /api/auto-checkin 拿到的正是
/// `400 {"error":{"message":"时间超出范围（00:00 - 23:59）","type":"invalid_request_error"}}`。
/// 契约以 Node 版为准，这里保持一致（`CHECKIN_BUSY_STATUS` 单独抽出，
/// 万一确认要用 409，改一处即可）。
#[derive(Clone, Debug)]
pub struct AutoCheckinConfigError {
    pub message: String,
    pub status_code: i32,
}

/// run 路由「正在执行中」的状态码：与 Node 的 AutoCheckinConfigError 一致为 400
pub const CHECKIN_BUSY_STATUS: i32 = 400;

impl AutoCheckinConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status_code: 400,
        }
    }
}

impl std::fmt::Display for AutoCheckinConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

// ─── 时间工具（对照 Node 的同名函数）────────────────────────

/// JS `String(value)` 的近似：只用于 normalizeTime 的输入转换。
/// `value ?? ''` 在 Node 里把 null/undefined 变成空串，其余走 String()：
/// 数字/布尔取字面量，对象/数组在 JS 里是 "[object Object]" / 逗号拼接 ——
/// 这些形态只可能来自手改的请求体或 config.json，最终都会被正则拒掉。
fn js_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Object(_)) => "[object Object]".to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

/// 校验并归一化 HH:MM，返回补零后的文本（对应 Node 版 normalizeTime）。
///
/// 正则 `^(\d{1,2}):(\d{1,2})$` 手写等价实现：冒号两侧各 1-2 位 ASCII 数字，
/// 且**整串**必须如此（`\d` 在 JS 里不含全角数字、不允许前后空白 ——
/// 前后空白在 Node 里由 `String(value).trim()` 提前去掉）。
pub fn normalize_time(value: Option<&Value>) -> Result<String, AutoCheckinConfigError> {
    let text = js_text(value);
    let text = text.trim();
    let parsed = text
        .split_once(':')
        .filter(|(head, tail)| {
            let digits = |part: &str| {
                !part.is_empty()
                    && part.len() <= 2
                    && part.bytes().all(|byte| byte.is_ascii_digit())
            };
            digits(head) && digits(tail) && !tail.contains(':')
        })
        .and_then(|(head, tail)| Some((head.parse::<u32>().ok()?, tail.parse::<u32>().ok()?)));
    let Some((hour, minute)) = parsed else {
        return Err(AutoCheckinConfigError::new(
            "时间格式应为 HH:MM（例如 00:01）",
        ));
    };
    if hour > 23 || minute > 59 {
        return Err(AutoCheckinConfigError::new("时间超出范围（00:00 - 23:59）"));
    }
    Ok(format!("{hour:02}:{minute:02}"))
}

/// 北京日期键 YYYY-MM-DD：用于「今天是否已触发」的判定。
///
/// 为什么不能用 UTC 算：UTC+8 的凌晨 00:01 在 UTC 下还是前一天，会让
/// 「今天签过没有」的判重错一整天（移植来源 Node 版用本地时区，作者在
/// 国内恰好等于 UTC+8 —— 这里显式钉住北京时间，见 `core::beijing`）。
pub fn date_key(now: DateTime<FixedOffset>) -> String {
    beijing::date_key(now)
}

/// 北京时间的今天（`YYYY-MM-DD`）——「今天是否已触发」判定的统一入口。
pub fn today_key() -> String {
    beijing::today_key()
}

/// 今天的触发时刻（北京时间）；已过则返回 Some，未到返回 None。
/// 对应 Node 版 dueNow：非法的 time 文本在这里同样表现为 None（Node 是抛异常，
/// 但调用方一律 catch 后 return，结果一致）。
fn due_now(time_text: &str, now: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
    let target = today_at(time_text, now)?;
    if now >= target {
        Some(target)
    } else {
        None
    }
}

/// 「下次执行」的绝对时刻（毫秒时间戳），给界面展示用。
///
/// 对应 Node 版 msUntilNext 的语义（目标时刻已过顺延到明天）外加**已触发**
/// 分支：启动补签会在条目时刻之前就把今天落账（补签只看日期不看时点），
/// 这时「下次执行」必须是明天 —— 只按时刻算会得到今天的未来时刻，
/// 界面显示一个其实不会跑的点。time 非法 → None（调度起不来的情形
/// 不该编一个时间出来）。
fn next_run_at_ms(time_text: &str, fired_today: bool, now: DateTime<FixedOffset>) -> Option<i64> {
    let mut target = today_at(time_text, now)?;
    if fired_today || target <= now {
        target += chrono::Duration::days(1);
    }
    Some(target.timestamp_millis())
}

/// 今天的 HH:MM 时刻（北京时间）；time 文本非法时 None。
///
/// Node 用 `target.setHours(hour, minute, 0, 0)`（就地改到当天）；
/// chrono 没有 set_* 的就地 API，用 `with_ymd_and_hms` 重建。
/// 固定偏移没有夏令时问题（构造恒为 `Single`），因此不需要 Node 那种
/// 「DST 当天该时刻不存在」的兜底 —— `single()` 取不到就当 None，
/// 也就是这台机器上的时钟环境异常到连北京时间的固定偏移都构造不出来。
fn today_at(time_text: &str, now: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
    let normalized = normalize_time(Some(&Value::String(time_text.to_string()))).ok()?;
    let (hour, minute) = normalized.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    let naive = now.date_naive().and_hms_opt(hour, minute, 0)?;
    beijing::offset().from_local_datetime(&naive).single()
}

// ─── 状态读写 ───────────────────────────────────────────────

/// `autoCheckin` 字段的原始对象（非对象/缺失都给空对象）。
/// `pub(crate)`：多条目配置（`core::checkin_schedules`）在同一个键下读写，
/// 合并写回的逻辑只有这一份。
pub(crate) fn raw_object() -> Map<String, Value> {
    match config::current().raw().get(CONFIG_KEY) {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

/// 总开关（`autoCheckin.enabled`）：关了所有条目都不跑。
/// 条目自己的开关在 `checkin_schedules::Schedule::enabled`，两层都开着才跑。
pub fn master_enabled() -> bool {
    raw_object().get("enabled").and_then(Value::as_bool) == Some(true)
}

/// 合并写回 `autoCheckin`（对应 Node 版 writeState 的 `{...current, ...patch}`）。
/// 走 config.rs 的 `update_raw_field`，config.json 里的其它字段原样保留。
/// `pub(crate)`：多条目配置在同一键下写回，合并逻辑只有这一份。
pub(crate) fn write_state(patch: Value) {
    let _ = write_state_checked(patch);
}

/// 与 write_state 同样合并配置，但把持久化失败显式交给恢复投影处理。
pub(crate) fn write_state_checked(patch: Value) -> bool {
    let mut merged = raw_object();
    if let Value::Object(patch) = patch {
        for (key, value) in patch {
            merged.insert(key, value);
        }
    }
    config::update_raw_field(CONFIG_KEY, Value::Object(merged))
}

/// 从 SQLite 批次摘要恢复顶层 AutoCheckin 兼容视图。
pub(crate) fn project_result(summary: &Value) -> Result<(), String> {
    if !summary.is_object() {
        return Err("自动签到摘要不是 JSON 对象".to_string());
    }
    if write_state_checked(json!({ "lastResult": summary })) {
        Ok(())
    } else {
        Err("自动签到 lastResult 写入失败；执行台账保留待恢复标记".to_string())
    }
}

// ─── 服务句柄 ───────────────────────────────────────────────

/// 句柄内部状态：下载/签到这类长任务只在这里留「是否在跑」与调度任务句柄。
/// 每个账号的结果及投影提交后才推进；JNI watchdog 与 Kotlin WakeLock
/// 共用这一份单调 revision，不把总时长误判成无进展。
#[derive(Clone)]
pub struct CheckinProgress(Arc<std::sync::Mutex<(u64, Option<std::time::Instant>)>>);

impl Default for CheckinProgress {
    fn default() -> Self {
        Self(Arc::new(std::sync::Mutex::new((0, None))))
    }
}

impl CheckinProgress {
    pub(crate) fn advance(&self) {
        let mut state = match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.0 = state.0.saturating_add(1);
        state.1 = Some(std::time::Instant::now());
    }

    pub(crate) fn begin_round(&self) {
        let mut state = match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.0 = state.0.saturating_add(1);
        state.1 = Some(std::time::Instant::now());
    }

    pub fn revision(&self) -> u64 {
        match self.0.lock() {
            Ok(state) => state.0,
            Err(poisoned) => poisoned.into_inner().0,
        }
    }

    pub fn idle_elapsed(&self) -> std::time::Duration {
        let last = match self.0.lock() {
            Ok(state) => state.1,
            Err(poisoned) => poisoned.into_inner().1,
        };
        // 新轮次必须先 begin_round；旧轮次残留不能使新等待永续。
        last.map_or(std::time::Duration::MAX, |at| at.elapsed())
    }
}

struct Inner {
    /// 是否已有一次签到在执行（对应 Node 的闭包变量 running）
    running: bool,
    /// 调度循环任务句柄（对应 Node 的 timer）
    task: Option<tokio::task::JoinHandle<()>>,
    /// 调度循环停机标志：stop() 置位后循环退出（与当前签到取消分离）。
    stopped: Arc<AtomicBool>,
    /// 调度代次（R49/T2）：stop() 与每次 start() 都推进。启动补签任务捕获
    /// 启动时的代次，每步比对；失配即停止启动后续计划。
    generation: u64,
    /// 当前签到轮次的合作式停止请求；只在账号边界检查，不中断在途 Provider。
    cancel_current: Arc<AtomicBool>,
    progress: CheckinProgress,
}

/// 定时签到服务句柄：内部一把锁 + Clone，与 update / models 同构。
///
/// 锁只用来读写 running 与任务句柄这类微秒级操作；签到执行全程在锁外
/// （硬约束：持锁不做网络请求、不 await）。`running` 用 CAS 抢锁的方式表达
/// 「同时只跑一次」，与 Node 单线程下 `if (running) return` 的语义一致。
#[derive(Clone)]
pub struct AutoCheckin {
    store: AccountStore,
    billing: BillingService,
    inner: Arc<Mutex<Inner>>,
}

/// 抢到「执行中」标记的守卫：无论正常返回还是中途被取消（任务 abort），
/// Drop 时都会把 running 复位 —— 对应 Node 的 `finally { running = false }`，
/// 同时避免 abort 把 running 永久卡在 true。
struct RunningGuard {
    inner: Arc<Mutex<Inner>>,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        match self.inner.lock() {
            Ok(mut guard) => guard.running = false,
            Err(poisoned) => poisoned.into_inner().running = false,
        }
    }
}

impl AutoCheckin {
    pub fn new(store: AccountStore, billing: BillingService) -> Self {
        Self {
            store,
            billing,
            inner: Arc::new(Mutex::new(Inner {
                running: false,
                task: None,
                stopped: Arc::new(AtomicBool::new(false)),
                generation: 0,
                cancel_current: Arc::new(AtomicBool::new(false)),
                progress: CheckinProgress::default(),
            })),
        }
    }

    /// 取锁；锁中毒（持锁 panic）不致命，接管内部数据继续用
    /// （与账号存储、脱敏器同一策略）
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 当前调度代次（R49/T2）：stop() 与每次 start() 都会推进。
    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// 某代次的补签任务是否仍然有效：总开关仍开，且代次没有被 stop()/新
    /// start() 取代。
    ///
    /// `master_enabled()` 这一半让 `POST /api/auto-checkin {enabled:false}`
    /// 这条关停路径（它不经过 stop()）也能立刻阻止后续补签计划。
    fn backfill_is_current(&self, generation: u64) -> bool {
        master_enabled() && self.generation() == generation
    }

    /// 是否正在执行签到（getState().running）
    /// 当前是否存在一轮签到执行中（包括手动、调度器与闹钟触发）。
    pub fn is_running(&self) -> bool {
        self.lock().running
    }

    pub fn progress(&self) -> CheckinProgress {
        self.lock().progress.clone()
    }

    /// 请求当前签到轮次在当前账号完成后停止。不会中断正在进行的 Provider 请求。
    /// 返回 true 表示当前确有轮次在执行；false 表示没有可请求停止的轮次。
    pub fn request_cancel_current(&self) -> bool {
        let inner = self.lock();
        if !inner.running {
            return false;
        }
        inner.cancel_current.store(true, Ordering::SeqCst);
        true
    }

    // ─── 对外状态 ───────────────────────────────────────────

    /// 对外暴露的状态（对应 Node 版 getState 的多条目版）。
    ///
    /// 形状分两层：
    ///   - `schedules`：条目视图（每条含自己的 enabled / time / providers /
    ///     lastFiredToday / nextRunAt / lastResult），签到中心的「定时签到」页直接渲染；
    ///   - 顶层 `time` / `providers` / `lastFiredDate` / `lastFiredToday` /
    ///     `lastResult` / `nextRunAt`：**兼容视图**——顶层 nextRunAt 是所有启用
    ///     条目里最近的一次（顶栏徽标用），其余字段取默认条目（合成的或显式的
    ///     id="default"；删光了就给缺省值），旧读方（启动日志）不用改。
    pub fn state(&self) -> Value {
        let master = master_enabled();
        let now = beijing::now();
        let today = date_key(now);
        let schedules = checkin_schedules::list();
        let entries: Vec<Value> = schedules
            .iter()
            .map(|schedule| {
                let fired_today = schedule.last_fired_date.as_deref() == Some(today.as_str());
                let next_run_at = if master && schedule.enabled {
                    next_run_at_ms(&schedule.time, fired_today, now)
                } else {
                    None
                };
                json!({
                    "id": schedule.id,
                    "label": schedule.label,
                    // 展示名：label 为空时退到 time，界面不必各自兜底
                    "displayLabel": checkin_schedules::display_label(schedule),
                    "enabled": schedule.enabled,
                    "time": schedule.time,
                    "providers": schedule.providers,
                    "lastFiredDate": schedule.last_fired_date,
                    "lastFiredToday": fired_today,
                    "nextRunAt": next_run_at,
                    "lastResult": schedule.last_result,
                })
            })
            .collect();
        // 聚合「下次执行」：启用条目里最近的一次（总开关关着就没了）
        let next_run_at = if master {
            entries
                .iter()
                .filter_map(|entry| entry.get("nextRunAt").and_then(Value::as_i64))
                .min()
        } else {
            None
        };
        // 兼容视图的「默认条目」：显式 default → 第一条 → （删光了）缺省
        let fallback = Schedule {
            id: checkin_schedules::DEFAULT_SCHEDULE_ID.to_string(),
            label: String::new(),
            enabled: false,
            time: default_time(),
            providers: default_providers(),
            last_fired_date: None,
            last_result: None,
        };
        let default_entry = schedules
            .iter()
            .find(|schedule| schedule.id == checkin_schedules::DEFAULT_SCHEDULE_ID)
            .or_else(|| schedules.first())
            .unwrap_or(&fallback);
        json!({
            "enabled": master,
            // 兼容字段（默认条目的投影；语义与改造前的顶层字段一致）
            "time": default_entry.time,
            "providers": default_entry.providers,
            "lastFiredDate": default_entry.last_fired_date,
            "lastFiredToday":
                default_entry.last_fired_date.as_deref() == Some(today.as_str()),
            "lastResult": default_entry.last_result,
            "nextRunAt": next_run_at,
            // 界面的复选框清单（签到范围），顺序即 CHECKIN_PROVIDERS 注册顺序
            "providerOptions": CHECKIN_PROVIDERS.iter().map(|id| {
                json!({ "id": id, "label": provider_label(id) })
            }).collect::<Vec<_>>(),
            "schedules": entries,
            "running": self.is_running(),
        })
    }

    // ─── 执行 ───────────────────────────────────────────────

    /// 执行一次签到并记录结果；失败只记日志，不影响下一次调度。
    ///
    /// 返回 None 的两种情形（Node 同）：① 已有一次在跑（本次跳过）；
    /// ② 执行抛错。路由层据此把 ① 翻成「签到正在执行中，请稍候」。
    ///
    /// 手动全量（`run_now`，无条目归属）走这里：范围是**全部提供商**——多条目
    /// 之后批量动作不再有全局「勾选范围」可借，那条范围概念进了每条定时条目。
    pub async fn fire(&self, reason: &str) -> Option<Value> {
        self.fire_for(reason, None, "manual").await
    }

    /// 执行一次签到，可归属到某条定时条目。
    ///
    /// `schedule_id` 给 Some：范围用**那条条目自己勾选的提供商**，去重标记落
    /// 条目级（`checkin_schedules::mark_fired` / `mark_result`）；给 None
    /// （手动全量）：范围是全部提供商，只落配置级标记。
    /// `source` 是签到记录台账的触发来源（schedule / manual / startup，
    /// 见 `core::checkin_records` 的模块头）。
    pub async fn fire_for(
        &self,
        reason: &str,
        schedule_id: Option<&str>,
        source: &str,
    ) -> Option<Value> {
        // 先抢「执行中」标记；抢不到说明上一次还在跑
        let guard = {
            let mut inner = self.lock();
            if inner.running {
                drop(inner);
                logging::verbose("[Checkin]", "上一次签到仍在执行，本次跳过");
                return None;
            }
            inner.running = true;
            inner.cancel_current.store(false, Ordering::SeqCst);
            RunningGuard {
                inner: self.inner.clone(),
            }
        };

        let today = today_key();
        // 先落日期再执行：即便签到中途进程被杀，也不会在重启后反复补签
        write_state(json!({ "lastFiredDate": today }));
        if let Some(id) = schedule_id {
            checkin_schedules::mark_fired(id, Some(&today));
        }
        logging::log("[Checkin]", &format!("⏰ 定时签到开始（{reason}）"));
        crate::server::events::publish(
            "checkin_started",
            json!({ "reason": reason, "source": source }),
        );

        // 范围：条目轮次用那条自己勾的提供商；手动全量（无条目）= 全部提供商。
        // 条目可能在执行前被删（罕见）：拿不到就按全量兜，不空跑。
        let providers = match schedule_id {
            Some(id) => checkin_schedules::list()
                .into_iter()
                .find(|schedule| schedule.id == id)
                .map(|schedule| schedule.providers)
                .unwrap_or_else(default_providers),
            None => default_providers(),
        };
        let (cancel_current, progress) = {
            let inner = self.lock();
            (inner.cancel_current.clone(), inner.progress.clone())
        };
        progress.begin_round();
        let outcome = match checkin::run_checkin_cancellable(
            &self.store,
            &self.billing,
            providers.as_slice(),
            None,
            reason,
            source,
            Some(cancel_current.as_ref()),
            schedule_id,
            true,
            Some(&progress),
        )
        .await
        {
            Ok(result) => {
                let summary = self.record_success(&result, &today, reason);
                if let Some(id) = schedule_id {
                    checkin_schedules::mark_result(id, Some(summary.clone()));
                }
                if let Some(db) = self.store.db() {
                    if let Err(error) =
                        crate::server::core::checkin_run_ledger::project_pending_auto_results(db)
                    {
                        logging::verbose("[Checkin]", &format!("自动签到摘要投影待恢复：{error}"));
                    }
                }
                Some(summary)
            }
            Err(error) if error.status_code == 409 => {
                // 手动 API 或另一条签到入口正在执行：这不是本轮签到失败。
                // 释放条目的当天去重标记，让下一次 tick 有机会执行；不要覆盖
                // lastResult，也不要把「并发冲突」写成账号签到失败。
                if let Some(id) = schedule_id {
                    checkin_schedules::mark_fired(id, None);
                }
                logging::verbose("[Checkin]", "已有另一条签到入口正在执行，本轮调度稍后重试");
                None
            }
            Err(error) => {
                // 失败也写 lastResult（failed 里放错误文案），与 Node 的 catch 分支一致
                let failure = json!({
                    "at": logging::now_ms(),
                    "date": today,
                    "reason": reason,
                    "succeeded": 0,
                    "active": 0,
                    "total": 0,
                    "skipped": 0,
                    "failed": [error.message.clone()],
                    "failedCount": 1,
                });
                write_state(json!({ "lastResult": failure.clone() }));
                if let Some(id) = schedule_id {
                    checkin_schedules::mark_result(id, Some(failure));
                }
                logging::log("[Checkin]", &format!("❌ 定时签到失败: {}", error.message));
                None
            }
        };
        drop(guard);
        outcome
    }

    /// 本轮是否有「Qoder 国际版账号没领到且不算已签」的行 —— 有则今天保持
    /// 未落账状态，等窗口开了重试（见 tick 的说明）。
    fn qoder_intl_pending(result: &Value) -> bool {
        result
            .get("results")
            .and_then(Value::as_array)
            .map(|items| {
                items.iter().any(|item| {
                    item.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.starts_with("qoder-global-"))
                        && crate::server::core::billing::checkin::claim_pending(
                            &item.get("claim").cloned().unwrap_or(Value::Null),
                        )
                })
            })
            .unwrap_or(false)
    }

    /// 成功分支：汇总 + 记录 lastResult + 日志（对应 Node fire 里的 try 主体）
    fn record_success(&self, result: &Value, today: &str, reason: &str) -> Value {
        let number = |key: &str| result.get(key).and_then(Value::as_u64).unwrap_or(0);
        let succeeded = number("succeeded");
        let active = number("active");
        let total = number("total");
        let skipped = number("skipped");
        // 失败明细：`名字（错误）`，名字缺失时退到 id，再退到「未知账号」
        let failures: Vec<String> = result
            .get("results")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let error = item.get("error").and_then(Value::as_str)?;
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or("未知账号");
                        Some(format!("{name}（{error}）"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let summary = json!({
            "at": logging::now_ms(),
            "date": today,
            "reason": reason,
            "succeeded": succeeded,
            "active": active,
            "total": total,
            "skipped": skipped,
            "cancelled": result.get("cancelled").and_then(Value::as_bool).unwrap_or(false),
            "remaining": number("remaining"),
            // 只留前 5 条：面板展示用，避免 config.json 被长列表撑大（Node 同）
            "failed": failures.iter().take(5).cloned().collect::<Vec<_>>(),
            "failedCount": failures.len(),
        });
        crate::server::events::publish(
            "checkin_done",
            serde_json::json!({
                "succeeded": succeeded,
                "total": total,
                "skipped": skipped,
                "failed": failures.len(),
                "active": active,
                "cancelled": result.get("cancelled").and_then(Value::as_bool).unwrap_or(false),
                "remaining": number("remaining"),
            }),
        );
        write_state(json!({ "lastResult": summary }));
        logging::log(
            "[Checkin]",
            &format!(
                "{}: {succeeded}/{total} 个账号成功领取{}{}{}",
                if result.get("cancelled").and_then(Value::as_bool) == Some(true) {
                    "签到已停止（当前账号已收尾）"
                } else {
                    "定时签到完成"
                },
                if skipped > 0 {
                    format!("，跳过 {skipped} 个")
                } else {
                    String::new()
                },
                if failures.is_empty() {
                    String::new()
                } else {
                    format!("，失败 {} 个", failures.len())
                },
                if active > 0 {
                    format!("，活跃保活 {active} 个")
                } else {
                    String::new()
                },
            ),
        );
        // qoderIntlPending 不进 lastResult（那是给界面看的快照），单独放在
        // fire 的返回值上供 tick 决定今天要不要保持未落账
        let mut summary = summary;
        if let Some(object) = summary.as_object_mut() {
            object.insert(
                "qoderIntlPending".to_string(),
                Value::Bool(Self::qoder_intl_pending(result)),
            );
        }
        summary
    }

    // ─── 调度 ───────────────────────────────────────────────

    /// 轮询回调：每条启用的条目各自「到点 + 今天没跑过」就执行（对应 Node 版
    /// tick 的多条目版）。
    ///
    /// ── Qoder 国际版的窗口补领（2026-10，issue #140）────────────
    /// 国际版「每日 100 Credits」的官方窗口**每天 10:00（UTC+8）开启**，而
    /// 缺省触发时刻是 00:01 —— 那时窗口还没开，活动列表里没有积分活动，
    /// 领取轮次对它只能得到「无活动」。若就此落条目的 `lastFiredDate` 整天
    /// 不再重试，国际版账号等于永远领不到。所以条目轮次**照常在配置时刻跑**，
    /// 但当轮次里存在「Qoder 国际版无活动未落账」的行时，**这一条**今天暂不落
    /// 到点标记：之后每个 tick 会重新进来，直到 10:00 窗口开启领到（或活动行
    /// 出现 CLAIMED）才落账 —— 与 10router「no-activity 刻意不记忆、下个
    /// tick 再看」同一语义。窗口判定按条目独立：没勾 qoder-intl 的条目
    /// 不会有这种行，到点即落账。
    pub async fn tick(&self) {
        if !master_enabled() {
            return;
        }
        let now = beijing::now();
        let today = date_key(now);
        for schedule in checkin_schedules::list() {
            // 条目自己的开关（总开关已在上面拦掉）
            if !schedule.enabled {
                continue;
            }
            // 未到点直接看下一条（time 在读取时已归一，这里不会解析失败）
            if due_now(&schedule.time, now).is_none() {
                continue;
            }
            // 条目级当天去重
            if schedule.last_fired_date.as_deref() == Some(today.as_str()) {
                continue;
            }
            let reason = format!(
                "到点触发（{}）",
                checkin_schedules::display_label(&schedule)
            );
            // fire 移出循环任务（R44b）：调度循环体内只剩 sleep/扫描，stop() 的
            // abort 因此不可能再掐掉进行中的签到。fire 落 CoreTaskSupervisor，
            // 名字携带条目 id（R47 T1.1）：同名活任务忽略 = **条目级**重入防重，
            // 不同条目互不阻挡（旧固定名曾让列表顺序靠前的整天 due 条目——
            // qoder-intl 待重试——锁死其后所有计划，确定性全天饥饿）；fire 内
            // 的全局防重守卫与条目落账先落后跑是后两层。进程停机由
            // shutdown_core_tasks 对该 scope 有界 abort+join 收尾。
            // ScopeStopped 只在停机后出现，丢触发属正常。
            let service = self.clone();
            let schedule_id = schedule.id.clone();
            let fire_name = format!("checkin-fire:{schedule_id}");
            match core_task_supervisor().spawn(TaskScope::Core, fire_name.clone(), move |_| {
                let service = service.clone();
                async move {
                    service
                        .fire_and_settle(schedule_id, reason, "schedule")
                        .await
                }
            }) {
                Ok(true) => {}
                Ok(false) => logging::verbose(
                    "[Checkin]",
                    &format!("签到条目 {fire_name} 仍在途，本 tick 跳过（防重入）"),
                ),
                Err(_) => logging::verbose("[Checkin]", "停机中，放弃本次到点触发"),
            }
        }
    }

    /// 执行一次签到并完成该条目的落账（[`Self::fire_for`] + 原 tick 的返回值
    /// 记账，两处必须同体：记账读的是 fire 的返回值）。由 supervisor 的
    /// `checkin-fire` 任务调用，见 [`Self::tick`] 注释。
    async fn fire_and_settle(&self, schedule_id: String, reason: String, source: &'static str) {
        match self
            .fire_for(&reason, Some(&schedule_id.as_str()), source)
            .await
        {
            Some(result) => {
                // 领取轮次完成，但里面有「Qoder 国际版无活动」的行 → 这一条
                // 不落账，今天窗口开了之后由后续 tick 重试
                if result.get("qoderIntlPending").and_then(Value::as_bool) == Some(true) {
                    checkin_schedules::mark_fired(&schedule_id, None);
                    logging::log(
                        "[Checkin]",
                        "⏳ Qoder 国际版的活动窗口未开（官方每日 10:00 UTC+8），今天会自动重试到领到为止",
                    );
                }
            }
            // None = 已有轮次在跑或执行出错：照旧落账（fire 内部已写），行为不变
            None => {}
        }
    }

    /// 起调度循环（对应 Node 版 schedule 的 `if (timer) return`）：
    /// 已有存活任务就不重复起，避免 configure 每次保存都叠一个循环。
    fn schedule(&self) {
        {
            let mut inner = self.lock();
            if let Some(task) = &inner.task {
                if !task.is_finished() {
                    return;
                }
            }
            inner.stopped = Arc::new(AtomicBool::new(false));
            let stopped = inner.stopped.clone();
            let service = self.clone();
            // 用 crate::spawn_task：本函数可能从 setup 钩子（主线程）或 axum handler
            // 调用，`crate::spawn_task` 兜底进全局运行时，
            // 不会像裸 `tokio::spawn` 那样在非运行时上下文 panic（release 是 panic=abort）
            inner.task = Some(crate::spawn_task(async move {
                loop {
                    // 先睡再 tick：与 setInterval 的「间隔后首次触发」一致，
                    // 也避开 start() 里那次补签刚发起的窗口
                    tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    service.tick().await;
                }
            }));
        }
    }

    /// 启动调度（对应 Node 版 start 的多条目版）。
    ///
    /// 开启状态下立即检查一次：**只看「今天签过没有」，不看时点是否已过**
    /// （理由见模块头部「补签」）。多条目之后按条目各补各的：今天没跑过的
    /// **启用**条目每条补一次，串在一个任务里跑（fire 的全局防重本来就只允许
    /// 一轮在跑，串行省得互相抢空）。
    pub fn start(&self) {
        if !master_enabled() {
            return;
        }
        self.schedule();
        // R49/T2：每次 start 开启新的调度代次。补签任务捕获启动时的代次，
        // stop() 与再一次 start() 都会让旧代次立刻失效 —— 旧任务不会继续
        // 执行新一代的计划（任务书 §3.3 的代次语义）。
        let generation = {
            let mut inner = self.lock();
            inner.generation = inner.generation.wrapping_add(1);
            inner.generation
        };
        let today = today_key();
        let pending: Vec<Schedule> = checkin_schedules::list()
            .into_iter()
            .filter(|schedule| {
                schedule.enabled && schedule.last_fired_date.as_deref() != Some(today.as_str())
            })
            .collect();
        if pending.is_empty() {
            logging::verbose("[Checkin]", "定时签到条目均已执行，无需补签");
            return;
        }
        // 补签整段注册进 supervisor（每代一个任务名），串行跑全部 pending：
        // 此前是裸 spawn_task，stop() 既管不到、停机也无人收尾，还漏了 tick 路径
        // 有的条目记账（qoderIntlPending 重试落账）——统一走 fire_and_settle。
        // 与轮询 fire 共用同一全局防重守卫。
        //
        // 任务名携带代次：旧代次任务正在收尾时，新一代仍能注册自己的补签。
        // 固定名会被 supervisor 的同名去重吃掉，「关→开」后的第一次补签
        // 就永远不会跑（任务书 §3.4 的任务身份规则）。
        let service = self.clone();
        if core_task_supervisor()
            .spawn(
                TaskScope::Core,
                format!("checkin-backfill:{generation}"),
                move |_| {
                    let service = service.clone();
                    async move {
                        let is_current = {
                            let service = service.clone();
                            move || service.backfill_is_current(generation)
                        };
                        let fire = {
                            let service = service.clone();
                            move |schedule: Schedule| {
                                let service = service.clone();
                                async move {
                                    let reason = format!(
                                        "启动补签（{}）",
                                        checkin_schedules::display_label(&schedule)
                                    );
                                    service
                                        .fire_and_settle(schedule.id.clone(), reason, "startup")
                                        .await;
                                }
                            }
                        };
                        run_backfill(pending, is_current, fire).await;
                    }
                },
            )
            .is_err()
        {
            logging::verbose("[Checkin]", "停机中，放弃启动补签");
        }
    }

    /// 停调度循环。幂等：重复调用不会报错。
    ///
    /// ⚠️ 调用方不止进程停机：JNI `schedulerSet(off)`（用户在设置页关调度器）
    /// 也走这里 —— 旧注释「进程退出时用」不成立。
    ///
    /// abort 的是**调度循环任务**：R44b 起循环体内只剩 sleep/扫描（fire 已移出，
    /// 见 [`Self::tick`]），abort 立即生效且无副作用。进行中的签到 fire 任务在
    /// supervisor 的 `checkin-fire` 名下，**不受本调用影响**，跑完自灭 ——
    /// 「关掉调度器不掐进行中的签到」自此语义与实现一致（旧实现把 fire await
    /// 在循环任务里，abort 会把签到掐在 HTTP await 点、lastFiredDate 未写）。
    /// 进程停机的最终收尾（有界 abort+join）在 `shutdown_core_tasks`。
    ///
    /// R49/T2：**无条件**推进调度代次 —— 旧实现在 `inner.task.is_none()` 时
    /// 直接 return，而启动补签是独立注册的 Core 任务，不受 `task` 是否存在
    /// 影响；那条路径会让「关闭调度器」对补签完全失效。
    pub fn stop(&self) {
        let task = {
            let mut inner = self.lock();
            inner.generation = inner.generation.wrapping_add(1);
            inner.stopped.store(true, Ordering::SeqCst);
            inner.task.take()
        };
        if let Some(task) = task {
            task.abort();
        }
    }

    // ─── 设置 ───────────────────────────────────────────────

    /// 保存设置（对应 Node 版 configure 的兼容入口）。
    ///
    /// 多条目之后字段落点分两层：
    ///   - `enabled` → **总开关**（`autoCheckin.enabled`，语义不变）；
    ///   - `time` / `providers` → 落到**默认条目**（`checkin_schedules::upsert`）：
    ///     老入口没有条目 id 概念，写的就是合成的 default 条目——第一次保存会把
    ///     条目固化进 `schedules`（去重状态一并带过去，见 upsert 的存量兼容）。
    ///
    /// R49/T2 起，关闭总开关会直接 `stop()`（推进代次并停掉轮询），不再
    /// 依赖「tick 读到开关关再早退」—— 那条路径管不到已经提交的启动补签，
    /// 用户关掉签到后剩余计划仍会继续跑。tick 的早退判定保留，作为停机
    /// 窗口与直接改 config.json 场景的兜底。
    pub fn configure(&self, payload: &Value) -> Result<Value, AutoCheckinConfigError> {
        let has_enabled = payload.get("enabled").is_some();
        let has_time = payload.get("time").is_some();
        let has_providers = payload.get("providers").is_some();
        if !has_enabled && !has_time && !has_providers {
            return Err(AutoCheckinConfigError::new("没有需要更新的字段"));
        }
        if has_enabled {
            // Node: `enabled !== undefined` → `patch.enabled = enabled === true`，
            // 也就是说 null / 字符串 / 0 都会把开关置为 false（不是「忽略」）
            let on = payload.get("enabled") == Some(&Value::Bool(true));
            write_state(json!({ "enabled": on }));
            logging::log(
                "[Checkin]",
                &format!("定时签到总开关已{}", if on { "开启" } else { "关闭" }),
            );
            if !on {
                // R49/T2：configure 是界面关闭签到的主路径（POST /api/auto-checkin），
                // 必须与 schedulerSet(false) 一样让补签立刻失效。
                self.stop();
            }
        }
        if has_time || has_providers {
            let mut entry = Map::new();
            entry.insert(
                "id".to_string(),
                Value::String(checkin_schedules::DEFAULT_SCHEDULE_ID.to_string()),
            );
            if has_time {
                entry.insert(
                    "time".to_string(),
                    Value::String(normalize_time(payload.get("time"))?),
                );
            }
            if let Some(value) = payload.get("providers") {
                // 勾选清单：只认注册过的提供商 id（去重、按注册顺序落盘）；
                // 一个都不勾没有意义 —— 定时到点也无账号可签，直接拒绝并说明。
                let Some(list) = value.as_array() else {
                    return Err(AutoCheckinConfigError::new("providers 必须是字符串数组"));
                };
                let picked: Vec<Value> = CHECKIN_PROVIDERS
                    .iter()
                    .filter(|id| list.iter().any(|item| item.as_str() == Some(*id)))
                    .map(|id| Value::String(id.to_string()))
                    .collect();
                if picked.is_empty() {
                    let names = CHECKIN_PROVIDERS
                        .iter()
                        .map(|id| provider_label(id))
                        .collect::<Vec<_>>()
                        .join("、");
                    return Err(AutoCheckinConfigError::new(format!(
                        "至少勾选一家签到提供商（{names}）"
                    )));
                }
                entry.insert("providers".to_string(), Value::Array(picked));
            }
            // 不带 enabled：条目自己的开关保持原样（这里只改时刻/范围）
            checkin_schedules::upsert(&Value::Object(entry))
                .map_err(|error| AutoCheckinConfigError::new(error))?;
        }
        // 总开关开着就走一遍 start()：确保调度循环在跑（幂等），并且刚开启 /
        // 刚改完配置的今天还没跑过的条目立刻补签（模块头「补签」的实测行为）
        if master_enabled() {
            self.start();
        }
        Ok(self.state())
    }

    /// 账号页「全部签到」的范围：**全部提供商**。
    ///
    /// 改造前这里返回的是配置勾选的清单（`autoCheckin.providers`）——那份清单
    /// 现在属于定时条目，不该再管手动批量：条目管「定时跑哪些」，手动全量
    /// 就该把所有能签的都签了（单账号的定点签到另有 /api/accounts/{id}/checkin）。
    pub fn batch_providers(&self) -> Vec<String> {
        default_providers()
    }

    /// 立即执行一次（界面上的「立即签到」按钮走这里，与定时触发同一条路径）。
    /// 正在执行中返回 None —— 路由层翻成「签到正在执行中，请稍候」。
    pub async fn run_now(&self) -> Option<Value> {
        self.fire("手动触发").await
    }

    /// 立即执行**某条定时条目**（定时签到页每条的「立即执行」按钮）。
    ///
    /// 与到点触发同一条 fire 路径（自带全局防重与条目落账），但不要求条目
    /// 或总开关开着 —— 用户点了就是明确要跑这一条。条目不存在返回 None 与
    /// 「在跑」不可区分，路由层先查存在性（见 api::checkin_schedules）。
    pub async fn run_schedule_now(&self, id: &str) -> Option<Value> {
        let schedule = checkin_schedules::list()
            .into_iter()
            .find(|entry| entry.id == id)?;
        let reason = format!(
            "手动触发（{}）",
            checkin_schedules::display_label(&schedule)
        );
        self.fire_for(&reason, Some(id), "manual").await
    }
}

// ─── 进程级句柄（供停机路径调用 stop）─────────────────────────

/// 进程级定时签到句柄。
///
/// 为什么不用 ServerState 就够了：停机入口是 `sub_kernel::shutdown(timeout)`，
/// 那里拿不到 ServerState（它是 sub_kernel 自己的停机编排）；而 Node 版退出时明确会
/// `autoCheckin.stop()`（server.mjs 1047-1049 的 closeAll）。用进程级句柄
/// （与 config / logging 同一个模式）让停机路径能直接拿到它。
/// ServerState 里那份与全局这份是**同一实例**（bootstrap 时装入）。
static GLOBAL: OnceLock<AutoCheckin> = OnceLock::new();

/// 初始化进程级句柄（启动时调用一次，幂等）
pub fn init_global(service: AutoCheckin) -> AutoCheckin {
    let _ = GLOBAL.set(service.clone());
    service
}

/// 停掉进程级调度；未初始化时什么都不做（路由以外的调用点用）
pub fn stop_global() {
    if let Some(service) = GLOBAL.get() {
        service.stop();
    }
}

/// 补签循环骨架（R49/T2）。
///
/// 把「依次执行全部待补签计划」与「取消语义」拆开：`is_current` 在**每条计划
/// 启动前**和**每条计划收尾后**各查一次，失配即停止启动后续计划；已经启动的
/// 那一条允许自然收尾，不中断在途 Provider 请求。
///
/// 之所以抽成自由函数：竞态矩阵（stop 落在首条之前/执行中/之后、换代重开）
/// 需要可控的 fire 回调，才能确定性地测，不靠 sleep 猜时序，也不打真实网络。
async fn run_backfill<F, Fut>(
    pending: Vec<Schedule>,
    mut is_current: impl FnMut() -> bool,
    mut fire: F,
) where
    F: FnMut(Schedule) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for schedule in pending {
        if !is_current() {
            logging::verbose("[Checkin]", "调度已关闭或已换代，停止启动后续补签计划");
            return;
        }
        fire(schedule).await;
        if !is_current() {
            logging::verbose("[Checkin]", "调度已关闭或已换代，停止启动后续补签计划");
            return;
        }
    }
}

#[cfg(test)]
mod scheduling_tests {
    //! T1.1（R47）tick 条目级调度的端到端验收。
    //!
    //! ── 全局态与隔离手法 ────────────────────────────────────
    //! 本模块触到的全局态：config 快照与统一库（`autoCheckin` 键）、config_dir
    //! 落盘位置、签到闸（`billing::checkin` 全局闸）、`CoreTaskSupervisor` 单例。
    //! 前三者用本模块一把锁串行（与 `config::auto_continue_tests` 的 CONFIG_LOCK
    //! 同一手法）；跨模块对整份 raw 的读改写并发窗口极小且读后写会保留他键，
    //! 与既有套件同一容忍度。supervisor 的已完成条目跨测试累积无害——同名任务
    //! 完成后可再注册，正是防重入语义本身。
    //!
    //! `config_dir` 经 `set_config_dir_override` 钉到进程临时目录（与 Android
    //! JNI 注入同一机制，OnceLock 同值重复设置幂等），绝不落真实 `~/.agate`。
    //! config 持久化走统一库（`update → save_raw`，无库时写回报 false），所以
    //! 还要给 config 塞一个临时库（`config::init`，OnceLock 先到先得）；守卫
    //! 存进静态量，库文件存活到进程结束。
    //!
    //! 账号库为空（TempDb）：`run_checkin_cancellable` 解析出零签到目标，全程
    //! 不打网络，fire 秒级完成。等待一律轮询记账标记（lastFiredDate/lastResult），
    //! 轮询的 await 点就是 fire 任务被调度推进的机会，不 sleep 猜进度。
    //! 「00:00」整点条目整天 due（`due_now` 是 now >= target），日期断言不依赖
    //! 测试运行的具体时刻。

    use super::*;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    static SCHEDULING_LOCK: Mutex<()> = Mutex::new(());
    static CONFIG_DB_GUARD: OnceLock<crate::server::db::test_temp::TempDb> = OnceLock::new();

    /// 一次性钉死 config_dir 与 config 统一库（同值重复设置幂等 / 先到先得）。
    fn isolate_state() {
        let dir = std::env::temp_dir().join(format!("agate-test-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时 config 目录应可创建");
        crate::paths::set_config_dir_override(dir).expect("config_dir 覆盖仅允许重复设置同值");
        CONFIG_DB_GUARD.get_or_init(|| {
            let (db, guard) = crate::server::db::test_temp::TempDb::open("sched-config");
            config::init(Some(db));
            guard
        });
    }

    /// 空账号库 + 真实 provider id ⇒ 零签到目标、零网络。
    fn fresh_service(label: &str) -> (AutoCheckin, crate::server::db::test_temp::TempDb) {
        let (db, guard) = crate::server::db::test_temp::TempDb::open(label);
        let store = AccountStore::with_db(Some(db));
        let billing = BillingService::new(crate::server::core::auth::AuthService::for_store(
            store.clone(),
        ));
        (AutoCheckin::new(store, billing), guard)
    }

    /// 直写 raw 键（绕过 upsert 校验）：测试要精确控制 lastFiredDate 等记账态。
    fn set_entries(entries: Value) {
        assert!(
            write_state_checked(json!({ "enabled": true, "schedules": entries })),
            "测试配置写回失败"
        );
    }

    #[test]
    fn progress_renews_only_after_account_completion() {
        let progress = super::CheckinProgress::default();
        progress.begin_round();
        let before = progress.revision();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(progress.idle_elapsed() >= std::time::Duration::from_millis(1));
        progress.advance();
        assert_eq!(before + 1, progress.revision());
        assert!(progress.idle_elapsed() < std::time::Duration::from_millis(100));
    }

    fn entry(id: &str, time: &str, fired_today: bool) -> Value {
        json!({
            "id": id,
            "label": id,
            "enabled": true,
            "time": time,
            "providers": [CHECKIN_PROVIDERS[0]],
            "lastFiredDate": if fired_today { Some(today_key()) } else { None },
        })
    }

    fn find_entry(id: &str) -> Option<crate::server::core::checkin_schedules::Schedule> {
        crate::server::core::checkin_schedules::list()
            .into_iter()
            .find(|schedule| schedule.id == id)
    }

    /// 轮询记账标记直到成立；轮询的 await 点就是 fire 任务被推进的机会。
    async fn wait_for(condition: &mut dyn FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "等待超时：{what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn cleanup_config() {
        write_state(json!({ "enabled": false, "schedules": Value::Null }));
    }

    /// 计划验收 #1/#2：同刻到期的多条目全部触发，互不丢弃。旧实现里列表顺序
    /// 靠前的条目占住全局固定名，排在后面的整天 due 条目（qoder-intl 待重试）
    /// 确定性饥饿（R47 T1.1 根因）；负对照：已触发条目不被本轮触碰。
    #[tokio::test]
    async fn same_tick_due_entries_all_fire() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-same-tick");
        set_entries(json!([
            entry("entry-a", "00:00", false),
            entry("entry-b", "00:00", false),
            entry("entry-done", "00:00", true),
        ]));

        // 同刻第二条要等下个 tick 才轮得到（fire 的全局 running CAS 串行，抢
        // 不到的那条本 tick 直接返回、留待下个 tick 重试——实施说明 §4.4 口径，
        // 测试里连续 tick 压缩节奏）。完成判定用 last_result（写于 fire 收尾）：
        // 它存在即签到闸已释放，不给相邻用例留跨用例竞态。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut both_complete = || {
            find_entry("entry-a").and_then(|s| s.last_result).is_some()
                && find_entry("entry-b").and_then(|s| s.last_result).is_some()
        };
        while !both_complete() {
            assert!(
                std::time::Instant::now() < deadline,
                "两条目都应完成 fire 记账"
            );
            service.tick().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert!(
            find_entry("entry-done")
                .and_then(|s| s.last_result)
                .is_none(),
            "已触发条目不应被本轮触碰"
        );
        cleanup_config();
    }

    /// 计划验收 #4：fire 在途/未轮询期间重复 tick 不产生第二次执行——supervisor
    /// 的条目级去重吸收重入（Ok(false) 留日志），记账只落一份且不被弄乱。
    #[tokio::test]
    async fn tick_reentry_does_not_disturb_in_flight_fire() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-reentry");
        set_entries(json!([entry("entry-r", "00:00", false)]));

        // 背靠背两个 tick：第二个 tick 时 fire 可能尚未被轮询（日期未落，触发
        // 同名防重入），也可能已完成（日期已落，跳过）——两种时序都不得重复执行。
        service.tick().await;
        service.tick().await;

        let mut fired = || {
            find_entry("entry-r")
                .and_then(|s| s.last_fired_date)
                .is_some()
        };
        wait_for(&mut fired, "条目应完成 fire 记账").await;
        assert!(
            find_entry("entry-r").and_then(|s| s.last_result).is_some(),
            "结果应已记账且只有一份"
        );
        cleanup_config();
    }

    /// 计划验收 #7（qoder-intl pending 口径）：当天标记被显式清空后条目保持
    /// due，下个 tick 必须真的再执行一次（不丢），而不是留旧账等明天。
    #[tokio::test]
    async fn pending_reset_entry_is_retried_next_tick() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-pending-retry");
        set_entries(json!([entry("entry-p", "00:00", false)]));

        service.tick().await;
        let mut first_done = || {
            find_entry("entry-p")
                .and_then(|s| s.last_fired_date)
                .is_some()
                && find_entry("entry-p").and_then(|s| s.last_result).is_some()
        };
        wait_for(&mut first_done, "首轮 fire 应完成记账").await;

        crate::server::core::checkin_schedules::mark_fired("entry-p", None);
        assert!(find_entry("entry-p")
            .and_then(|s| s.last_fired_date)
            .is_none());

        service.tick().await;
        let mut retried = || {
            find_entry("entry-p")
                .and_then(|s| s.last_fired_date)
                .is_some()
                && find_entry("entry-p").and_then(|s| s.last_result).is_some()
        };
        wait_for(&mut retried, "清账后的条目应在下个 tick 重试并完成记账").await;
        cleanup_config();
    }

    /// 计划验收 #5 的结构性部分：stop() 掐调度循环并推进代次，但**手动 tick
    /// 的触发/记账路径照常可用**（R44b：fire 不在循环任务名下；R49/T2 只让
    /// 补签任务按代次自检，不改 tick 语义）。条目在 start 时已触发 ⇒ start
    /// 不派补签，后台无任务搅局。
    #[tokio::test]
    async fn stop_only_cancels_dispatch_loop() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-stop");
        set_entries(json!([entry("entry-s", "00:00", true)]));
        service.start();
        service.stop();

        crate::server::core::checkin_schedules::mark_fired("entry-s", None);
        service.tick().await;
        let mut done = || {
            find_entry("entry-s")
                .and_then(|s| s.last_fired_date)
                .is_some()
                && find_entry("entry-s").and_then(|s| s.last_result).is_some()
        };
        wait_for(&mut done, "stop 后 tick 仍应正常触发并完成记账").await;
        cleanup_config();
    }

    // ─── R49/T2：启动补签的取消语义矩阵 ──────────────────────
    //
    // `run_backfill` 用可控 fire 回调驱动，不依赖真实 Provider、不靠 sleep
    // 猜进度：第一条进入 fire 后由测试置取消标志并放行，断言第二条不启动。

    fn backfill_schedule(id: &str) -> Schedule {
        Schedule {
            id: id.to_string(),
            label: id.to_string(),
            enabled: true,
            time: "00:01".to_string(),
            providers: Vec::new(),
            last_fired_date: None,
            last_result: None,
        }
    }

    /// stop 落在「第一条执行中」：第一条自然收尾，第二条不启动。
    #[tokio::test]
    async fn backfill_stops_after_in_flight_plan_when_scheduler_closes() {
        let active = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(tokio::sync::Notify::new());
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));

        let is_current = {
            let active = active.clone();
            move || active.load(Ordering::SeqCst)
        };
        let fire = {
            let gate = gate.clone();
            let fired = fired.clone();
            move |schedule: Schedule| {
                let gate = gate.clone();
                let fired = fired.clone();
                async move {
                    fired.lock().unwrap().push(schedule.id);
                    gate.notified().await;
                }
            }
        };

        let task = tokio::spawn(run_backfill(
            vec![backfill_schedule("first"), backfill_schedule("second")],
            is_current,
            fire,
        ));
        wait_for(
            &mut || fired.lock().unwrap().len() == 1,
            "第一条补签应已进入 fire",
        )
        .await;

        // 模拟用户关闭调度：代次/开关失效，然后放行第一条。
        active.store(false, Ordering::SeqCst);
        gate.notify_waiters();
        task.await.expect("补签任务不应 panic");

        assert_eq!(
            vec!["first".to_string()],
            *fired.lock().unwrap(),
            "第一条之后的计划不得启动"
        );
    }

    /// stop 发生在任务真正跑起来之前：一条都不执行。
    #[tokio::test]
    async fn backfill_starts_no_plan_when_stopped_before_task_runs() {
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));
        let fire = {
            let fired = fired.clone();
            move |schedule: Schedule| {
                let fired = fired.clone();
                async move {
                    fired.lock().unwrap().push(schedule.id);
                }
            }
        };

        run_backfill(vec![backfill_schedule("only")], || false, fire).await;

        assert!(fired.lock().unwrap().is_empty(), "取消后不得启动首条计划");
    }

    /// 未取消时按 pending 顺序串行执行，且不并发。
    #[tokio::test]
    async fn backfill_keeps_order_and_never_overlaps_plans() {
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));
        let active_now = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let fire = {
            let fired = fired.clone();
            let active_now = active_now.clone();
            let max_active = max_active.clone();
            move |schedule: Schedule| {
                let fired = fired.clone();
                let active_now = active_now.clone();
                let max_active = max_active.clone();
                async move {
                    let current = active_now.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    fired.lock().unwrap().push(schedule.id);
                    active_now.fetch_sub(1, Ordering::SeqCst);
                }
            }
        };

        run_backfill(
            vec![
                backfill_schedule("a"),
                backfill_schedule("b"),
                backfill_schedule("c"),
            ],
            || true,
            fire,
        )
        .await;

        assert_eq!(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            *fired.lock().unwrap(),
            "补签必须保持 pending 顺序"
        );
        assert_eq!(1, max_active.load(Ordering::SeqCst), "补签不得并发");
    }

    /// 旧代次失效后不得执行，新代次（重新开启）正常执行。
    #[tokio::test]
    async fn stale_generation_cannot_run_new_generation_plans() {
        let generation = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));

        let make_fire = |fired: Arc<Mutex<Vec<String>>>| {
            move |schedule: Schedule| {
                let fired = fired.clone();
                async move {
                    fired.lock().unwrap().push(schedule.id);
                }
            }
        };

        // 旧代次 2 已被 stop() 推进到 3。
        generation.store(3, Ordering::SeqCst);
        let old_is_current = {
            let generation = generation.clone();
            move || generation.load(Ordering::SeqCst) == 2
        };
        run_backfill(
            vec![backfill_schedule("old")],
            old_is_current,
            make_fire(fired.clone()),
        )
        .await;
        assert!(fired.lock().unwrap().is_empty(), "旧代次不得执行");

        // 新一代 3 正常执行。
        let new_is_current = {
            let generation = generation.clone();
            move || generation.load(Ordering::SeqCst) == 3
        };
        run_backfill(
            vec![backfill_schedule("new")],
            new_is_current,
            make_fire(fired.clone()),
        )
        .await;
        assert_eq!(vec!["new".to_string()], *fired.lock().unwrap());
    }

    /// stop() 没有轮询任务时也必须推进代次（旧实现的 early-return 缺陷）。
    #[tokio::test]
    async fn stop_advances_generation_even_without_dispatch_loop() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-generation");
        set_entries(json!([]));

        let before = service.generation();
        // 从未 start()：inner.task 为 None，旧实现这里直接 return。
        service.stop();
        assert_ne!(before, service.generation(), "stop 必须推进调度代次");

        let stopped = service.generation();
        service.start();
        assert_ne!(stopped, service.generation(), "start 必须开启新代次");
        assert!(!service.backfill_is_current(stopped), "旧代次必须失效");
        assert!(
            service.backfill_is_current(service.generation()),
            "当前代次应有效"
        );

        service.stop();
        cleanup_config();
    }

    /// 关闭总开关（configure(false)）后补签代次失效，且轮询任务被停掉。
    #[tokio::test]
    async fn configure_disable_invalidates_backfill_generation() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-configure-off");
        set_entries(json!([entry("entry-off", "00:00", true)]));
        service.start();
        let generation = service.generation();

        let state = service
            .configure(&json!({ "enabled": false }))
            .expect("关闭总开关应成功");

        assert_eq!(
            false,
            state
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        );
        assert!(
            !service.backfill_is_current(generation),
            "关闭后旧代次必须失效"
        );
        assert!(!master_enabled(), "总开关应已关闭");
        cleanup_config();
    }

    // ─── R49/T2 补测：start/stop 接线与代次生命周期 ─────────────
    //
    // 任务书 §3.5 的「无待补签不提交」「start() 执行全部待补签」「stop 后立即
    // start」「多次 stop 幂等」「多次 start 不产生重复补签」用真实 start()/
    // stop() 驱动；空账号库 ⇒ 零 Provider 网络。观察点用条目记账（fire 入口
    // 即写 lastFiredDate）与 supervisor 的任务名，不靠固定 sleep 猜时序。

    fn backfill_task_name(generation: u64) -> String {
        format!("checkin-backfill:{generation}")
    }

    fn has_live_backfill(generation: u64) -> bool {
        core_task_supervisor().has_live_task(TaskScope::Core, &backfill_task_name(generation))
    }

    fn fired_today(id: &str) -> bool {
        let today = today_key();
        find_entry(id)
            .and_then(|schedule| schedule.last_fired_date)
            .as_deref()
            == Some(today.as_str())
    }

    /// §3.5 基本 #1：没有待补签计划时不提交补签任务。
    #[tokio::test]
    async fn start_without_pending_plans_registers_no_backfill() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-start-empty");
        set_entries(json!([entry("entry-done", "00:00", true)]));

        service.start();

        assert!(
            !has_live_backfill(service.generation()),
            "没有待补签计划时不得注册补签任务"
        );
        service.stop();
        cleanup_config();
    }

    /// §3.5 基本 #2：start() 把全部待补签计划交给补签任务执行。
    #[tokio::test]
    async fn start_dispatches_all_pending_plans() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-start-pending");
        set_entries(json!([
            entry("entry-a", "00:00", false),
            entry("entry-b", "00:00", false),
        ]));

        service.start();
        let generation = service.generation();

        wait_for(
            &mut || fired_today("entry-a") && fired_today("entry-b"),
            "两条待补签计划都应被启动",
        )
        .await;

        service.stop();
        wait_for(
            &mut || !has_live_backfill(generation),
            "停止后补签任务应收尾",
        )
        .await;
        cleanup_config();
    }

    /// 竞态 #4：stop 后立即 start —— 旧代次失效，新代次照常执行。
    #[tokio::test]
    async fn stop_then_immediate_start_runs_only_the_new_generation() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-stop-start");
        set_entries(json!([entry("entry-next", "00:00", false)]));

        service.start();
        let previous = service.generation();
        service.stop();
        service.start();
        let current = service.generation();

        assert_ne!(previous, current, "start 必须开启新代次");
        assert!(!service.backfill_is_current(previous), "旧代次必须失效");
        assert!(service.backfill_is_current(current), "新代次应有效");

        // 旧代次即使被驱动，也不得执行任何计划。
        let old_ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fire = {
            let old_ran = old_ran.clone();
            move |_schedule: Schedule| {
                let old_ran = old_ran.clone();
                async move {
                    old_ran.fetch_add(1, Ordering::SeqCst);
                }
            }
        };
        let is_current = {
            let service = service.clone();
            move || service.backfill_is_current(previous)
        };
        run_backfill(vec![backfill_schedule("old-only")], is_current, fire).await;
        assert_eq!(0, old_ran.load(Ordering::SeqCst), "旧代次不得执行任何计划");

        wait_for(&mut || fired_today("entry-next"), "新代次应启动待补签计划").await;
        service.stop();
        cleanup_config();
    }

    /// 竞态 #5：多次 stop 幂等，且不影响随后的 start。
    #[tokio::test]
    async fn repeated_stop_is_idempotent() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-repeat-stop");
        set_entries(json!([entry("entry-idem", "00:00", true)]));

        let before = service.generation();
        service.stop();
        let first = service.generation();
        service.stop();
        let second = service.generation();

        assert_ne!(before, first, "首次 stop 必须推进代次");
        assert_ne!(first, second, "重复 stop 仍应推进且不 panic");
        assert!(
            service.lock().task.is_none(),
            "重复 stop 后不应残留轮询任务"
        );

        service.start();
        let current = service.generation();
        assert!(
            service.backfill_is_current(current),
            "stop 后的 start 应正常工作"
        );
        service.stop();
        cleanup_config();
    }

    /// 竞态 #6：多次 start 不产生重复补签，两个代次的任务最终全部收尾。
    #[tokio::test]
    async fn repeated_start_leaves_no_duplicate_backfill() {
        let _serial = SCHEDULING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        isolate_state();
        let (service, _guard) = fresh_service("sched-repeat-start");
        set_entries(json!([entry("entry-dup", "00:00", false)]));

        service.start();
        let first = service.generation();
        service.start();
        let second = service.generation();

        assert_ne!(first, second, "每次 start 都应开启新代次");
        assert!(!service.backfill_is_current(first), "旧代次应失效");
        assert!(service.backfill_is_current(second), "新代次应有效");

        wait_for(&mut || fired_today("entry-dup"), "新一代应启动待补签计划").await;

        service.stop();
        wait_for(
            &mut || !has_live_backfill(first) && !has_live_backfill(second),
            "两个代次的补签任务都应收尾，不留孤儿",
        )
        .await;
        cleanup_config();
    }
}
