//! DeepSeek 网页版的**工作量证明**（PoW）：算法、求解、请求头编码。
//!
//! ── 为什么本家必须自己做 PoW（不是可选优化）──────────────────────
//! `chat.deepseek.com` 的对话接口有一道反自动化关卡：每次 `chat/completion`
//! 之前必须先 `create_pow_challenge` 领一道题，在**本地**算出答案放进
//! `x-ds-pow-response` 头，否则请求被拒。这道题必须在前端就能算出来
//! （浏览器要能算），所以算法是公开的、可复现的 —— 本文件就是它的复刻。
//!
//! ── 算法：DeepSeekHashV1 = SHA3-256「少跑一轮」（本文件的核心事实）──
//! 它是一个 **Keccak-f\[1600\] 海绵**，参数与标准 SHA3-256 完全一致：
//! ```text
//!   rate = 136 字节（capacity 512 位）、输出 256 位、
//!   域字节 0x06 + 收尾 0x80、lane 小端打包
//! ```
//! **唯一的差异**：置换只跑 **23 轮**，且轮常数从 `RC[1]` 开始用
//! （跳过 `RC[0]`）—— 等价于「SHA3-256 少跑一轮」。
//!
//! ── 为什么不能用现成的 `sha2` crate ───────────────────────────
//! `sha2::Sha3_256` 把轮数写死在实现里（恒定 24 轮），既不暴露轮数也不暴露
//! 置换函数，因此拿不到「23 轮」这个变体。本文件手写 Keccak-f（下面约 80 行），
//! 而不是去 fork/patch 依赖 —— 置换本身很短，且**可被自检向量逐字验证**
//! （见下），自持一份的风险远低于改依赖。
//!
//! ── 离线自验：两条自检向量（本文件的回归护栏）──────────────────
//! 官方网页版的 wasm 模块给出的值，逐字节一致：
//! ```text
//!   H("")    = e594808bc5b7151ac160c6d39a02e0a8e261ed588578403099e3561dc40c26b3
//!   H("abc") = f841106c601ce9be9bc38525e90d4178d47f21dd8eb9f238fc55ffaa4ca94506
//! ```
//! 这两条是**本文件唯一能证明「算法没写错」的手段**：没有真实 userToken 就
//! 无法联调上游，但只要有这两条向量，算法正确性就是确定的（`tests` 模块跑它们）。
//! 改动 `permute` / `absorb` / 填充中的任何一处，测试立刻红。
//!
//! ── PoW 协议（题面 → 求解 → 请求头）────────────────────────────
//! ```text
//!   1. 领题  POST /api/v0/chat/create_pow_challenge
//!            body: {"target_path": "/api/v0/chat/completion"}
//!            → data.biz_data.challenge = { algorithm, challenge, salt,
//!                                          difficulty, expire_at, signature,
//!                                          target_path }
//!   2. 求解  prefix = salt + "_" + expire_at + "_"
//!            在 [0, difficulty) 内找**最小**的 answer，
//!            使 DeepSeekHashV1(prefix + answer) == challenge（hex 相等）
//!   3. 组装  base64(紧凑 JSON { algorithm, challenge, salt, answer,
//!                              signature, target_path })
//!            → 放进 x-ds-pow-response 头
//! ```
//! ⚠️ `prefix` 里的 `expire_at` **按原样（毫秒整数）拼**，不做任何单位换算；
//! `answer` 以十进制字符串拼在末尾。两处口径错一个字节，算出来的哈希就永远
//! 对不上题面（表现为「在 [0,difficulty) 内找不到 answer」），而这条报错本身
//! 不会告诉你拼错了 —— 所以这里逐字写下。
//!
//! ── 求解耗时是**真实成本**（不是可忽略的开销）────────────────────
//! `difficulty` 实测约 14 万~26 万，是**搜索上界**（最坏要试这么多轮哈希），
//! 每轮一次 23 轮 Keccak。实测 `difficulty=262140` 约 150ms。因此求解必须在
//! 转发路径上**同步**完成（上游要求每次 completion 都带一份**新鲜**的题解，
//! 挑战带随机 salt 且一次性消费，不能复用、不能预取缓存）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

// ── Keccak-f[1600] 常量（与标准 SHA3-256 逐字相同）──────────────

/// 轮常数。本家**从 `RC[1]` 开始用**（跳过 `RC[0]`），这是算法的全部差异。
const RC: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808a,
    0x8000000080008000,
    0x000000000000808b,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008a,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000a,
    0x000000008000808b,
    0x800000000000008b,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800a,
    0x800000008000000a,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

/// 本家跑 23 轮（标准 SHA3-256 是 24 轮）
const ROUNDS: usize = 23;

