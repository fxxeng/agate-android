# Agate R42 — Android-only Mainline Candidate

- Verification branch: `verify/arch-8faa5f5`
- Base candidate: R41 `530e02bb90c068d43eee8d7ce25a25b82b6864e9`
- Current batch: R42 / Android-only build and scope convergence
- Status: workflow + documentation candidate; **R42 CI not yet run**

## Direction

Agate's only active product delivery target is Android. Prioritize Android Kotlin/Compose Host, Foreground Service, JNI, Android Rust ABIs, Shared Core runtime, storage, Sidecar, background work, app packaging and release closure. Do not schedule Windows/macOS/Linux Desktop feature work as a prerequisite for the Android mainline.

The repository already has no complete Tauri/Desktop UI Host. The remaining desktop-specific code is mostly platform-gated path discovery, credential import, Windows APIs and a Headless binary. Keep the single Rust Shared Core; isolate or remove only code proven unnecessary to Android and safe for account/data compatibility.

## R42 changes

1. Remove the Windows Shared Core CI job from the Android workflow.
2. Narrow Linux Rust checks to portable library `--lib` check/tests; do not compile the Headless binary as an Android mainline target.
3. Keep Android arm64-v8a + x86_64 Rust build, JNI parity, JVM tests, APK checks and emulator Instrumentation as the authoritative product gate.
4. Add `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` with module boundaries, phased completion order and safe deletion rules.

## Verification state

R40 `d3e049e89b932325e49c57b5f6a5a23c825bcca8` is the last CI-verified source baseline (Run `42199123476`). R41 `530e02bb90c068d43eee8d7ce25a25b82b6864e9` is a candidate and was not independently CI-verified before this R42 scope pivot. R42 has not yet run Cargo, Gradle, Android ABI, emulator or device validation.

See `docs/ANDROID_ONLY_MAINLINE_PLAN_R42.md` for the current product direction and implementation order.
