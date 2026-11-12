//! 自定义提供商的目录、转发接线与余额查询。

use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::custom_providers;
use crate::server::core::key_scope::{self, KeyScope};
use crate::server::core::models::{list_item, model_id};
use crate::server::core::providers::catalog::ListMode;

/// 余额查询：七种方案（sub2api / NewAPI / DeepSeek / OpenCode Go / 商汤日日新 /
/// 智谱 / 智谱 z.ai 国际）的编排。方案由提供商记录的 `balanceScheme` 决定
/// （预置卡写入），查询凭证按账号存（`balanceQuery`）—— 详见各文件模块头。
pub mod balance;
/// 七种方案的上游响应解析（纯函数；与 `balance` 的请求链路分开，
/// 便于对照移植来源逐字段读）
pub mod balance_parse;
pub mod emulation;
pub mod forward;
/// 商汤日日新的账号密码登录（OAuth2 + PKCE 换控制台 JWT），只服务余额查询
pub mod sensenova_auth;

/// 目录里的自定义家：`id` → 该家的对外模型清单。
///
/// ── 零模型的家**也在表里**（2026-10-30 修）────────────────────
/// 旧实现要求 `!models.is_empty()` 才留下这家，于是「刚建好、还没登记模型」
/// 的自定义家从目录里**彻底消失**：模型页的【全部】筛选行按清单里的家派生，
/// 于是这家不出现 —— 用户看到的就是「我建的这家怎么不见了」，而界面上
/// 没有任何报错能解释它去哪儿了。
///
/// 更要命的是它与「拉取模型」构成死循环：拉取弹层的候选要「零模型也列出」
/// 才能拉第一家，而第一家拉不到模型就永远进不了筛选行。
///
/// 现在零模型的家照旧在表里（模型列表为空）：调用方按「这家有哪些模型」
/// 取用，空列表就是空列表 —— 语义诚实，且不影响任何按名字取模型的路径。
pub(crate) fn catalog_providers(store: &AccountStore) -> Vec<(String, Vec<Value>)> {
    custom_providers::list().into_iter().filter_map(|provider| {
        let id = provider.get("id")?.as_str()?.to_string();
        if store.accounts_for_provider(&id).is_empty() { return None; }
        let models = custom_providers::public_models(&provider);
        Some((id, models))
    }).collect()
}

/// 自定义家的条目在各清单形态下写成什么（裸名 / `provider/model`，或两者都要）。
///
/// ── 为什么必须跟着形态走（2026-11-12 真机反馈）──────────────
/// 这里原先是无条件 `data.push(list_item(&model, &provider))`：内置家的循环
/// （`catalog::view::public_builtin_items`）按 `ListMode` 决定带不带前缀，而
/// 自定义家这一段**完全不知道当前形态**。于是「按提供商」模式下清单里多出几条
/// **没有前缀**的条目 —— 用户看到的是「最后三个没有前缀」，而那三个正是他自己
/// 的自定义 / 预置 API 家的模型名（重名时与别家带前缀的那条并列，看起来像
/// 重复，也看不出是哪一家的）。
///
/// 语义与内置家逐条对齐（见 `ListMode` 的说明）：
///   · `Model`    → 只出裸名（跨家同名去重）；
///   · `Provider` → 只出带前缀（每家的每个模型各一条）；
///   · `Hybrid`   → 裸名 + 「被 pin 的那几家 / 那几条」另出带前缀。
fn exposed_names(provider: &str, id: &str, mode: ListMode, pinned: bool) -> Vec<String> {
    let prefixed = match mode {
        ListMode::Provider => true,
        ListMode::Hybrid => pinned,
        ListMode::Model => false,
    };
    let mut out = Vec::new();
    if mode != ListMode::Provider {
        out.push(id.to_string());
    }
    if prefixed {
        out.push(format!("{provider}/{id}"));
    }
    out
}

