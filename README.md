# Agate — Android-only Mainline Candidate

- Verification branch: `verify/arch-8faa5f5`
- Current batch: R51 / 旧登录与账号数据兼容移除（已收口）
- Latest verified: `d28588c` — GitHub Actions Run `42751468673`（双 job 全绿；R51 旧登录/账号数据兼容移除）
- 状态：R51 已验证 rust-tests **802 passed / 11 ignored**（CI 宿主口径；本机 809，环境差异以 CI 为准）、JVM **143/143**、JNI **15**、双 APK 仅 arm64-v8a + AArch64 ELF（debug 共享凭证指纹命中，release 未签名放行）、Instrumentation **2/2**；`cargo fmt --check` 与六 deny clippy 全绿。
- 真机验收待做（APK 自取 run artifacts）；release 正式签名按委托方裁定暂缓。

## Direction

Agate's only active product delivery target is Android. Prioritize Android Kotlin/Compose Host, Foreground Service, JNI, Android Rust ABIs, Shared Core runtime, storage, Sidecar, background work, app packaging and release closure. Do not schedule Windows/macOS/Linux Desktop feature work as a prerequisite for the Android mainline.

The repository already has no complete Tauri/Desktop UI Host. The remaining desktop-specific code is mostly platform-gated path discovery, credential import, Windows APIs and a Headless binary. Keep the single Rust Shared Core; isolate or remove only code proven unnecessary to Android and safe for account/data compatibility. mihomo Sidecar remains interface-only on Android: `sub_kernel::asset_for_platform()` returns `None` for `android/*`, so kernel download/autostart are not Android deliverables.

## R47 changes — 运行时正确性与所有权收口

1. **运行时正确性（P1）**：checkin-fire 任务身份还原到条目级（修多计划调度确定性全天饥饿）；supervisor 完成条目回收（panic 进 64 条有限历史，不再无界累积）。
2. **数据目录所有权**：`acquire_shared` 登记表锁全程持有（同进程并发首锁不再误报 Occupied）、bootstrap 冲突测试真正落在 bootstrap 链路、`ensure_core` 校验目录一致性（`DATA_DIR_MISMATCH`）、CLOEXEC 失败显式报错。
3. **JNI 契约**：`json_reply` 兜底前清理 pending exception；`envelope` 结构从严（只捕 `JSONException`、`ok` 必须布尔、失败必须带 error）；stop 三态（`released=false` → `StopTimeout`，不再假装 Stopped）；Kotlin/Rust 描述符签名级互检（12 入口）。
4. **门禁与发布地基**：Android target clippy 门（首次覆盖 `cfg(target_os="android")` 与 `jni_bridge`）；APK 签名校验接入 CI（debug 共享凭证指纹硬断言；release 未签名放行，正式签名暂缓）。
5. **收尾（T5）**：ChatGLM 嵌套令牌迁移补齐「仅 refresh / 多账号同批 / 落库失败原数据完整可重试」测试；README / HANDOFF / MAINLINE / JNI 与 Gateway 头注释 / `GLOBAL_RUNTIME` 注释与当前实现对齐。
6. **Service 接线（T6）**：`runTick` 的「探测异常 → null → 策略」提为可测纯函数 `stopServiceAfterTick`（异常上报后 fail-closed），JVM 补 2 条接线测试（连同原有策略用例共 3 条）；关闭「Service 内接线无单测」遗留。

## R48 changes — 格式化批

1. **全库格式化**：`cargo fmt --all`（agate-server workspace）418 文件 +21475/−8800，纯格式化无逻辑改动；rustfmt 非幂等，两轮收敛后 `--check` 干净；本地 `cargo test --lib` 796/0/11。
2. **格式门转阻断**：CI `rust-tests` 的 `cargo fmt --check` 移除 `continue-on-error`，格式漂移自此红灯。预检确认本机与 CI stable 的 rustfmt `1.10.0 (58cf8f9c35 2026-10-13)` 对同一提交输出 diff 的 SHA-256 完全一致。

## R49 changes — 运行时正确性（STOP / 补签 / 启动顺序 / 端口）

