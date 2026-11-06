# CI Release Gates

This document records which CI checks must be treated as merge/release gates for
`verify/arch-8faa5f5`. Workflow definitions create checks; GitHub branch
protection/rulesets determine whether those checks actually block merges.

## Required status checks

| Check / job | Scope | Required policy |
|---|---|---|
| `rust-tests` | Linux Shared Rust Core: `cargo check --all-targets` and `cargo test --all-targets` | Required |
| `windows-core-check` | Windows MSVC Shared Rust Core: `cargo check --locked --all-targets` | Required |
| `android-build` | Android Rust arm64-v8a/x86_64, JNI export parity, JVM tests, APK validation, Instrumentation | Required |

`windows-core-check` has a 15-minute job timeout. It compiles only the shared
`agate-server` crate; it does not build Tauri, Desktop Host UI, desktop installers,
or signing artifacts. Desktop Host CI remains a separate future workflow when
that host is present in this repository.

## Repository settings still required

A repository administrator must configure a branch protection rule or ruleset
for `verify/arch-8faa5f5` (and any future protected integration branch) with:

1. Require status checks to pass before merging.
2. Select the three job checks listed above from the GitHub Actions check list.
3. Require the branch to be up to date before merging if the repository's merge
   policy expects it.
4. Do not infer that a job is blocking merely because it appears green in the
   Actions run. Confirm the protection/ruleset settings in GitHub repository
   settings.

This file is a repository-side policy declaration only. It does not prove that
remote branch protection has been configured; until an administrator confirms
those settings, the checks are implemented but enforcement is **not verified**.
