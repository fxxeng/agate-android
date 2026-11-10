//! 账号侧的**启动期一次性迁移**：唯一入口，两处调用。
//!
//! 为什么单独一个文件（而不是留在 `server/mod.rs` 里）
//! ────────────────────────────────────────────────────
//! 两件事凑在一起：`server/mod.rs` 已经超过本项目的单文件行数约定（800 行），
//! 而这一组迁移的**调用时机**（启动时跳过、升级后补做）恰恰是最容易读错的一处，
//! 值得一个能被直接找到的位置。
//!
//! ── 旧登录/账号数据不再兼容（2026-11-10 委托方裁定）──────────
//! 账号可以重新登录，所以不再维护旧登录态导入与旧账号文件迁移：
//!   - 旧版单账号 `auth.json` → 账号；
//!   - Raccoon / CatPaw / AutoClaw 桌面旧账号文件导入；
//!   - chatglm 嵌套 `auth.accessToken` → 顶层迁移。
//! 这些入口与模块已删除，旧文件原地保留、不再读取；用户重新登录/添加即可。
//! 仍保留的是**当前数据库形态**的收敛迁移（provider / 优先级 / 拆池 / 拆家 /
//! 明文加密 / 限流键），它们不是“旧登录数据兼容”，删掉会让现行账号列表的
//! 语义或静态加密过渡态失效。
//!
//! ── 为什么仍由两处调用 ──────────────────────────────────────
//! 这些迁移建立在「库里的账号/配置就是当前真相」这个前提上。若还有待迁移的
//! 旧 JSON/JSONL 配置，启动时会先跳过，等 `POST /api/upgrade/run` 把配置搬进库、
//! `config::reload()` 之后再补调一次；否则 `migrate_cline_split` 读不到刚导入的
//! `modelRules`，本次运行会看到半迁移状态。
//!
//! ── 顺序（有真实依赖，别随手调）─────────────────────────────
//!   `migrate_startup`（整队/拆池/拆家）→ 历史账号凭证加密
//!   → 历史网关 Key 加密 → 顶层 `apiKey` 原地加密 → `migrate_cline_split`
//!   → 网关 Key 白名单补齐 → 限流冷却键清理
//!   - 三条「形态加密」紧随 `migrate_startup` 之后、彼此相邻：第一条收尾它没
//!     碰到的历史明文行；后两条只碰 kv 里的 `apiKeys` 与顶层 `apiKey`。
//!   - `migrate_cline_split` 必须排在 `migrate_startup` 之后：后者把 Cline 的
//!     账号 id 拆成 `cline-free` / `cline-pass`，前者改的是按 provider id 记账
//!     的 `modelRules`；顺序反了，规则会在旧 id 状态下被判一遍池。
//!   - 它们必须读**已经导入过配置**的快照：升级路径先 `config::reload()`，
//!     启动路径 `config::init` 早已装好。
//!   - 限流键清理读 `model_rules::current()` 的映射表，且要早于任何转发。

use crate::server::core::account_store::AccountStore;
use crate::server::core::model_rules;
use crate::server::logging;

