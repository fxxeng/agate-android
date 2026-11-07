# Agate Android-only 主线推进方案 — R42

- 基线候选：R41 `530e02bb90c068d43eee8d7ce25a25b82b6864e9`；R41 本地候选（未推送远端；代码改动已合入 R42 ad1bc9cef3）
- 工作分支：`verify/arch-8faa5f5`
- 本轮定位：收敛 Android-only 构建与开发范围
- 状态：R42 ad1bc9cef3 已通过 GitHub Actions Run 42235540958：rust-tests 761 passed / 11 ignored，lib warning 9 条；android-build JVM 99/99、JNI 12 符号、双 APK 校验、Instrumentation 2/2。

## 1. 总决策

Agate 当前阶段以 Android App 为唯一产品交付主线。优先完成 Android Host、Kotlin Runtime、JNI、Android Rust ABI、网关业务、Sidecar、数据目录、后台任务与发布闭环；不再为了桌面端功能完整性安排独立开发批次，也不把 Windows/macOS/Linux Desktop CI 作为 Android 主线门禁。

这里的“剥离桌面端”首先指**从 Android 构建、依赖、启动和验证路径中隔离**，不是删除所有带有 Windows/macOS 条件编译的源码。Rust Shared Core 保持单一实现；只有经调用链证明 Android 不需要、且不会破坏 Provider/账号/数据兼容的桌面专属功能，才分批删除或编译期排除。

## 2. 当前源码边界（基于 R41）

| 模块 | 当前状态 | Android 主线处理 |
|---|---|---|
| `app/` | Kotlin/Compose Android App | 保留并作为产品主入口 |
| `core-gateway/` | Android Library、JNI Kotlin Facade、Foreground Service | 保留并优先完善 |
| `agate-server/src/jni_bridge.rs` | `cfg(target_os = "android")` JNI 入口 | 保留，收敛契约与线程/取消语义 |
| `agate-server/src/server/**` | Gateway、Provider、数据库、协议、任务和 Sidecar | 保留 Android 运行所需业务，不复制第二套 Core |
| `agate-server/src/bin/agate-server.rs` | Headless 二进制入口 | Android `.so` 使用 `cargo build --lib`，不进入 APK；后续评估是否从默认检查目标排除 |
| `#[cfg(windows)]` 及 Windows 依赖 | DPAPI、注册表、DLL、Windows 路径等 | 不进入 Android target 编译；不再为它们维护 Android 主线 Windows job |
| Desktop 自动导入路径 | Provider 的桌面客户端目录发现 | Android 只允许显式文件导入/手动凭证；自动发现应返回明确不支持，不扫描桌面路径 |
| Tauri/Desktop UI | R41 仓库中不存在完整 Desktop Host 工程 | 不需要重复删除；不得重新引入 Android 依赖链 |

## 3. CI 收敛

R42 将验证结构收敛为两个 job：

1. `rust-tests`：Ubuntu 上仅运行可移植 Rust library `cargo check --lib` 与 `cargo test --lib`。它用于快速执行共享业务纯逻辑测试，不代表 Linux Desktop 产品构建，也不再构建 Headless binary。
2. `android-build`：权威产品门禁。交叉编译 Android `arm64-v8a` 与 `x86_64` Rust `.so`，核对 JNI exports，运行 Gradle/JVM tests，构建 Debug/Release APK，检查 APK/ELF，并运行 x86_64 Emulator Instrumentation。

删除 Windows Shared Core job。Windows 平台源码不再阻塞 Android 主线；保留在仓库中的 Windows 条件编译代码只作为未承诺的兼容遗留，不得标记为本项目当前交付能力。

## 4. 主线完成顺序（先闭环，再打磨）

### M1 — Android Host 与构建边界闭环

- CI 只保留 Android 产品门禁 + 共享 Rust 纯逻辑测试。
- Rust Android `.so` 仅构建 `--lib`，APK 只打入声明 ABI。
- 确认 Release APK、签名配置、版本号、ABI、JNI 符号和产物可重复。
- 清点所有 desktop-only path discovery、process invocation、DPAPI/registry/DLL 分支；Android 统一返回明确能力状态。

