//! 对外模型名到上游模型的解析。路由与发送侧共用本解析，避免选中一家后再换一套规则。

use serde_json::Value;

use crate::server::core::{custom_providers, model_rules};
use crate::server::core::models::model_id;
use crate::server::core::providers::{kind_from_id, kind_id, ProviderKind};

use super::{adapter_for, all_kinds, manifest_for};
use crate::server::core::providers::images::model_is_image;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireTarget {
    pub model: String,
    pub reasoning: Option<String>,
}

pub(super) fn entry_id_in_manifest(manifest: &[Value], requested: &str) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    // 保留展示名兼容，但必须先匹配原始 ID；返回清单里的实际大小写。
    manifest.iter().find(|entry| model_id(entry).eq_ignore_ascii_case(requested))
        .or_else(|| manifest.iter().find(|entry| {
            entry.get("name")
                .map(crate::server::core::models::shape_value_text)
                .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        }))
        .map(model_id)
}

/// 对外名 → 这一家的上游 id。**两条路都必须过启停**：
///
///   1. 直接命中清单里的原生 id（含展示名回落）；
///   2. 命中一条**生效**的别名映射 —— 且它指向的目标在这家是**开着**的。
///
/// 第 2 条的后半句是 2026-11-12 补上的：只判映射自己的开关时，模型页关掉的
/// 模型仍能通过自动种下的别名被解析（详见函数内的注释）—— 于是「关掉」既
/// 不拦广告（`/v1/models`）、也不拦转发（`route_for_forward`）。
pub(super) fn builtin_target(
    rules: &model_rules::ModelRules,
    kind: ProviderKind,
    manifest: &[Value],
    requested: &str,
) -> Option<WireTarget> {
    let provider = kind_id(kind);
    if let Some(id) = entry_id_in_manifest(manifest, requested) {
        if rules.default_enabled(provider, &id) {
            return Some(WireTarget {
                reasoning: rules.binding(provider, &id, &id).and_then(|m| m.reasoning.clone()),
                model: id,
            });
        }
    }
    let candidates: Vec<_> = rules.mappings.iter()
        .filter(|mapping| mapping.alias.eq_ignore_ascii_case(requested))
        .filter(|mapping| !mapping.alias.eq_ignore_ascii_case(&mapping.target))
        .filter(|mapping| mapping.provider.as_deref().map_or(true, |id| id == provider))
        .filter(|mapping| rules.binding(provider, &mapping.alias, &mapping.target)
            .is_some_and(|effective| effective.enabled))
        .filter_map(|mapping| {
            // ── 目标模型在这家被关掉 → 别名跟着失效（2026-11-12 真机）──────
            // 别名是「这一行的另一个名字」，不是绕过启停的后门。
            //
            // 症状：小浣熊每次拉目录都会给 `sn-*` 种一条去前缀别名
            // （`sn-kimi-k8 → kimi-k8`，`enabled: true`，见
            // `model_rules::seed_raccoon_defaults`），而模型页的开关只写
            // `disabled[(provider, 上游 id)]`。此前这里只判映射自己的
            // `enabled`，于是「模型页关了 8 个、只剩 2 个」之后，`kimi-k8`
            // 照样解析成 `sn-kimi-k8`：清单继续广告它、请求继续派给这家，
            // 上游回 408 —— 用户看到的是「这家一个模型都没启用，却还在用它」。
            //
            // 判据取 `default_enabled`（与上面按原生 id 命中那条同源）：
            // 它同时覆盖 `disabled` 与「别名与 id 逐字相同」的恒等映射开关。
            entry_id_in_manifest(manifest, &mapping.target)
                .filter(|target| rules.default_enabled(provider, target))
                .map(|target| (mapping, target))
        })
        .collect();
    let prefix = super::super::cline::models::Pool::from_provider_id(provider)
        .map(super::super::cline::models::Pool::target_prefix);
    let selected = candidates.iter()
        .filter(|(mapping, _)| mapping.provider.is_some())
        .find(|(_, target)| prefix.is_some_and(|prefix| target.starts_with(prefix)))
        .or_else(|| candidates.iter().find(|(mapping, _)| mapping.provider.is_some()))
        .or_else(|| candidates.first());
    selected.map(|(mapping, target)| WireTarget {
        model: target.clone(),
        reasoning: rules.binding(provider, &mapping.alias, &mapping.target)
            .and_then(|effective| effective.reasoning.clone()),
    })
}

