//! 自定义提供商的目录、转发接线与余额查询。

use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::custom_providers;
use crate::server::core::key_scope::{self, KeyScope};
use crate::server::core::models::{list_item, model_id};

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

pub fn append_models_response(store: &AccountStore, scope: Option<&KeyScope>, data: &mut Vec<Value>) {
    let mut claimed: std::collections::HashSet<_> = data.iter()
        .map(|item| model_id(item).to_lowercase()).collect();
    for (provider, models) in catalog_providers(store) {
        if !key_scope::allows_provider(scope, &provider) { continue; }
        for model in models {
            let id = model_id(&model);
            if key_scope::allows_model(scope, &id) && claimed.insert(id.to_lowercase()) {
                data.push(list_item(&model, &provider));
            }
        }
    }
}