1. **STOP 生命周期（T1）**：新增 `StopExecutionResult` 四态（SkippedByNewerIntent / Stopped / StopTimedOut / Failed）与 `StopLifecyclePolicy`；只有 `Stopped` 释放保活锁并退出前台，超时与失败保留 FGS、10s×3 有界复核，复核用尽仍保留服务（R48「超限退出前台」废止）；JNI 异常收敛为 `Failed(cause)`，不再上抛。
2. **补签取消（T2）**：`AutoCheckin` 引入调度代次；`stop()` 无条件推进（修 `task.is_none()` early-return）；`start()` 每代一个 `checkin-backfill:{gen}` 任务；`configure(enabled=false)` 直接停调度；取消骨架 `run_backfill` 在每条计划启动前/收尾后校验，只阻止后续计划、不中断在途 Provider 请求。
3. **启动顺序（T3）**：`server::run_after_successful_bind` —— 端口绑定成功后才提交 Sidecar autostart；`jni_bridge` 与 headless bin 同一契约。
4. **端口（T4）**：`GatewayPortChoice` 统一探测端口（运行态实际端口优先、配置端口回落）；控制页 5s 轮询刷新、不再写死 8787；健康卡文案取设置；失败卡片区分「启动失败 / 停止未完成」。
5. **文档**：新增 `docs/R49_RUNTIME_CORRECTNESS.md`（含未执行的真机验收清单与「不得折算为通过」声明）；JNI 契约 §3.9 与沿革同步。

## R50 changes — 运行时加固（STOP / 缓存 / TICK / 日志 / 闹钟 / 异步）

1. **STOP 相位（T1）**：停止确认改用 serve 退出标记，`status` 暴露 `stopping`；只有确认退出才报 Stopped，不再把「已发停止请求」当「已停」。
2. **管理面缓存（T2）**：Router 缓存按 `(port, host, local_panel)` 失效；改端口 / host / 本地面板后立即走新目标，无需杀进程。
3. **LAN 管理 Key（T3）**：改用 `SecureRandom` 24 字节 CSPRNG，不再用时间/随机数拼接。
4. **TICK 非粘性（T4）**：前台服务 TICK 改 `START_NOT_STICKY`，系统重启不复活已停止的网关。
5. **签到上限（T5）**：`checkin_run_and_wait` 改 15min `recv_timeout`，超时返回 `{done:false,error:"timeout"}` 并请求账号边界取消；Kotlin WakeLock 16min，覆盖 JNI 等待并留 1min 余量，避免上游挂住无限持锁。
6. **取消锁存（T6）**：`TaskCancellation` 取消状态锁存，二次消费不再 panic。
7. **日志转发（T7）**：Tokio worker 改 `attach_current_thread_permanently`；删除非 BMP→U+FFFD 转换，emoji 按 CESU-8 原样过 JNI；Kotlin 改用 `BoundedLogQueue(2048)`，消费慢时丢弃并限频告警；serve 非 stop 退出发布 `gateway_stopped(reason=serve_exited)`，SSE 不再静默断流。
8. **就绪等待（T8）**：Core 就绪等待从 0.6s 扩到 15s，Failed 立即返回，不再把慢初始化误判为启动失败。
9. **Manifest / 版本（T9）**：`BootReceiver exported=false`；新增 `ExactAlarmPermissionReceiver`，精确闹钟授权变化后重排；versionCode 50 / versionName `0.5.0-r50`；页脚读 `BuildConfig.VERSION_NAME`；补 JNI keep 规则；`onDestroy` 清理排队回调；停止复核改专用单线程。
10. **异步管理面（T10）**：管理面直调异步化并可取消（`api_call` + callback），JNI 契约升 v3；响应体上限 16 MiB；CI 工具链全钉版本。
11. **交付 ABI（T11）**：release 仅 `arm64-v8a`，构建仍保留 x86_64 供 emulator Instrumentation；APK 校验断言仅 arm64 + AArch64 ELF。
12. **CI（T12）**：放行 emulator-runner 的旧 `add-path` 命令，避免「测试 2/2 通过但 step 红」。

## R51 changes — 旧登录/账号数据兼容移除

委托方裁定：账号可以重新登录，不再维护账号登录数据相关的旧数据兼容，避免加重维护负担。旧文件原地保留、不改名、不删除，只是不再读取。

1. **旧 `auth.json` 导入删除**：`account_bootstrap::import_legacy_auth_json` / `read_legacy_session`、`AccountStore::import_legacy_session`、`endpoints::legacy_auth_file` 全部移除。
2. **三家旧账号导入删除**：`raccoon_import.rs` / `catpaw_import.rs` / `autoclaw_import.rs` 三个模块、`account_bootstrap` 调用与 HOME 门禁、对应测试全部删除。
3. **chatglm 嵌套令牌兼容删除**：`credentials::from_record` 只读顶层 `accessToken` / `refreshToken`；`migrate_nested_tokens` 迁移与全部迁移测试删除。旧嵌套记录不再被识别，重新添加/登录即可。
4. **`accounts.json` 迁移删除**：`db/migrate/accounts.rs` 与注册表项移除；升级流程不再导入账号，旧文件留在原处。`upgrade_items` 只剩配置/日志/统计类。
5. **保留当前数据库形态的收敛**：`migrate_startup`（provider 补齐 / 优先级去重 / Cline 拆池 / WorkBuddy 拆家）、历史明文账号行加密、网关 Key 加密、限流冷却键清理；这些不是旧登录数据兼容。
6. **保留当前登录/导入能力**：`account_transfer` 账号转移、WorkBuddy 桌面登录态手工导入入口（Android 平台门禁不可用）、各 provider 手动粘贴凭证的现行输入解析。

