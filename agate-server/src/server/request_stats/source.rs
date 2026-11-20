//! 一次请求的「来源」：谁发来的（客户端）、用哪把 Key、以及识别证据。
//!
//! ── 为什么单独一个文件 ──────────────────────────────────────
//! 这三个事实要一起流动：请求日志要落它们，统计要按它们分组，容量准入要拿
//! `filter_value()` 当调用方标识。散在三个结构里会让「加一个来源维度」变成
//! 改三处签名。
//!
//! ── `behavior_client` 为什么不参与序列化 ────────────────────
//! 它是**严格行为识别**的结果（`core::client::classify`），只给协议兼容用。
//! 落库的 `client` 是显示识别的结果 —— 两者可能不同（用户给客户端改了名），
//! 而「协议按哪个客户端分支」不能由用户配置决定。所以行为值标 `skip`：
//! 它随请求活着，但不进日志、不进报表。

use serde::{Deserialize, Serialize};

/// 命中哪一把网关 Key。
///
/// `source` 三档：`key`（配置里的一条记录）/ `env`（环境变量注入）/
/// `none`（免鉴权模式）。`filter_value()` 是它在筛选与分组里的**规范写法**
/// —— 界面上一串 Key 名要让用户能区分「同名不同 id」，所以带 id 前缀。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApiKeyIdentity {
    #[serde(rename = "apiKeyId", default)]
    pub id: String,
    #[serde(rename = "apiKeyName", default)]
    pub name: String,
    #[serde(rename = "apiKeySource", default)]
    pub source: String,
}

impl ApiKeyIdentity {
    /// 免鉴权模式（没有 Key 挡在前面）。
    pub fn none() -> Self {
        Self {
            source: "none".to_string(),
            ..Default::default()
        }
    }

    /// 环境变量注入的那把（有 Key 但不在配置表里）。
    pub fn env() -> Self {
        Self {
            source: "env".to_string(),
            ..Default::default()
        }
    }

    /// 配置里的一条记录。
    pub fn key(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source: "key".to_string(),
        }
    }

    /// 筛选/分组用的规范值（`key:<id>` / `env` / `none` / `unknown`）。
    pub fn filter_value(&self) -> String {
        match self.source.as_str() {
            "key" => format!("key:{}", self.id),
            "env" | "none" => self.source.clone(),
            _ => "unknown".to_string(),
        }
    }

    /// 反解 `filter_value()`。空 id 的 `key:` 视为不合法（返回 None）。
    pub fn from_filter(value: &str) -> Option<Self> {
        let value = value.trim();
        let (source, id) = match value {
            "env" | "none" => (value, ""),
            "unknown" => ("", ""),
            _ => (
                "key",
                value.strip_prefix("key:").filter(|id| !id.is_empty())?,
            ),
        };
        Some(Self {
            id: id.to_string(),
            name: String::new(),
            source: source.to_string(),
        })
    }
}

/// 一次请求的来源快照。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RequestSource {
    /// 显示识别的结果（落库的 `client` 列）。
    #[serde(default)]
    pub client: String,
    #[serde(flatten)]
    pub api_key: ApiKeyIdentity,
    /// 识别证据（UA + 身份头）。`None` = 内部调用（模型测试 / 签到保活等），
    /// 那些请求不来自某个真实客户端。
    #[serde(
        rename = "clientEvidence",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub evidence: Option<crate::server::core::client::ClientEvidence>,
    /// 严格行为识别的结果：协议兼容只认它，落库/报表都不用它。
    #[serde(skip)]
    pub behavior_client: String,
}

impl RequestSource {
    /// 内部调用（不来自真实客户端）。
    ///
    /// 判据是「没有证据 **且** 客户端名在内部名单里」——只看名字会把
    /// 恰好叫 `model_test` 的真实客户端误判，只看证据缺失又会把「没带任何头
    /// 的裸请求」算成内部调用。
    pub fn is_internal(&self) -> bool {
        self.evidence.is_none()
            && matches!(
                self.client.as_str(),
                "model_test"
                    | "assistant"
                    | "assistant_images"
                    | "assistant_title"
                    | "checkin_keepalive"
            )
    }

    /// 证据的序列化形态（落库用）。无证据给 `"{}"`。
    pub fn evidence_json(&self) -> String {
        self.evidence
            .as_ref()
            .map(|evidence| evidence.encoded())
            .unwrap_or_else(|| "{}".to_string())
    }

    /// 落库时那一份展示名（识别规则后来改名/删除时，历史记录仍显示当时的名字）。
    pub fn recorded_client_name(&self) -> &str {
        self.evidence
            .as_ref()
            .map(|evidence| evidence.client_name.as_str())
            .unwrap_or("")
    }

    /// 去掉首尾空白（构造后统一归一一次，避免「同名两个客户端」）。
    pub fn normalized(mut self) -> Self {
        self.client = self.client.trim().to_string();
        self.evidence = self.evidence.map(|evidence| evidence.normalized());
        self.api_key.id = self.api_key.id.trim().to_string();
        self.api_key.name = self.api_key.name.trim().to_string();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_value_round_trips() {
        let identity = ApiKeyIdentity::key("k1", "面板");
        assert_eq!(identity.filter_value(), "key:k1");
        let back = ApiKeyIdentity::from_filter("key:k1").expect("反解");
        assert_eq!(back.id, "k1");
        assert_eq!(back.source, "key");
        // 空 id 的 `key:` 不合法：界面上不可能产生它，出现了说明调用方拼错了
        assert!(ApiKeyIdentity::from_filter("key:").is_none());
        assert_eq!(ApiKeyIdentity::env().filter_value(), "env");
        assert_eq!(ApiKeyIdentity::none().filter_value(), "none");
        assert_eq!(ApiKeyIdentity::default().filter_value(), "unknown");
    }

    #[test]
    fn internal_requires_both_no_evidence_and_known_name() {
        let mut source = RequestSource {
            client: "model_test".to_string(),
            ..Default::default()
        };
        assert!(source.is_internal());
        // 有证据就不是内部调用（真实客户端恰好叫这个名字也不会误判）
        source.evidence = Some(Default::default());
        assert!(!source.is_internal());
    }
}
