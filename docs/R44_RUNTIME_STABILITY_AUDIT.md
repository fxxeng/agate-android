# R44 — Android Runtime 运行时稳定性 · 审计与收口（2026-11-07）

基线 `5d6201c662`（R43）。本轮主题：把台账里挂了多轮的「静态风险」逐条核验，真实缺口修掉，设计有意的写成裁定。

## 1. 台账四条「静态风险」核验结论

| # | 台账记录 | 核验 | 结论 |
|---|---|---|---|
| 1 | `stop_kernel()` 的 `child.wait()` 无超时 | `runtime.rs:832` 已有 `timeout(3s, wait)` + `kill_on_drop` 兜底 + 注释 | **台账过时，已修，风险关闭** |
| 2 | 注释漂移指向 `backend::shutdown` | `auto_checkin.rs` 停机入口注释确实还写着 `backend::shutdown(&AppState)` 与「Tauri 的应用状态」 | **属实，本轮修正** |
| 3 | `auto_checkin` 任务不 join | `stop()` 注释明确「与 Node clearInterval 对齐，不打断进行中的签到（abort 在下一个 await 点结束，config.json 写入同步安全）」；调用场景是进程退出，join 无意义 | **设计有意，裁定关闭** |
| 4 | `GLOBAL_RUNTIME` 跨 runtime arm | `lib.rs:63` 是有意的兜底运行时（注释：「只承接非 Tokio 上下文发起的后台任务」），主路径不经过它 | **设计有意，裁定关闭** |

## 2. 新发现并收口：`port_conflict.rs` 桌面版残留

Android-only 转向后，该模块残留整段桌面语境：

- 模块头：默认端口 3065、`WSAEADDRINUSE/10048` Windows 错误码、netsh 保留段、
  `backend.rs` Win32 查进程/杀软启发式 —— 全部失真。已重写为 Android 现状
  （Linux `EADDRINUSE`/`EACCES`、判据仍是错误码、occupant 查询已随桌面版移除）。
- `describe()` 的 `Reserved` 文案还在讲 Hyper-V/WSL2/winnat —— Android 上的真实场景是
  特权端口（<1024）或 SELinux 策略。已改为 Android 语义。
- `Occupied` 且无 occupant 时的文案「查不到是哪个进程（可能是权限不足）」—— Android
  壳侧从不填 occupant，用户必然命中这句误导文案。已改为「可结束占用该端口的程序，
  或改用其它端口（设置页）」。
- `Occupant`/`with_occupant`/`can_end_occupant`/`StartupFailure` 是 pub API 但壳侧零调用，
  保留结构不删（R45 代码边界清理再议）。

## 3. 真实缺口修复：端口冲突用户闭环（T1）

**修复前**：Rust 侧 `PortConflict` 已按错误码分类（`ConflictKind::Occupied/Reserved/Other`）
且 `message` 含处置建议，但 JNI 回传只给 `{"ok":false,"stage":"bind","error":message}` ——
**kind 丢失**；Kotlin `RuntimeController.startWith` 有 `GatewayFailed(stage, code, message)`
事件、reducer 折成 `GatewayRuntimeState.Failed`，但**没有任何 UI 消费点**；
`AgateForegroundService.runStart` 通知文案是裸拼 `"stage: error"`。

**修复后**（文案单一出口原则不变 —— 界面不拼说明文案，只透传内核生成的 message）：

1. Rust `jni_bridge.rs`：bind 失败 JSON 增加 `"kind": conflict.kind.as_str()`；
   `ConflictKind::as_str()`（occupied/reserved/other，与 serde kebab-case 一致）。
2. Kotlin `Gateway.StartResult` 解析 `kind`。
3. `GatewayStartFailure`（core-gateway，纯函数）：Kotlin 侧失败文案统一出口 ——
   error 非空原样透传，空时按 stage 兜底。`runStart()` 失败分支改走它（去掉裸 stage 前缀）。
4. `RuntimeStateEvent.GatewayFailed` / `GatewayRuntimeState.Failed` 增加 `kind`
   （默认 null，旧事件兼容），reducer 透传 —— 结构化锚点从此流到 UI。
5. `GatewayControlPage` 新增启动失败卡片（5s 轮询快照，读内存 reducer 状态、不涉 JNI）：
   Failed 时按 kind 显示徽标（端口被占用 / 端口被系统拒绝 / 上次启动失败）+ 内核指引文案。

**测试**（详见 §5）：Rust `port_conflict` 3 条（as_str 契约 + 两类文案的 Android 关键词锚定，
Hyper-V/winnat/WSL2 出现即失败）；Kotlin JVM `GatewayStartFailureTest` 4 条（透传、
兜底分派、kind 常量与 Rust 契约、reserved 文案关键词镜像）。

## 4. 顺手账面（T3）

- **R43 D1 遗留**：`docs/R43_T2_RUST_COMPILE_SCOPE_AUDIT.md` 两处过时表述
  （「决定 A 不改 workflow」「本机无 Rust 工具链」）加【R44 补记】，原文保留存档。
- **M1 遗留**：`workbuddy_import.rs` 的 `CodeBuddyExtension/Data/Public/auth` 固定段
  抽纯函数 `auth_relative_segments()`，Windows/macOS 两个 cfg 分支复用；
  段序 + fold/join 等价性测试全平台可跑（R39 坑：cfg 分支在 Linux CI 编不到，
  共享常量 + 全平台测试是唯一能锁住它的手段）。Linux 非 test 编译下加
  `#[cfg_attr(not(test), allow(dead_code))]`（R40 `resolve_absolute_path` 同款）。

## 5. 本轮验证

- Host（Termux，Rust 1.88）：`cargo check --lib --bins` 通过，lib 警告 **7 条与 R43 基线逐条一致（新增 0）**
  （实施中途 `auth_relative_segments` 曾报第 8 条 never used，加 `#[cfg_attr(not(test), allow(dead_code))]` 后消除）；
  `cargo test --lib`：**779 passed + 11 ignored，0 failed**（R43 基线 774 → +5：port_conflict 3、workbuddy 2）。
- Kotlin JVM（真 junit 4.13.2 + hamcrest 1.3，Termux kotlinc）：`GatewayStartFailureTest` 4/4 OK。
- CI：推送后 Run 结果见台账 §10 回填。
- 设备侧（C 类，并入待验清单）：R43 锁真机语义 4 项 + headless 信号退出 +
  mihomo 子进程回收 + 端口冲突卡片/通知的真机观感。