/// 海绵速率（字节）；136 = 1600/8 - 256/8，即 capacity 512 位
const RATE: usize = 136;

/// 输出字节数（256 位）
const OUT_LEN: usize = 32;

/// 25 个 lane 的旋转偏移（Keccak 规范的 rho 步；按 x + 5y 索引）
const ROTATION: [u32; 25] = [
    0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
];

/// Keccak-f[1600] 置换（本家跑 `ROUNDS` 轮，从 `RC[1]` 起）。
///
/// 状态是 25 个 u64 lane；标准写法，theta/rho/pi/chi/iota 五步。
fn permute(state: &mut [u64; 25]) {
    for round in 1..=ROUNDS {
        // theta：列奇偶
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
        }
        for x in 0..5 {
            for y in 0..5 {
                state[x + 5 * y] ^= d[x];
            }
        }

        // rho + pi：先旋转再按 pi 换位（用临时数组，避免就地换位互相覆盖）
        let mut rotated = [0u64; 25];
        for y in 0..5 {
            for x in 0..5 {
                rotated[y + 5 * ((2 * x + 3 * y) % 5)] = state[x + 5 * y].rotate_left(ROTATION[x + 5 * y]);
            }
        }

        // chi：逐行非线性
        for y in 0..5 {
            let base = 5 * y;
            let row = [
                rotated[base],
                rotated[base + 1],
                rotated[base + 2],
                rotated[base + 3],
                rotated[base + 4],
            ];
            for x in 0..5 {
                state[base + x] = row[x] ^ ((!row[(x + 1) % 5]) & row[(x + 2) % 5]);
            }
        }

        // iota：唯一直和轮常数打交道的 lane
        state[0] ^= RC[round];
    }
}

/// `DeepSeekHashV1`：23 轮 Keccak-f 海绵，输出 32 字节。
///
/// 吸收 → 填充（0x06 …… 0x80）→ 挤压。填充与标准 SHA3-256 逐字相同：
/// 域字节 `0x06` 落在消息末尾，`0x80` 落在**块末**（注意二者可能**同一字节**：
/// 当消息长度 % RATE == RATE-1 时，`0x06 | 0x80 = 0x86` 要合并成一字节 ——
/// 少写这一步会让这一类长度的输入全错，而自检向量的两条恰好都不是这种长度，
/// 所以下面的单测额外补了一条边界用例）。
pub fn hash(message: &[u8]) -> [u8; OUT_LEN] {
    let mut state = [0u64; 25];

    // 吸收：整块直接异或进 lane（小端）
    let mut chunks = message.chunks_exact(RATE);
    for chunk in &mut chunks {
        absorb_block(&mut state, chunk);
        permute(&mut state);
    }
    // 尾部不足一块：拷贝进零缓冲，就地补填充
    let mut block = [0u8; RATE];
    let tail = chunks.remainder();
    block[..tail.len()].copy_from_slice(tail);
    let pad_index = tail.len();
    if pad_index == RATE - 1 {
        // 域字节与收尾字节落在同一格
        block[pad_index] |= 0x86;
    } else {
        block[pad_index] ^= 0x06;
        block[RATE - 1] ^= 0x80;
    }
    absorb_block(&mut state, &block);
    permute(&mut state);

    // 挤压：32 字节 < RATE，一轮就够
    let mut out = [0u8; OUT_LEN];
    for (index, slot) in out.iter_mut().enumerate() {
        let lane = state[index / 8];
        *slot = ((lane >> (8 * (index % 8))) & 0xff) as u8;
    }
    out
}

/// 把一整块（`RATE` 字节）异或进状态（lane 小端打包）
fn absorb_block(state: &mut [u64; 25], block: &[u8]) {
    for (index, lane) in state.iter_mut().enumerate().take(RATE / 8) {
        let start = index * 8;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&block[start..start + 8]);
        *lane ^= u64::from_le_bytes(bytes);
    }
}

/// 十六进制小写（题面里的 `challenge` 是 hex 串，比对时两边都转成 hex）
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `DeepSeekHashV1(prefix + answer)` 的十六进制形态（求解时逐轮调用）
pub fn hex_with_answer(prefix: &str, answer: u64) -> String {
    let material = format!("{prefix}{answer}");
    hex(&hash(material.as_bytes()))
}

/// 求解结果
#[derive(Clone, Debug)]
pub struct Solved {
    /// 找到的 answer（拼进请求头的那个整数）
    pub answer: u64,
    /// 题面原样回传的字段（组装请求头要用）
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub signature: String,
    pub target_path: String,
}