### M2 — Core 与数据目录所有权

- Android 显式注入 app-private data dir。
- 在数据库迁移、配置写入、Sidecar spawn、后台任务启动之前取得 Android data-dir owner lock。
- 锁采用 Android/Linux 可用的 OS advisory lock；不为未承诺的 Windows/Desktop 做抽象扩展。
- 同一目录第二 owner 稳定拒绝；异常退出由 OS 释放锁；拒绝路径不得产生写入或启动副作用。
- 验证多进程/重复初始化、锁释放、进程崩溃恢复和路径别名。

### M3 — Android Runtime / Service 生命周期闭环

- Kotlin 是 Android Service、通知、Boot、Alarm、WakeLock 和系统回调的唯一 owner。
- Rust Core 生命周期与 Gateway start/stop、Activity 销毁、Foreground Service stop 明确区分。
- STOP/START/TICK 顺序屏障、重复命令合并、onDestroy 清理、长任务取消与超时形成可测状态机。
- Sidecar Child、Tokio task、SSE/回调的取消和 join 有确定收尾路径。

### M4 — JNI 与并发契约闭环

- JNI API 版本/能力探测、错误结构、线程模型、回调 GlobalRef、取消和超时统一。
- 禁止 panic 穿越 JNI；不把 `null` 当通用错误吞掉。
- Kotlin Repository/Transport 只依赖稳定 DTO/JSON 契约，不依赖 Rust 内部模块路径。

### M5 — Android 平台能力与网络闭环

- Android app-private filesystem、URI 文件导入、权限、网络变化、代理、TLS/证书、IPv4/IPv6、后台限制逐项验证。
- 进程/Sidecar 仅使用 Android ABI 可执行产物；明确下载、校验、权限、启动、停止、升级与崩溃恢复。
- 对不支持的桌面自动导入、注册表、系统命令等能力给出明确 UI/API 状态，不伪装为普通文件不存在。

### M6 — 产品功能与发布闭环

- UI 页面与 Rust API 对账，检查每个用户操作都有可观察状态、错误和恢复路径。
- 账号/Provider、模型、路由、代理、签到任务、日志、备份恢复、配置迁移完成 Android 端端到端验证。
- Release APK 安装、升级覆盖、数据保留、卸载行为、重启恢复和长时间后台运行完成滚动验收。

### M7 — 主线完成后再做打磨

- 性能 profiling、启动耗时、内存、包体积、数据库 IO、网络连接复用。
- Rust/Kotlin 代码整洁、格式债、模块拆分、依赖瘦身和可观测性优化。
- 只在真实瓶颈或维护成本有证据时做微优化，不提前引入过度抽象。

## 5. 桌面代码删除/隔离规则

每个候选删除项必须先满足：

1. 不被 Android target 的依赖图、模块引用或运行时路径使用；
2. 不承载 Android 用户仍需要的通用 Provider 协议、凭证解析、文件导入能力；
3. 删除后不会破坏现有数据库、账号来源标记、配置迁移和升级兼容；
4. 有 Android compile/test 证据；
5. 修改范围按功能切片，不能一次性删除整个 Provider 或 Shared Core。

优先清理：仅服务于 Windows Desktop 自动发现的注册表/DLL/DPAPI入口、桌面安装目录探测、Android 永远不可达的 UI/Host 依赖。谨慎处理：Provider 凭证格式解析、用户显式选择文件后的导入、共享网络/协议/数据库逻辑。

## 6. 验证口径

- 静态审查、YAML 解析、`git diff --check` 不等于编译通过。
- Linux host `--lib` 测试是共享纯逻辑测试，不代表 Linux Desktop 支持。
- Android Rust ABI、Gradle、APK、Emulator Instrumentation 是 CI 证据。
- 真机证据在 APP 日常使用中滚动收集，不阻塞 M1-M5 的代码推进；未实测的 Android 行为必须标为待验证。
- 只在 `verify/arch-8faa5f5` 工作，不修改 `main`。
