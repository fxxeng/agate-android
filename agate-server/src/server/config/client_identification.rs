//! 客户端识别的可配置部分（`clientIdentification`）：用户自定义规则与自定义客户端。
//!
//! ── 与 `core::client` 的分界 ────────────────────────────────
//! `core::client::classify` 是**严格行为识别**（固定前缀表，协议兼容只认它）；
//! 本模块给的是**显示识别**的配置：额外规则、自定义客户端名、调用方声明头。
//! 两者共用同一份固定目录，没配规则时 `identify` 退化成 `classify`。
//!
//! ── 为什么缓存而不是每次从 raw 解析 ──────────────────────────
//! `identify` 在**每个请求**上都要跑一次（请求来源要落库）。`routing_profiles`
//! 那种「每次从 raw 解析」的做法在低频项上是对的，这里不行 —— 那等于每请求
//! 克隆一次整份配置 Map。
//!
//! 所以这里用「懒加载快照 + 写时刷新」：第一次读时从 `raw` 算一份并缓存，
//! `save` 成功后立刻换掉缓存。热路径只剩一次 `RwLock` 读 + `Arc` 自增。
//! 启动期不需要显式初始化（第一次读会自己算），因此不存在「忘了调 init」的
//! 失效面 —— 这正是选懒加载而不是 bootstrap 挂钩子的理由。
//!
//! ── 解析失败不等于崩溃 ──────────────────────────────────────
//! `settings` 是 `Result`：配置写坏时保留**错误文案**而不是回退默认值。
//! 理由与上游一致 —— 静默回退会让用户以为「我的规则没生效」，而实际是格式错；
//! 界面拿到 `settingsError` 能直接告诉他是哪一条不对。

use std::sync::{Arc, RwLock};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::client::{catalog, ClientSettings};
use crate::server::errors::GatewayError;

pub const KEY_CLIENT_IDENTIFICATION: &str = "clientIdentification";

/// 当前生效的识别配置 + 它的修订号。
#[derive(Clone, Debug)]
pub struct ClientIdentificationSnapshot {
    /// 解析结果。`Err` 是给人看的文案（配置格式错 / 校验不过）。
    pub settings: Result<ClientSettings, String>,
    /// 配置原文的哈希：客户端保存时回传，用来发现「别处改过了」。
    pub revision: String,
}

impl Default for ClientIdentificationSnapshot {
    fn default() -> Self {
        from_value(None)
    }
}

fn revision_of(value: Option<&Value>) -> String {
    let text = value.cloned().unwrap_or(Value::Null).to_string();
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn from_value(value: Option<&Value>) -> ClientIdentificationSnapshot {
    let settings = match value {
        None => Ok(ClientSettings::default()),
        Some(value) => serde_json::from_value::<ClientSettings>(value.clone())
            .map_err(|error| format!("客户端识别配置格式不正确：{error}"))
            .and_then(|settings| {
                catalog::validate(&settings)?;
                Ok(settings)
            }),
    };
    ClientIdentificationSnapshot {
        settings,
        revision: revision_of(value),
    }
}

/// 进程级快照（懒加载，见模块头）。
static SNAPSHOT: RwLock<Option<Arc<ClientIdentificationSnapshot>>> = RwLock::new(None);

/// 当前生效的识别配置。
pub fn current() -> Arc<ClientIdentificationSnapshot> {
    if let Ok(guard) = SNAPSHOT.read() {
        if let Some(snapshot) = guard.as_ref() {
            return snapshot.clone();
        }
    }
    // 未初始化（或锁中毒）：从 raw 现算一份并缓存。锁中毒时不 panic，直接
    // 返回算好的值 —— 配置读取失败不该让整个转发链停下（与 config 层同一取向）。
    let snapshot = Arc::new(from_value(
        crate::server::config::current()
            .raw()
            .get(KEY_CLIENT_IDENTIFICATION),
    ));
    if let Ok(mut guard) = SNAPSHOT.write() {
        *guard = Some(snapshot.clone());
    }
    snapshot
}

/// 保存（带修订号校验），成功后立刻刷新快照。
///
/// 校验顺序：先 `validate`（形状与约束），再比对修订号（防并发覆盖）。
/// 两步都过了才写盘 —— 写盘失败时 `try_update_raw_field` 会把错误透传出来，
/// 且内存快照不动（下次 `current()` 读到的还是旧的）。
pub fn save(settings: ClientSettings, expected: &str) -> Result<(), GatewayError> {
    catalog::validate(&settings).map_err(GatewayError::bad_request)?;
    let value = serde_json::to_value(&settings)
        .map_err(|error| GatewayError::new(format!("客户端识别设置无法序列化：{error}")))?;
    crate::server::config::try_update_raw_field(
        KEY_CLIENT_IDENTIFICATION,
        |raw| {
            if revision_of(Some(raw)) != expected {
                return Err(GatewayError::with_status(
                    409,
                    "客户端识别设置已在其他位置修改，请重新读取后再保存；当前草稿未丢失",
                ));
            }
            *raw = value;
            Ok(())
        },
        || GatewayError::new("客户端识别设置保存失败，未应用本次修改"),
    )?;
    // 写盘成功：快照换成新的。这里**重算**而不是用 `settings` 直接构造 ——
    // 落盘的值与内存生效的值必须来自同一处解析，否则「保存成功但行为没变」
    // 会变成一条查不到的差异。
    let next = Arc::new(from_value(Some(
        &crate::server::config::current().raw()[KEY_CLIENT_IDENTIFICATION],
    )));
    if let Ok(mut guard) = SNAPSHOT.write() {
        *guard = Some(next);
    }
    Ok(())
}

/// 配置视图（`GET /api/client-identification` 的响应体）。
pub fn view(snapshot: &ClientIdentificationSnapshot) -> Value {
    let mut out = serde_json::Map::new();
    match &snapshot.settings {
        Ok(settings) => {
            out.insert(
                "settings".to_string(),
                serde_json::to_value(settings).unwrap_or(Value::Null),
            );
        }
        Err(error) => {
            out.insert("settings".to_string(), Value::Null);
            out.insert("settingsError".to_string(), Value::String(error.clone()));
        }
    }
    out.insert(
        "defaults".to_string(),
        serde_json::to_value(ClientSettings::default()).unwrap_or(Value::Null),
    );
    out.insert(
        "revision".to_string(),
        Value::String(snapshot.revision.clone()),
    );
    out.insert(
        "catalog".to_string(),
        serde_json::to_value(&catalog::CLIENTS).unwrap_or(Value::Null),
    );
    Value::Object(out)
}

/// 供测试与排障：从一份 raw 底稿构造快照（不碰进程缓存）。
pub fn snapshot_from_raw(raw: &Map<String, Value>) -> ClientIdentificationSnapshot {
    from_value(raw.get(KEY_CLIENT_IDENTIFICATION))
}