净删除约 **2.3k 行**（21 文件，−2291/+135）；CI Run `42751468673` 双绿：Rust **802/11**（本机 809，环境差异以 CI 为准）、JVM **143/143**、JNI **15**、双 APK 仅 arm64-v8a、Instrumentation **2/2**。`cargo check --lib` 0 warning，`cargo fmt --check` 通过。

## Verification state

R50 代码基线 `756e99d` 通过 Run `42733103334`：rust-tests 815 passed / 11 ignored（`cargo fmt --check` + 宿主/Android target clippy 六 deny 全绿）；android-build 全步 success（JNI 15 入口符号 + 描述符互检、JVM 143/143、双 APK 均仅 arm64-v8a + AArch64 ELF、Instrumentation 2/2）。相对 R49：Rust CI 812→815（净 +3），JVM 136→143（+7）。

R49 代码基线 `9fe250e` 通过 Run `42605799073`：rust-tests 812 passed / 11 ignored（`cargo fmt --check` + 宿主 clippy 六 deny 全绿）；android-build 全步 success（Rust 全量重编译、Android target clippy 六 deny、JNI 符号 + 描述符互检、JVM 136/136、双 APK 校验、Instrumentation 2/2）。R49 新增 19 条 Rust 用例（本地 `--lib` 815，宿主 812——3 条 `jni_bridge` 用例是 Android-only）与 15 条 JVM 用例（121 → 136）。

R48 代码基线 `b9dcaa0` 通过 Run `42562774123`：rust-tests 793 passed / 11 ignored（`cargo fmt --check` 已阻断并通过）；android-build 全步 success（Rust 全量重编译、Android clippy 门、JNI 符号 + 描述符、JVM 121/121、双 APK 校验、Instrumentation 2/2）。

R47 代码基线 `2d210d5` 通过 Run `42558923248`：rust-tests 793 passed / 11 ignored；android-build 全步 success（Android clippy 门、JNI 符号 + 描述符、JVM 121/121、双 APK 校验、Instrumentation 2/2）。

历史：R40 `d3e049e89b932325e49c57b5f6a5a23c825bcca8` 为 R42 之前的已验证基线（Run `42199123476`）；R41 本地候选（未推送远端；代码改动已合入 R42）；R42 完成 Android-only 范围收敛（Run `42235540958`：JVM 99/99）。

## R42 changes（范围收敛，历史）

1. Remove the Windows Shared Core CI job from the Android workflow.
2. Narrow Linux Rust checks to portable library `--lib` check/tests; do not compile the Headless binary as an Android mainline target.
3. Keep Android arm64-v8a + x86_64 Rust build, JNI parity, JVM tests, APK checks and emulator Instrumentation as the authoritative product gate.
4. Add `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` with module boundaries, phased completion order and safe deletion rules.

## Signing

- `app/agate-debug.jks` 是随仓库提交的**共享调试凭证**（口令 `agatedebug`，alias `agate`）：只用于让本地与 CI 产出同一签名的 debug 包、覆盖安装不丢数据。它**不是生产密钥**，禁止用于对外发布；指纹即公开信息，仅用于核对安装包来源，CI 对 debug 产物做该指纹硬断言（`scripts/verify-android-apks.py`）。
  证书 SHA-256：`37:2D:51:DC:45:DC:4A:73:C3:29:06:5F:43:01:A0:66:38:4F:CA:A8:FB:FF:13:16:21:7E:6D:16:64:CA:0E:D3`
- release 构建当前**未配置签名**（产物为未签名 APK），正式签名的地基（GitHub Secrets 注入 + `--release-cert-sha256` 指纹断言，脚本能力已就绪）暂缓启用。启用前 release 产物仅限内部自装测试，不得对外分发；一旦启用正式签名，历史共享 debug 签名的安装将无法直接覆盖升级（签名不匹配），需卸载重装。

See `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` for the current product direction and implementation order.
