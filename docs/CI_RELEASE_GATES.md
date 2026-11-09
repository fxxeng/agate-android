# CI Release Gates — Android-only mainline

This document records the CI checks for `verify/arch-8faa5f5`. Agate's active
product target is Android; Windows/macOS/Linux Desktop product builds are not
part of this mainline. Workflow definitions create checks; GitHub branch
protection/rulesets determine whether those checks actually block merges.

## Required status checks

| Check / job | Scope | Required policy |
|---|---|---|
| `rust-tests` | Ubuntu host execution of portable Rust library `--lib` check/tests + panic-discipline clippy (six denies); not a Linux Desktop build. `cargo fmt --check` is blocking since R48 | Required |
| `android-build` | Android Rust arm64-v8a/x86_64, Android-target clippy (same six denies), JNI symbol + Kotlin/Rust descriptor parity, JVM unit tests, debug/release APK validation (incl. debug certificate fingerprint), Emulator Instrumentation | Required |

The Android job is the authoritative product build and test gate. The Linux job
exists only to execute portable Shared Core library tests quickly. It does not
build the Headless binary or claim Linux Desktop support.

R49 coverage: `rust-tests` covers AutoCheckin backfill cancellation (6 cases:
stop before first plan / during plan / generation hand-off / generation advance
without a dispatch loop / configure-off) and the `bind → start_gateway_tasks`
ordering contract (3 cases). The STOP four-state consumption and probe-port
decision are Kotlin logic and run in the `android-build` JVM unit-test step
(`StopLifecyclePolicyTest`, `GatewayPortChoiceTest`,
`RuntimeControllerTest`). R49 adds 9 Rust cases (local `--lib` 796 → 805; host runner 802 because three
`jni_bridge` cases are Android-only) and 15 JVM cases (121 → 136). Verified by
Run `42597312744` on `3e47d97`: `rust-tests` 802 passed / 11 ignored,
`android-build` JVM 136/136 and Instrumentation 2/2.

## Repository settings still required

A repository administrator must configure a branch protection rule or ruleset
for `verify/arch-8faa5f5` with:

1. Require status checks to pass before merging.
2. Select `rust-tests` and `android-build` from the GitHub Actions check list.
3. Require the branch to be up to date before merging if the repository's merge
   policy expects it.
4. Confirm the protection/ruleset settings in GitHub repository settings.

This file is a repository-side policy declaration only. It does not prove that
remote branch protection has been configured; until an administrator confirms
those settings, enforcement is **not verified**.
