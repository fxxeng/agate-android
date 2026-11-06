# Shared Core 平台能力审计 — Batch 4D-3C-D 初审

- 日期：2026-11-06
- 基线：R39 `8c046f17dd61a942eedde01c070c0ac0dc3e29aa`
- 当前候选：R40 Batch 4D-3D（本文件是静态审计记录，不代表运行验证）
- 范围：`agate-server/src` 中时间、网络监听、进程与宿主 OS 条件编译调用

## 1. 结论

本轮没有发现需要把 `std::time`、Tokio timer、`std::net::TcpListener` 或 reqwest 网络调用再包一层通用 Platform API 的证据。它们当前使用的是 Rust/Tokio 跨平台接口；真正的平台差异已经集中在少量 `#[cfg(unix)]` / `#[cfg(windows)]` 分支或既有 `ProcessAdapter` 中。继续机械抽象会增加间接层，但不会改变平台行为。

本轮仅形成静态审计结论；没有执行 Windows/Android 运行时网络、信号、时钟或子进程验证。

## 2. 时间能力

- `SystemTime` + `UNIX_EPOCH` 用于持久化/协议所需的 epoch 时间戳（日志、事件、访问记录、签名/临时文件名等）。这是墙上时钟，系统时间被校正时允许跳变，不能用于计算超时。
- `Instant` / `Duration` 用于进程内经过时间、缓存新鲜度、deadline 与等待窗口；应继续使用单调时钟语义。
- `tokio::time::sleep` / `interval` 用于异步任务调度，不应替换成 `std::thread::sleep` 阻塞 Tokio worker。
- `zcode::activation::{timezone_now, os_version_now}` 的 Windows `tzutil` / `reg` 调用是明确的上游兼容探测；非 Windows 返回 `unknown` / 空串是当前产品策略，不应为了统一而在 Android 上伪造时区或 OS 版本。

## 3. 网络能力

- Gateway 使用 `std::net::TcpListener::bind` 做同步端口占用判定，再在 Tokio runtime 内通过 `tokio::net::TcpListener::from_std` 接管并由 Axum serve；源码注释已明确 `from_std` 必须位于 runtime context。
- Provider OAuth callback server 使用 Tokio TCP listener，绑定 loopback；请求客户端使用 reqwest/rustls。没有发现需按 Android/Windows 分叉的 socket API 调用。
- 网络层仍需在运行环境验证：Android 网络权限/前后台网络切换、IPv4/IPv6 loopback 行为、Windows 防火墙与端口占用错误分类；这些不是增加抽象即可证明的事项。

## 4. 进程与信号能力

- `sub_kernel::ProcessAdapter` 已将 Windows 与 Unix 的命令行窗口标志、PID kill、可执行权限差异收口。Unix 机制可被 Android target 编译复用，不代表 mihomo Android 资产受支持。
- headless `agate-server` 对 Ctrl+C 使用跨平台 Tokio signal，对 SIGTERM 使用 `#[cfg(unix)]` 分支；Windows 不伪造 SIGTERM。
- `zcode::activation` 的 `tzutil` / `reg` 仅 Windows 编译执行；非 Windows 明确返回 fallback。
- `std::process::id()`、`current_exe()` 属于标准库跨平台能力；但错误处理仍需单独审计，不能仅因 API 跨平台就推断路径一定可用。

## 5. 仍需后续处理的边界

1. R40 的 WorkBuddy `LOCALAPPDATA` 绝对路径校验由 Windows CI 编译覆盖；真实 Windows 环境变量缺失/相对值场景仍未运行验证。
2. Android 无 HOME 时旧账号导入门禁需要真机启动日志滚动确认；不把 CI 单测当作设备证据。
3. Sidecar 的 Child 回收、PID stale cleanup、端口释放需要 Windows/Linux/Android 对应宿主运行证据；`cargo check` 只能证明编译。
4. Android 网络权限、Doze/后台切换和 callback loopback 需要日常真机运行证据。

## 6. 结论与架构决定

4D-3C-D 当前不新增通用 NetworkProvider / ClockProvider。保留标准库/Tokio 跨平台调用；只对真实 OS 差异使用 `cfg` 与既有窄接口。下一步进入 4E 时，把双实例资源所有权与平台运行证据一起纳入检查，但不将设备测试设为架构批次的前置阻塞。
