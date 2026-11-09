# Agate — Android-only Mainline Candidate

- Verification branch: `verify/arch-8faa5f5`
- Current batch: R47 / 运行时正确性与所有权收口（T1–T5）
- Latest verified: `a5fb016` — GitHub Actions Run `42551225790`（双 job 全绿；R47 T5 收尾批）
- 状态：rust-tests 793 passed / 11 ignored；android-build JVM 单元测试 119/119、JNI 12 入口（符号 + Kotlin/Rust 描述符签名级互检）、双 APK 校验（debug 共享凭证指纹硬断言）、Instrumentation 2/2。
- 真机验收待做（APK 自取 run artifacts）；release 正式签名按委托方裁定暂缓。

## Direction

Agate's only active product delivery target is Android. Prioritize Android Kotlin/Compose Host, Foreground Service, JNI, Android Rust ABIs, Shared Core runtime, storage, Sidecar, background work, app packaging and release closure. Do not schedule Windows/macOS/Linux Desktop feature work as a prerequisite for the Android mainline.

The repository already has no complete Tauri/Desktop UI Host. The remaining desktop-specific code is mostly platform-gated path discovery, credential import, Windows APIs and a Headless binary. Keep the single Rust Shared Core; isolate or remove only code proven unnecessary to Android and safe for account/data compatibility.

## R47 changes — 运行时正确性与所有权收口

1. **运行时正确性（P1）**：checkin-fire 任务身份还原到条目级（修多计划调度确定性全天饥饿）；supervisor 完成条目回收（panic 进 64 条有限历史，不再无界累积）。
2. **数据目录所有权**：`acquire_shared` 登记表锁全程持有（同进程并发首锁不再误报 Occupied）、bootstrap 冲突测试真正落在 bootstrap 链路、`ensure_core` 校验目录一致性（`DATA_DIR_MISMATCH`）、CLOEXEC 失败显式报错。
3. **JNI 契约**：`json_reply` 兜底前清理 pending exception；`envelope` 结构从严（只捕 `JSONException`、`ok` 必须布尔、失败必须带 error）；stop 三态（`released=false` → `StopTimeout`，不再假装 Stopped）；Kotlin/Rust 描述符签名级互检（12 入口）。
4. **门禁与发布地基**：Android target clippy 门（首次覆盖 `cfg(target_os="android")` 与 `jni_bridge`）；APK 签名校验接入 CI（debug 共享凭证指纹硬断言；release 未签名放行，正式签名暂缓）。
5. **收尾（T5）**：ChatGLM 嵌套令牌迁移补齐「仅 refresh / 多账号同批 / 落库失败原数据完整可重试」测试；README / HANDOFF / MAINLINE / JNI 与 Gateway 头注释 / `GLOBAL_RUNTIME` 注释与当前实现对齐。

## Verification state

R47 代码基线 `a5fb016` 通过 Run `42551225790`：rust-tests 793 passed / 11 ignored；android-build 全步 success（Android clippy 门、JNI 符号 + 描述符、JVM 119/119、双 APK 校验、Instrumentation 2/2）。

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