/// 从题面 JSON 里取出一道题的字段。
///
/// 信封是 `data.biz_data.challenge`（见模块头）；字段缺失 / 类型不对就地报错，
/// 而不是让求解阶段拿到一串空值后「找不到 answer」（那句话看不出是题面残缺）。
pub fn parse_challenge(payload: &Value) -> Result<Challenge, String> {
    let challenge = payload
        .get("data")
        .and_then(|item| item.get("biz_data"))
        .and_then(|item| item.get("challenge"))
        .or_else(|| {
            // 兼容：部分路径把 challenge 直接放在 biz_data 下
            payload
                .get("data")
                .and_then(|item| item.get("biz_data"))
                .filter(|item| item.get("challenge").is_some())
        });
    let challenge = challenge.ok_or_else(|| "PoW 题面缺失（data.biz_data.challenge）".to_string())?;

    let text = |key: &str| -> Result<String, String> {
        challenge
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("PoW 题面缺少字段 {key}"))
    };
    let difficulty = challenge
        .get("difficulty")
        .and_then(Value::as_u64)
        .ok_or_else(|| "PoW 题面缺少 difficulty".to_string())?;
    // expire_at 兼容整数与字符串（见模块头：原样拼接，不做单位换算）
    let expire_at = match challenge.get("expire_at") {
        Some(Value::Number(number)) => number
            .as_u64()
            .map(|value| value.to_string())
            .or_else(|| number.as_f64().map(|value| format!("{value:.0}")))
            .ok_or_else(|| "PoW 题面的 expire_at 不是有效数字".to_string())?,
        Some(Value::String(text)) => text.clone(),
        _ => return Err("PoW 题面缺少 expire_at".to_string()),
    };

    Ok(Challenge {
        algorithm: text("algorithm").unwrap_or_else(|_| "DeepSeekHashV1".to_string()),
        challenge: text("challenge")?,
        salt: text("salt")?,
        difficulty,
        expire_at,
        signature: text("signature").unwrap_or_default(),
        target_path: text("target_path").unwrap_or_else(|_| "/api/v0/chat/completion".to_string()),
    })
}

/// 一道题（`parse_challenge` 的产物）
#[derive(Clone, Debug)]
pub struct Challenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub difficulty: u64,
    /// 原样字符串（见模块头：不复原成整数再格式化，避免丢进位）
    pub expire_at: String,
    pub signature: String,
    pub target_path: String,
}

impl Challenge {
    /// 题面前缀：`salt + "_" + expire_at + "_"`（末尾下划线不能少）
    pub fn prefix(&self) -> String {
        format!("{}_{}_", self.salt, self.expire_at)
    }

    /// 求解：在 `[0, difficulty)` 内找最小的 `answer`，使哈希等于题面的
    /// `challenge`（hex 相等）。找不到返回 Err（题面过期 / 拼接口径错的可能）。
    ///
    /// 逐轮同步计算（见模块头「求解耗时是真实成本」）；`difficulty` 是搜索上界。
    pub fn solve(&self) -> Result<Solved, String> {
        let prefix = self.prefix();
        for answer in 0..self.difficulty {
            if hex_with_answer(&prefix, answer) == self.challenge {
                return Ok(Solved {
                    answer,
                    algorithm: self.algorithm.clone(),
                    challenge: self.challenge.clone(),
                    salt: self.salt.clone(),
                    signature: self.signature.clone(),
                    target_path: self.target_path.clone(),
                });
            }
        }
        Err(format!(
            "PoW 求解失败：在 [0,{}) 内没有满足条件的 answer（题面可能已过期，请重试）",
            self.difficulty
        ))
    }
}