/// 这一家里「已被生效别名取代」的 `(原生 id, 别名)` 对。
///
/// ── 为什么「改名」要影响清单，而不只影响解析 ────────────────
/// 解析侧映射是「追加一个可用名」：原名照旧能调（见 [`builtin_target`] 的第一
/// 段）。所以在清单里两个都列在技术上不算错 —— 但它**不是用户要的**。用户建
/// 映射的动机就是「我这边只叫这个名字」：清单里同时出现 `sn-glm-5-3` 与
/// `glm-5.3`，客户端下拉列表就有两条看起来不同、实则同一个上游模型的条目。
/// 2026-11-03 用户为此报了第三次：模型列表按别名显示之后，「客户端所见清单」
/// 里**还是原名** —— 那张卡宣称自己等于 `GET /v1/models`，而它确实等于：
/// 问题在这一层（清单出口），不在界面。
///
/// ── 只影响「列不列」，不影响「能不能调」─────────────────────
/// 返回的是「该隐去哪条原名」，调用方据此**只从清单里删掉它**。转发侧不受影响：
/// 老客户端 / 老 Key 白名单里写着的原名照旧打得通。这是刻意的 ——
/// 已投产的调用方不该因为一次改名而静默失效（`api::pipeline` 那层「列表里没有
/// 就拒绝」的门禁因此要额外认这些原名，见 `catalog::callable_names`）。
///
/// ── 两条判据都不能少 ────────────────────────────────────
///   1. 有一条 **enabled** 映射，`target` 是这条原生 id 且 `alias != target`；
///   2. 那条别名在本家**真的解析得回同一个 id**
///      （`builtin_target` 命中的上游条目正是它）。
///
/// 少了第 2 条，别名指向一个已下架 / 已换名的上游 id 时原名也被隐去 —— 这个
/// 模型会从清单里**彻底消失**，而它其实还能调。少列一个名字只是吵，消失是故障。
/// 第 2 条还要求「解析回**这同一个** id」而不只是「解析得通」：别名与这家另一个
/// 原生 id 撞名时（`glm-5.3` 既是别名、又是别条的原生 id），解析会落到那条
/// 原生条目上 —— 此时隐去原名等于让这个名字无路可走。
///
/// 返回成对而不是只给原名集合：调用方还要回答「这条别名替谁顶了名」
/// （清单条目要带上它，供 Key 白名单那道过滤认老名字）。
pub(super) fn renamed_away_pairs(
    rules: &model_rules::ModelRules,
    kind: ProviderKind,
    manifest: &[Value],
) -> Vec<(String, String)> {
    let provider = kind_id(kind);
    let mut out: Vec<(String, String)> = Vec::new();
    for mapping in rules.mappings.iter() {
        if !mapping.enabled || mapping.alias.eq_ignore_ascii_case(&mapping.target) {
            continue;
        }
        if !mapping.provider.as_deref()
            .map_or(true, |owner| owner.eq_ignore_ascii_case(provider))
        {
            continue;
        }
        // 别名本身也是这家的原生 id 时不算「改名」：那只是给一个已存在的名字
        // 加一条同义映射，隐去原名会把那条原生模型一起删掉
        if manifest.iter().any(|item| model_id(item).eq_ignore_ascii_case(&mapping.alias)) {
            continue;
        }
        // `target` 必须是这家的原生 id（不是别家的、也不是另一个别名）
        if !manifest.iter().any(|item| model_id(item).eq_ignore_ascii_case(&mapping.target)) {
            continue;
        }
        let resolves_back = builtin_target(rules, kind, manifest, &mapping.alias)
            .is_some_and(|wire| wire.model.eq_ignore_ascii_case(&mapping.target));
        if resolves_back {
            out.push((mapping.target.clone(), mapping.alias.clone()));
        }
    }
    out
}