/// 账号侧的启动期一次性迁移（幂等，可重复调用）。
pub(crate) fn run(store: &AccountStore) {
    // 启动期数据迁移（一次读、一次写，见 store_admin::migrate_startup）：
    //   ① 历史账号补 `provider: "workbuddy"`（Agate 惰性迁移，§3.2）
    //   ② 优先级去重 —— 逐 provider 分组重编号，相对顺序保持不变
    //   ③ Cline 拆池改名（`provider: "cline"` + `pool` → cline-free / cline-pass）
    //   ④ WorkBuddy 拆家（`provider: "workbuddy"` + `edition: "intl"` → workbuddy-intl）
    store.migrate_startup();
    // 历史明文账号行的整列加密（见 store_admin::encrypt_legacy_accounts）：
    // `accounts.data` 从本轮改造起写的是 `enc1:` 密文，而升级前已在库里的行仍是
    // 明文 JSON（读侧 `decode_data` 兼容明文）。这一步把过渡态收敛掉 —— 之后
    // 「拷走库文件」就看不到任何凭证。位置在 `migrate_startup` 之后：那一轮会按
    // 差异重写改了字段的行（写出的已经加密），剩下没被它碰到的历史行由这一步收尾。
    // 幂等（判据是数据自证，没有 kv 标记）、失败不阻断启动（下次启动重试）。
    store.encrypt_legacy_accounts();
    // 网关 Key 的历史明文加密（见 `core::api_keys::encrypt_legacy_keys`）：
    // `apiKeys` 里每把 Key 的 `key` 从本轮改造起写的是 `enc1:` 密文，而升级前
    // 已在配置里的仍是明文（读侧 `from_value` 兼容明文）。位置与上一条并列；
    // 读 `config::current().raw()`，因此必须等配置快照就位。
    crate::server::core::api_keys::encrypt_legacy_keys();
    // 顶层旧字段 `apiKey` 的**原地**加密（见 `core::legacy_api_key`）：与上一条
    // 同族的第三条「形态加密」—— `apiKeys` 数组与顶层 `apiKey` 是同一把网关口令
    // 的两种形态。它只换值、不搬字段，避免破坏「写侧动过列表才落成 apiKeys 并
    // 删掉 apiKey」的既有语义。
    crate::server::core::legacy_api_key::encrypt_legacy_api_key();
    // 模型规则的同一轮迁移：改的是 `kv` 里的 `modelRules`，必须读已导入的配置快照。
    if let Some(summary) = model_rules::migrate_cline_split() {
        logging::log("[Models]", &summary);
    }
    // WorkBuddy 拆家（2026-10）的三条存量迁移：统一挂在一次性标记下。
    // 模型规则 / 网关 Key 白名单 / 按家提示词覆盖拆家前都由 `workbuddy` 一个 id
    // 覆盖两个地区，因此都要把那份配置**复制**给国际版。必须成组只跑一次：
    // 「一边有、另一边没有就补一份」的判据在拆家之后一直成立，不设标记会在每次
    // 启动把用户之后新做的国内版配置反复镜像过去。
    if !crate::server::config::workbuddy_split_migrated() {
        let (outcome, rules_summary) = model_rules::migrate_workbuddy_split();
        let patched_keys = crate::server::core::api_keys::add_intl_to_workbuddy_allowlists();
        let prompt_summary = crate::server::config::migrate_workbuddy_split_prompt_providers();
        if let Some(summary) = rules_summary {
            logging::log("[Models]", &summary);
        }
        if !patched_keys.is_empty() {
            logging::log(
                "[Keys]",
                &format!(
                    "🔑 已为 {} 把网关 Key 补上「WorkBuddy 国际版」的可用提供商（拆家前 workbuddy 覆盖两地，限制范围保持不变）：{}",
                    patched_keys.len(),
                    patched_keys
                        .iter()
                        .map(|entry| entry.name.as_str())
                        .collect::<Vec<_>>()
                        .join("、"),
                ),
            );
        }
        if let Some(summary) = prompt_summary {
            logging::log("[Prompt]", &summary);
        }
        // 规则迁移没落盘时不写标记（下次启动重来一次，好过让用户的启停状态
        // 停在半迁移位置）。另两条各自独立落盘，失败也只影响自己。
        if outcome.allows_marking_done() {
            if !crate::server::config::mark_workbuddy_split_migrated() {
                logging::log(
                    "[Models]",
                    "⚠️  WorkBuddy 拆家迁移标记未能落盘：下次启动会再核对一遍（三条迁移都幂等，不会重复改动）",
                );
            }
        }
    }
    // 限额冷却键的存量修复：把按**请求名**记下的键（映射别名）清掉。
    // 位置必须在这里 —— 它读 `model_rules::current()` 的映射表，且要早于任何
    // 转发（否则孤儿键会继续在账号页上显示成「限流中」）。
    let stale_keys = store.migrate_rate_limit_keys();
    if !stale_keys.is_empty() {
        logging::log(
            "[Accounts]",
            &format!(
                "🧹 已清理 {} 条错位的限流记录（旧版把映射别名当成冷却键，实际额度按上游真名记）：{}",
                stale_keys.len(),
                stale_keys.join("；"),
            ),
        );
    }
}
