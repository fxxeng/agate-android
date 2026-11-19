//! DeepSeek 网页版（`chat.deepseek.com`）适配实现：**无状态转发 + path-value SSE
//! + 每次请求现解 PoW**。
//!
//! ── 上游长什么样（来源：web 插件包二进制 + 公开逆向的交叉印证）────
//! 本家是**网页版对话站**，不是开放平台那条 `api.deepseek.com`（那是另一套
//! 计费的 API Key 接口，本目录完全不碰）。整条链路四步，缺一步都发不出去：
//! ```text
//!   1. POST /api/v0/chat/create_pow_challenge   领一道 PoW 题
//!   2. 本地求解（23 轮 Keccak 海绵，见 pow.rs）
//!   3. POST /api/v0/chat_session/create         建会话（拿 data.biz_data.id）
//!   4. POST /api/v0/chat/completion             带 x-ds-pow-response 发问（SSE）
//! ```
//! 认证是 `Authorization: Bearer <userToken>`（浏览器 localStorage 里的那一串，
//! **不是 Cookie** —— 这条最容易走偏，见 `credentials.rs` 的模块头）。
//!
//! ── 为什么能「不用 WASM、不用真 Cookie」就把 PoW 做出来（本目录最硬的一块）──
//! 网页版的 PoW 必须在前端就能算（浏览器要能算），所以算法是公开的、可复现的。
//! 公开逆向（多份独立实现）一致给出结论：
//! ```text
//!   DeepSeekHashV1 = SHA3-256 但 Keccak-f[1600] 只跑 23 轮（跳过 RC[0]）
//! ```
//! 这条结论有**官方的自检向量**（网页版 wasm 模块的输出），因此算法正确性
//! **可以离线证明、不需要任何真实凭证**：`pow.rs` 的单测跑那两条向量。
//! 本机不引 WASM 运行时、不复刻前端那套 wasm-bindgen 调用约定 —— 纯 Rust
//! 手写 Keccak-f 反而更短（约 80 行）且可被向量逐字验证。
//!
//! ── 流为什么另开一台状态机（不复用 ChatGLM 那台）────────────────
//! 本家的 SSE 是 **path-value patch 结构**（`{"p":路径,"o":操作,"v":值}`，
//! 正文主体是不带 `p` 的裸增量），与 ChatGLM 的 `parts[]` 结构、OpenAI 的
//! `choices[].delta` 都不同。⚠️ 特别注意：ChatGLM 那台需要**差分**（增量与
//! 全文快照混发），本家**不做任何差分**（裸增量就是净增量，从不发全文快照）
//! —— 把差分照搬过来会把每个字都判成「与已发内容对不上」而全部丢弃。
//! 详见 `protocol::deepseek_stream`。
//!
//! ── 子模块分工 ──────────────────────────────────────────────
//! ```text
//!   pow.rs           DeepSeekHashV1（23 轮 Keccak 海绵）+ 题面解析 + 求解 + 请求头
//!   endpoints.rs     基址 / 路径 / 浏览器常规头 / 鉴权头
//!   credentials.rs   userToken 形态检查 / 脱敏 / 归一化 / 探活
//!   login.rs         粘贴式的一步归一化 + best-effort 探活（本家没有 OAuth）
//!   session.rs       每次请求的两步异步准备（建会话 + 解题）+ 取走即失效的状态表
//!   plan.rs          messages 拍平成单条 prompt（本家网页版只收一条）
//!   models.rs        静态两条清单（本家没有远程目录接口）
//!   chat.rs          领题 / 建会话 / 组装 completion 的异步链路
//!   adapter.rs       ProviderAdapter 实现（本目录的装配层）
//! ```
//!
//! ── 能力对照（照官方文档 provider-docs.json 的能力表逐列）────────
//! | 列 | 本家 | 依据 |
//! |---|---|---|
//! | LLM 请求 | ✓ | 四步链路，`chat/completion` 走 SSE |
//! | Token 自动续期 | ✗ | 上游无 refresh 接口（文档 `refresh: undeclared`） |
//! | 模型列表 | ✓ | 静态两条（网页版只有 flash / pro 两档） |
//! | 余额查询 | ✗ | 文档 `usage: undeclared` |
//! | 签到 | ✗ | 文档 `checkin: undeclared` |
//! | 领取类 | ✗ | 文档 `claims: undeclared` |
//!
//! ── 未联调部分（诚实标注，别当成已验过）──────────────────────
//! 可离线证明的只有 `pow.rs` 的算法（有官方自检向量）。**整条链路没有真实
//! `userToken` 就无法跑通验证**，因此凡是「上游是否接受这个请求体形状」的
//! 判断，依据都是二进制字符串簇 + 多份公开逆向的交叉印证，**未经实测**。
//! 官方文档对此也如实标着「未实测联网调用」。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本目录零 unwrap/expect/panic。

pub mod adapter;
pub mod chat;
pub mod credentials;
pub mod endpoints;
pub mod login;
pub mod models;
pub mod plan;
pub mod pow;
pub mod session;

pub use adapter::{DeepSeekWebAdapter, DEEPSEEK_WEB_ADAPTER};

/// 本家的 provider id（与注册表口径一致）
pub const ID: &str = "deepseek-web";