/// 原始能力查询不应用开关，供管理 API 展开旧版全局规则。
pub fn providers_for_model(model: &str) -> Vec<String> {    let model = model.trim();
    if model.is_empty() {
        return Vec::new();
    }
    let manifests: Vec<_> = all_kinds().into_iter().map(|kind| (kind, manifest_for(kind))).collect();
    let exact = manifests.iter().any(|(_, items)| {
        items.iter().any(|item| model_id(item).eq_ignore_ascii_case(model))
    });
    let mut result: Vec<_> = manifests.into_iter().filter(|(_, items)| {
        if exact {
            items.iter().any(|item| model_id(item).eq_ignore_ascii_case(model))
        } else {
            entry_id_in_manifest(items, model).is_some()
        }
    }).map(|(kind, _)| kind_id(kind).to_string()).collect();
    result.extend(custom_providers::list().into_iter().filter_map(|provider| {
        let models = provider.get("models")?.as_array()?;
        models.iter().any(|entry| model_id(entry).eq_ignore_ascii_case(model))
            .then(|| provider.get("id").and_then(Value::as_str).unwrap_or("").to_string())
    }));
    result
}

/// 只返回有开启绑定的提供商，不把关闭的 ID 回落给默认提供商。
pub fn forwarding_providers(model: &str) -> Vec<String> {
    let rules = model_rules::current();
    let mut result: Vec<_> = all_kinds().into_iter().filter(|kind| {
        builtin_target(&rules, *kind, &manifest_for(*kind), model).is_some()
    }).map(|kind| kind_id(kind).to_string()).collect();
    result.extend(custom_providers::carriers_of_model(model));
    result
}

pub fn model_blocked_everywhere(model: &str) -> bool {
    // ── 先剥「锁家」前缀（2026-11-12 修）─────────────────────────
    // `provider/模型` 与裸名是**同一个模型**的两种写法（见
    // `catalog::synonymous_names` 的说明）。不先归一的话，「按提供商」清单里
    // 复制出来的名字会因为「目录里找不到这个字符串」被判成「已在网关中关闭」
    // —— 名字明明就在客户端刚拉到的清单里。内置家与自定义家同此一处。
    let normalized = split_provider_prefix(model)
        .map(|(_, bare)| bare)
        .unwrap_or_else(|| model.trim().to_string());
    let model = normalized.as_str();
    if !forwarding_providers(model).is_empty() {
        return false;
    }
    !providers_for_model(model).is_empty()
        || model_rules::current().has_alias(model)
        || custom_providers::list().iter().any(|provider| {
            provider.get("mappings").and_then(Value::as_array).is_some_and(|mappings| {
                mappings.iter().any(|mapping| mapping.get("alias").and_then(Value::as_str)
                    .is_some_and(|alias| alias.eq_ignore_ascii_case(model)))
            })
        })
}

/// 把对外带前缀名拆成 `(provider_id, 裸名)`。
///
/// 只认**已登记的 provider id**（内置家 + 自定义家）：`deepseek/deepseek-chat`
/// 这类真实模型名不会命中（head 不是 provider id），所以可以放心在转发入口先
/// 用它试一次。
///
/// ── 自定义家也要认（2026-11-12 修）───────────────────────────
/// 「按提供商」形态下自定义家的条目现在也写成 `custom-xxx/模型`（见
/// `providers::custom::exposed_names`）。前缀不在这里被认出来，那个名字就
/// 只是「清单里看得见、发出去 404」—— 比不带前缀更糟。`is_custom_provider_id`
/// 同时要求 `custom-` 前缀与记录存在，真实模型名不会误命中。
///
/// 这是清单里「`provider/model` 锁定这一家」那条语义的**请求侧**实现 ——
/// 没有它，候选家只能拿完整名字去目录里查，一个也查不到，于是回落默认家：
/// 名字对了、家却可能错（多提供商有同名模型时尤其危险）。
pub fn split_provider_prefix(model: &str) -> Option<(String, String)> {
    let text = model.trim();
    let (head, rest) = text.split_once('/')?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    if kind_from_id(head).is_none() && !custom_providers::is_custom_provider_id(head) {
        return None;
    }
    Some((head.to_string(), rest.to_string()))
}

