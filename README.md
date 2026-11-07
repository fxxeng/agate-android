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

See `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` for the current product direction and implementation order.