/// 把自定义家的对外条目并进清单。
///
/// ── 白名单按**同义名**判（与内置家同一口径）──────────────────
/// 自定义家的别名存在**自己的记录**里（`mappings[]`），不在 `modelRules` —— 所以
/// `catalog::synonymous_names` 看不见它们。清单按别名收敛之后，一把勾了**原名**
/// 的 Key 会因此看不到这个模型（逐字比对判成「不在白名单」）。这里就地把这家
/// 自己的「名字 + 它的同义名」展开一次，判据与 `bindings::public_models`
/// 的收敛口径同源。
///
/// ── 形态也决定了清单里的写法（[`exposed_names`]）──────────────
/// 「按提供商」必须每一条都带前缀，否则那几条既看不出是哪一家，也没法与别家
/// 同名的条目区分。带前缀的写法在转发层是**锁家**语义（见
/// `catalog::routing::split_provider_prefix`，自定义家 id 也认）。
pub fn append_models_response(
    store: &AccountStore,
    scope: Option<&KeyScope>,
    mode: ListMode,
    pins: &[String],
    data: &mut Vec<Value>,
) {
    let mut claimed: std::collections::HashSet<_> = data.iter()
        .map(|item| model_id(item).to_lowercase()).collect();
    let restricts = scope.is_some_and(KeyScope::restricts_models);
    for (provider, models) in catalog_providers(store) {
        if !key_scope::allows_provider(scope, &provider) { continue; }
        // 这一家要不要整体带前缀（Hybrid 的 pin 语义）：整家 pin 认 provider id
        // 与展示名，单条 pin 认 `provider/模型` —— 与内置家同一套写法。
        let label = crate::server::core::providers::label_of(&provider);
        let pinned = |name: &str| {
            pins.iter().any(|pin| {
                pin.eq_ignore_ascii_case(&provider)
                    || pin.eq_ignore_ascii_case(&label)
                    || pin.eq_ignore_ascii_case(&format!("{provider}/{name}"))
            })
        };
        // 这家的允许集合：勾了哪些名字 + 它们的同义名（只在真的限制了模型时算）
        let allowed: Option<std::collections::HashSet<String>> = if restricts {
            let record = crate::server::core::custom_providers::get(&provider);
            let mut set = std::collections::HashSet::new();
            for name in scope.map(KeyScope::allowed_model_names).unwrap_or_default() {
                set.insert(name.clone());
                for synonym in record.as_ref()
                    .map(|record| crate::server::core::custom_providers::synonymous_models(record, &name))
                    .unwrap_or_default()
                {
                    set.insert(synonym.to_lowercase());
                }
            }
            Some(set)
        } else {
            None
        };
        for model in models {
            let id = model_id(&model);
            // 白名单里可能是裸名，也可能是用户在清单里看到的 `provider/模型`
            // —— 两种写法都算勾过（否则切到「按提供商」这把 Key 的清单会变空）
            let prefixed = format!("{provider}/{id}");
            let permitted = match &allowed {
                None => true,
                Some(set) => set.contains(&id.to_lowercase())
                    || set.contains(&prefixed.to_lowercase()),
            };
            if !permitted { continue; }
            for name in exposed_names(&provider, &id, mode, pinned(&id)) {
                if !claimed.insert(name.to_lowercase()) { continue; }
                let mut item = list_item(&model, &provider);
                // 带前缀那条与内置家同一写法：id / name 都是 `provider/模型`，
                // 且不是默认模型（客户端照抄这个名字发请求 → 锁家）
                if !name.eq_ignore_ascii_case(&id) {
                    if let Some(object) = item.as_object_mut() {
                        object.insert("id".to_string(), Value::String(name.clone()));
                        object.insert("name".to_string(), Value::String(name));
                        object.insert("is_default".to_string(), Value::Bool(false));
                    }
                }
                data.push(item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「按提供商」必须每一条都带前缀。
    ///
    /// 2026-11-12 真机：清单末尾三条没有前缀（`glm-5.3` / `kimi-k3` /
    /// `deepseek-v4.1-flash-1017`），它们正是自定义 / 预置 API 家的模型 ——
    /// 内置家那段跟着形态走、这一段没有，于是同一份清单里两种写法混着出。
    #[test]
    fn provider_mode_prefixes_custom_entries() {
        assert_eq!(
            exposed_names("custom-relay", "glm-5.3", ListMode::Provider, false),
            vec!["custom-relay/glm-5.3".to_string()],
        );
        // Provider 形态下带不带前缀与 pin 无关（全带）
        assert_eq!(
            exposed_names("custom-relay", "glm-5.3", ListMode::Provider, true),
            vec!["custom-relay/glm-5.3".to_string()],
        );
    }

    /// 「按模型」保持裸名（同名跨家合并，选哪家交给路由队列）
    #[test]
    fn model_mode_keeps_custom_entries_bare() {
        assert_eq!(
            exposed_names("custom-relay", "glm-5.3", ListMode::Model, true),
            vec!["glm-5.3".to_string()],
        );
    }

    /// 「混合」：裸名 + 被 pin 的那家另加一条带前缀的（与内置家同一语义）
    #[test]
    fn hybrid_prefixes_only_pinned_custom_entries() {
        assert_eq!(
            exposed_names("custom-relay", "glm-5.3", ListMode::Hybrid, false),
            vec!["glm-5.3".to_string()],
        );
        assert_eq!(
            exposed_names("custom-relay", "glm-5.3", ListMode::Hybrid, true),
            vec!["glm-5.3".to_string(), "custom-relay/glm-5.3".to_string()],
        );
    }
}