pub fn wire_target_for_provider(model: &str, provider_id: &str, _account: Option<&Value>) -> WireTarget {
    let requested = model.trim();
    // 对外带前缀名（`provider/model`）：前缀是清单里的「锁家」写法，**上游不认识**
    // —— 原样发出去会被上游拒。实测：`workbuddy/glm-5.3-flash` →
    // 上游 400 `model [workbuddy/glm-5.3-flash] service info not found`。
    //
    // 只在 head **与目标提供商一致** 时剥：真实模型名里也可能带 `/`
    // （OpenRouter 的 `deepseek/deepseek-chat`），见斜杠就切会把它们切坏。
    let requested = match requested.split_once('/') {
        Some((head, rest)) if !rest.is_empty() && head.eq_ignore_ascii_case(provider_id) => rest,
        _ => requested,
    };
    // 自定义提供商在自己的转发模块解析一次，避免把别名提前改写后丢失其等级绑定。
    let resolved = kind_from_id(provider_id).and_then(|kind| {
        builtin_target(&model_rules::current(), kind, &manifest_for(kind), requested)
    });
    resolved.unwrap_or_else(|| WireTarget { model: requested.to_string(), reasoning: None })
}

/// 「默认模型」的候选清单：**排除图片模型**。
///
/// ── 为什么必须排除（不做就会再错一次）─────────────────────────
/// 这份清单唯一的消费者是对话入口的默认模型回落（`pipeline::resolve_model_for`
/// 的三级回落链）。图片条目混进来之后的症状很难查：客户端没点名模型 →
/// 回落挑中一条 `kind=image` → 对话入口的**反向门禁**把它拦下，回一句
/// 「模型 X 是图片生成模型，请使用 POST /v1/images/generations」。
/// 用户既没点名模型，也没打算生图，却被告知去用另一个端点。
///
/// 排除的成本是零：图片模型从来不参与「默认对话模型」这个概念。
pub fn default_model_catalog() -> Vec<Value> {
    let rules = model_rules::current();
    let mut result = Vec::new();
    for kind in all_kinds().into_iter().filter(|kind| adapter_for(*kind).supports_default_model()) {
        for entry in manifest_for(kind) {
            let id = model_id(&entry);
            if !id.is_empty()
                && rules.default_enabled(kind_id(kind), &id)
                && !model_is_image(&entry)
                && !result.iter().any(|item| model_id(item).eq_ignore_ascii_case(&id))
            {
                result.push(entry);
            }
        }
    }
    result
}

