# Agate R42 — Android-only Mainline Candidate

- Verification branch: `verify/arch-8faa5f5`
- Base candidate: R41 `530e02bb90c068d43eee8d7ce25a25b82b6864e9`
- Candidate note: R41 本地候选（未推送远端；代码改动已合入 R42 ad1bc9cef3）
- Current batch: R42 / Android-only build and scope convergence
- Status: R42 CI verified
- 状态：R42 ad1bc9cef3 已通过 GitHub Actions Run 42235540958：rust-tests 761 passed / 11 ignored，lib warning 9 条；android-build JVM 99/99、JNI 12 符号、双 APK 校验、Instrumentation 2/2。

## Direction

Agate's only active product delivery target is Android. Prioritize Android Kotlin/Compose Host, Foreground Service, JNI, Android Rust ABIs, Shared Core runtime, storage, Sidecar, background work, app packaging and release closure. Do not schedule Windows/macOS/Linux Desktop feature work as a prerequisite for the Android mainline.

The repository already has no complete Tauri/Desktop UI Host. The remaining desktop-specific code is mostly platform-gated path discovery, credential import, Windows APIs and a Headless binary. Keep the single Rust Shared Core; isolate or remove only code proven unnecessary to Android and safe for account/data compatibility.

## R42 changes

1. Remove the Windows Shared Core CI job from the Android workflow.
2. Narrow Linux Rust checks to portable library `--lib` check/tests; do not compile the Headless binary as an Android mainline target.
3. Keep Android arm64-v8a + x86_64 Rust build, JNI parity, JVM tests, APK checks and emulator Instrumentation as the authoritative product gate.
4. Add `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` with module boundaries, phased completion order and safe deletion rules.

## Verification state

R42 ad1bc9cef3 已通过 GitHub Actions Run 42235540958：rust-tests 761 passed / 11 ignored，lib warning 9 条；android-build JVM 99/99、JNI 12 符号、双 APK 校验、Instrumentation 2/2。

历史：R40 `d3e049e89b932325e49c57b5f6a5a23c825bcca8` 为 R42 之前的已验证基线（Run `42199123476`）；R41 本地候选（未推送远端；代码改动已合入 R42 ad1bc9cef3）。R40 的 Windows Shared Core 门禁已由 R42 移除。

## Signing

- `app/agate-debug.jks` 是随仓库提交的**共享调试凭证**（口令 `agatedebug`，alias `agate`）：只用于让本地与 CI 产出同一签名的 debug 包、覆盖安装不丢数据。它**不是生产密钥**，禁止用于对外发布；指纹即公开信息，仅用于核对安装包来源，CI 对 debug 产物做该指纹硬断言（`scripts/verify-android-apks.py`）。
  证书 SHA-256：`37:2D:51:DC:45:DC:4A:73:C3:29:06:5F:43:01:A0:66:38:4F:CA:A8:FB:FF:13:16:21:7E:6D:16:64:CA:0E:D3`
- release 构建当前**未配置签名**（产物为未签名 APK），正式签名的地基（GitHub Secrets 注入 + `--release-cert-sha256` 指纹断言，脚本能力已就绪）暂缓启用。启用前 release 产物仅限内部自装测试，不得对外分发；一旦启用正式签名，历史共享 debug 签名的安装将无法直接覆盖升级（签名不匹配），需卸载重装。

See `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` for the current product direction and implementation order.
