//! Agate 侧的 `plugin_host` 最小适配层（**不是**插件体系）。
//!
//! ── 为什么有这个文件 ────────────────────────────────────────────
//! 上游 Agent2API 把「某家提供商申报了什么能力」交给插件宿主
//! （`core/plugin_host/`，34.7 KB / 124 个文件）托管。移植路由模块时，
//! `core/account_order.rs` 与 `core/routing_scope.rs` 各有一处引用它：
//!
//!   1. `account_order.rs`：`account_features_of(provider).map(|f| f.credit_expiry)`
//!   2. `routing_scope.rs`：`installed_ids()`
//!
//! Agate 已明确不引入插件架构（provider 全部内置、能力硬编码），因此这里
//! 只补这两个符号的最小实现，**不复制上游 34.7 KB 的插件宿主**。
//!
//! ── 行为等价性说明 ──────────────────────────────────────────────
//! 上游 `account_features_of` 的调用点带着兜底：
//!
//! ```ignore
//! account_features_of(provider).map(|f| f.credit_expiry)
//!     .unwrap_or_else(|| matches!(provider, "workbuddy" | "workbuddy-intl" | "trae" | "officeace"))
//! ```
//!
//! 也就是说「插件没申报」时本来就回落到内置名单。Agate 没有插件，
//! 本实现返回 `None`，调用点自动走同一份兜底 —— 与上游在无插件环境下的
//! 行为完全一致，不是简化也不是近似。
//!
//! `installed_ids()` 同理：上游用它把「已安装但还没有账号」的 provider
//! 补进可用列表；Agate 的 provider 全部随二进制内置、由账号与自定义提供商
//! 两条路径覆盖，故返回空集，不产生任何新增项。

/// 账号行参数（对应上游 `proto::AccountFeatures` 的最小子集）。
///
/// Agate 只用到 `credit_expiry` 一个字段，因此只声明它。
/// 上游该字段是 `bool`（调用点 `.map(|features| features.credit_expiry)`
/// 之后直接与布尔逻辑组合）。
pub struct AccountFeatures {
    /// 该提供商是否支持「按额度到期时间」排序。
    pub credit_expiry: bool,
}

/// 某一家申报的账号行参数（没申报 = `None`）。
///
/// Agate 无插件体系，恒返回 `None`，由调用点的内置兜底接管。
pub fn account_features_of(_provider: &str) -> Option<AccountFeatures> {
    None
}

/// 已安装插件的 provider id 列表。
///
/// Agate 不引入插件，恒为空。
pub fn installed_ids() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_plugin_declared_features() {
        assert!(account_features_of("workbuddy").is_none());
        assert!(account_features_of("unknown-provider").is_none());
    }

    #[test]
    fn no_installed_plugin_ids() {
        assert!(installed_ids().is_empty());
    }
}