pub fn default_model_usable(model: &str) -> bool {
    entry_id_in_manifest(&default_model_catalog(), model).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core::model_rules::ModelRules;

    /// 构造一份只含 mappings 的规则（不碰全局 config）
    fn rules_with(mappings: Value) -> ModelRules {
        let raw = serde_json::json!({ "modelRules": { "mappings": mappings } })
            .as_object()
            .cloned()
            .unwrap_or_default();
        ModelRules::from_raw(&raw)
    }

    /// **模型被关掉 = 它的别名一起失效**（2026-11-12 真机：小浣熊只剩 2 个
    /// 启用，`kimi-k8` 的请求却仍被派给小浣熊并 408）。
    ///
    /// 复现链：拉目录时 `model_rules::seed_raccoon_defaults` 给每个 `sn-*`
    /// 自动种一条去前缀别名（`enabled: true`），而模型页的开关只写
    /// `disabled[{provider, 上游 id}]`。别名分支只判映射自己的 `enabled` 时，
    /// `kimi-k8` 照样解析成 `sn-kimi-k8` —— 清单继续广告、`route_for_forward`
    /// 继续把这家算成承载家，于是开关只是摆设。
    #[test]
    fn alias_of_a_disabled_target_does_not_resolve() {
        let raw = serde_json::json!({
            "modelRules": {
                "mappings": [
                    { "alias": "kimi-k8", "target": "sn-kimi-k8", "provider": "raccoon", "enabled": true },
                    { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
                ],
                "disabled": [{ "provider": "raccoon", "id": "sn-kimi-k8" }]
            }
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        let rules = ModelRules::from_raw(&raw);
        let manifest = vec![
            serde_json::json!({ "id": "sn-kimi-k8" }),
            serde_json::json!({ "id": "sn-glm-5-3" }),
        ];
        assert!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "kimi-k8").is_none(),
            "模型已关，别名不该还能解析出上游 id"
        );
        assert!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "sn-kimi-k8").is_none(),
            "原名同样不可用：两条路必须同一判据"
        );
        // 没关的那条不受影响 —— 修复不能把功能一起修没
        assert_eq!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "glm-5.3")
                .map(|target| target.model),
            Some("sn-glm-5-3".to_string())
        );
    }

    /// **别名必须能把请求名解析回本家的上游 id**。
    ///
    /// 这是「映射改名后能不能用」的核心事实：用户把小浣熊的 `sn-glm-5-3`
    /// 映射成 `glm-5.3` 之后，客户端发 `glm-5.3`，网关在这一家必须发
    /// `sn-glm-5-3`（上游只认这个）。
    ///
    /// 2026-11-02：用户报「加了映射还是单独显示、没归入候选」。功能这一层
    /// 实际是通的（本测试证明），问题在**模型页的展示口径**（`manage_view`
    /// 的 `models[]` 只列上游 id，别名只在 `mappings[]` 里）—— 两个事实
    /// 分开钉住，免得下次又去改一个本来就对的解析。
    #[test]
    fn alias_resolves_to_upstream_id_within_the_same_provider() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        let resolved = builtin_target(&rules, ProviderKind::Raccoon, &manifest, "glm-5.3");
        assert_eq!(
            resolved.map(|target| target.model),
            Some("sn-glm-5-3".to_string()),
            "别名 glm-5.3 在小浣熊家必须解析成上游 id sn-glm-5-3"
        );
        // 原始 id 仍然可直接用（映射是追加别名，不是替换）
        assert_eq!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "sn-glm-5-3")
                .map(|target| target.model),
            Some("sn-glm-5-3".to_string())
        );
    }

    /// 别名**在别家解析不出来**（这一家没有这个上游 id）—— 这正是「同名多家
    /// 主备」的前提：每家各自解析得了，路由才可能把同名请求分给它。
    #[test]
    fn alias_does_not_leak_into_a_provider_without_that_target() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        // WorkBuddy 的清单里没有 sn-glm-5-3
        let other = vec![serde_json::json!({ "id": "glm-5.3" })];
        let resolved = builtin_target(&rules, ProviderKind::WorkBuddy, &other, "glm-5.3");
        // 它靠自己的原生 id 命中（而不是借小浣熊那条映射）
        assert_eq!(
            resolved.map(|target| target.model),
            Some("glm-5.3".to_string())
        );
    }

    /// 关掉的映射不参与解析（用户关掉 = 这个名字在这一家失效）
    #[test]
    fn disabled_alias_does_not_resolve() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": false }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        assert!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "glm-5.3").is_none(),
            "关闭的映射不该把 glm-5.3 解析成 sn-glm-5-3"
        );
    }

    /// 生效别名 → 原名进「让位」集合（清单出口据它把原名从 `/v1/models` 拿掉）
    #[test]
    fn enabled_alias_marks_the_original_as_renamed_away() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        let pairs = renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest);
        assert_eq!(pairs, vec![("sn-glm-5-3".to_string(), "glm-5.3".to_string())]);
    }

    /// 关掉的映射不让位（「关掉映射才显示原名」）
    #[test]
    fn disabled_alias_does_not_rename_away_the_original() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": false }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        assert!(renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest).is_empty());
    }

    /// 别家的映射不给这一家改名（provider 归属）
    #[test]
    fn other_providers_alias_does_not_rename_away_this_manifest() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "workbuddy", "enabled": true }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        assert!(renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest).is_empty());
    }

    /// 图片模型不进「默认对话模型」候选。
    ///
    /// 混进去的症状：客户端没点名模型 → 回落挑中一条 `kind=image` → 对话入口
    /// 的反向门禁拦下它，回一句「请使用 POST /v1/images/generations」。
    /// 用户既没点名也没打算生图，却被告知去用另一个端点 —— 而这条路径上
    /// 没有任何一处能看出是回落挑错了。
    #[test]
    fn image_models_are_never_default_chat_candidates() {
        let chat = serde_json::json!({ "id": "gemini-3-flash" });
        let image = serde_json::json!({
            "id": "gemini-3-pro-image",
            "kind": "image",
            "output_modalities": ["image"],
        });
        assert!(!model_is_image(&chat));
        assert!(model_is_image(&image));
        // 判据本身（`default_model_catalog` 的过滤就是这一句）
        let candidates: Vec<_> = [chat, image]
            .into_iter()
            .filter(|entry| !model_is_image(entry))
            .collect();
        assert_eq!(candidates.len(), 1);
        assert_eq!(model_id(&candidates[0]), "gemini-3-flash");
    }
}