/// 组装 `x-ds-pow-response` 头的值：`base64(紧凑 JSON)`。
///
/// 字段顺序按题面原样（`algorithm, challenge, salt, answer, signature,
/// target_path`）—— 上游按 JSON 解析，字段顺序理论无关，但保持与参考实现逐字
/// 一致便于日后对照抓包。
pub fn response_header(solved: &Solved) -> String {
    use base64::Engine;
    let payload = serde_json::json!({
        "algorithm": solved.algorithm,
        "challenge": solved.challenge,
        "salt": solved.salt,
        "answer": solved.answer,
        "signature": solved.signature,
        "target_path": solved.target_path,
    });
    // 紧凑序列化（无多余空格）后 base64 标准编码
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自检向量：官方 wasm 模块逐字节一致（见模块头）。
    /// **本测试是算法正确性的唯一凭据** —— 改坏任何一处立刻红。
    #[test]
    fn self_check_vectors_match_official_wasm() {
        assert_eq!(
            hex(&hash(b"")),
            "e594808bc5b7151ac160c6d39a02e0a8e261ed588578403099e3561dc40c26b3"
        );
        assert_eq!(
            hex(&hash(b"abc")),
            "f841106c601ce9be9bc38525e90d4178d47f21dd8eb9f238fc55ffaa4ca94506"
        );
    }

    /// 边界：消息长度 % RATE == RATE-1 时，域字节 0x06 与收尾 0x80 落在同一格
    /// （合并成 0x86）。这条单独测，因为两条自检向量都不是这种长度 ——
    /// 它保证「合并填充」那一支不是死代码。
    #[test]
    fn padding_merge_branch_is_reachable() {
        // 135 字节：135 % 136 == 135 == RATE-1
        let input = vec![0x41u8; 135];
        let digest = hash(&input);
        // 与标准 SHA3-256 的差异是轮数，所以两者必然不同；这里只锁住
        // 「同输入两次一致」且「不等于 134 / 136 字节输入的结果」
        assert_eq!(digest, hash(&input.clone()));
        assert_ne!(digest, hash(&input[..134]));
        assert_ne!(digest, hash(&vec![0x41u8; 136]));
    }

    /// 与标准 SHA3-256 的差异必须存在（否则说明轮数参数被改回了 24：
    /// 那会「自检向量以外的输入全都算错」，是最危险的静默回归）。
    #[test]
    fn differs_from_standard_sha3_256() {
        // "abc" 的标准 SHA3-256 是 3a985da7...，本家应是 f841106c...
        // 两者不同即证明「少跑一轮」这个改动确实生效
        assert_ne!(
            hex(&hash(b"abc")),
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"
        );
    }

    /// 题面解析：信封 + 字段 + expire_at 的两种形态
    #[test]
    fn parses_challenge_envelope() {
        let payload = serde_json::json!({
            "code": 0,
            "data": { "biz_data": { "challenge": {
                "algorithm": "DeepSeekHashV1",
                "challenge": "abc123",
                "salt": "s4lt",
                "difficulty": 144000,
                "expire_at": 1680000000000i64,
                "signature": "sig",
                "target_path": "/api/v0/chat/completion"
            }}}
        });
        let challenge = parse_challenge(&payload).expect("题面应能解析");
        assert_eq!(challenge.salt, "s4lt");
        assert_eq!(challenge.difficulty, 144000);
        assert_eq!(challenge.prefix(), "s4lt_1680000000000_");
        assert_eq!(challenge.target_path, "/api/v0/chat/completion");
    }

    /// expire_at 是字符串时原样使用（不做数字往返，避免精度/格式漂移）
    #[test]
    fn expire_at_string_is_used_verbatim() {
        let payload = serde_json::json!({
            "data": { "biz_data": { "challenge": {
                "challenge": "x", "salt": "s", "difficulty": 10,
                "expire_at": "1680000000000", "signature": "", "algorithm": "DeepSeekHashV1"
            }}}
        });
        let challenge = parse_challenge(&payload).expect("题面应能解析");
        assert_eq!(challenge.prefix(), "s_1680000000000_");
    }

    /// 求得的 answer 必须能反算回题面（自洽验证，不需要上游）
    #[test]
    fn solved_answer_reproduces_challenge() {
        // 造一道自己出得起的题：假定 answer = 7 是正解
        let prefix = "test_1700000000000_";
        let expected = hex_with_answer(prefix, 7);
        let challenge = Challenge {
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: expected,
            salt: "test".to_string(),
            difficulty: 1000,
            expire_at: "1700000000000".to_string(),
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };
        let solved = challenge.solve().expect("应当能解出 answer=7");
        assert_eq!(solved.answer, 7);
    }

    /// 无解时给出可读报错（而不是 panic / 空结果）
    #[test]
    fn unsolvable_challenge_reports_error() {
        let challenge = Challenge {
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: "deadbeef".to_string(), // 不可能命中的题面
            salt: "test".to_string(),
            difficulty: 64, // 小上界，测试跑得快
            expire_at: "1700000000000".to_string(),
            signature: String::new(),
            target_path: "/api/v0/chat/completion".to_string(),
        };
        assert!(challenge.solve().is_err());
    }

    /// 请求头是 base64，解回来字段与题面一致
    #[test]
    fn header_roundtrips_to_compact_json() {
        use base64::Engine;
        let solved = Solved {
            answer: 42,
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: "abc".to_string(),
            salt: "s".to_string(),
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };
        let header = response_header(&solved);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(header.as_bytes())
            .expect("应是合法 base64");
        let text = String::from_utf8(decoded).expect("应是 UTF-8");
        let value: Value = serde_json::from_str(&text).expect("应是合法 JSON");
        assert_eq!(value.get("answer").and_then(Value::as_u64), Some(42));
        assert_eq!(value.get("salt").and_then(Value::as_str), Some("s"));
        // 紧凑：不含空格
        assert!(!text.contains(": "));
    }
}