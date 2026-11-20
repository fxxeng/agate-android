//! 辅助识图结果的进程内缓存：同一张图 + 同一条配置在 TTL 内只识读一次。
//!
//! 上游 `vision_compat/cache.rs` 原文移植（纯 std + sha2，无桌面依赖）。
//! 键由调用方用 `key(parts)` 拼好（各段带长度前缀，避免拼接歧义）；值有
//! TTL / 条数 / 总字节三道上界，避免长期运行内存泄漏。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

struct Entry {
    text: String,
    created: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
const TTL: Duration = Duration::from_secs(900);
const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn key(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn get(key: &str) -> Option<String> {
    let mut entries = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    entries.retain(|_, entry| entry.created.elapsed() < TTL);
    entries.get(key).map(|entry| entry.text.clone())
}

pub(super) fn put(key: String, text: String) {
    if text.is_empty() || text.len() > 65_536 {
        return;
    }
    let mut entries = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    entries.retain(|_, entry| entry.created.elapsed() < TTL);
    while entries.len() >= MAX_ENTRIES
        || entries
            .values()
            .map(|entry| entry.text.len())
            .sum::<usize>()
            + text.len()
            > MAX_BYTES
    {
        let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.created)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        entries.remove(&oldest);
    }
    entries.insert(
        key,
        Entry {
            text,
            created: Instant::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_length_prefixed_and_stable() {
        // 两段拼接可能撞车（"ab"+"c" vs "a"+"bc"），长度前缀把它分开。
        assert_ne!(key(&["ab", "c"]), key(&["a", "bc"]));
        assert_eq!(key(&["x", "y"]), key(&["x", "y"]));
    }

    #[test]
    fn put_get_and_size_bound() {
        let k = key(&["test", "put_get"]);
        put(k.clone(), "描述".to_string());
        assert_eq!(get(&k).as_deref(), Some("描述"));
        // 超长文本不入库
        let big = key(&["test", "big"]);
        put(big.clone(), "x".repeat(65_537));
        assert_eq!(get(&big), None);
    }
}
