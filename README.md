# Agate R40 — Batch 4D-3D Candidate

- Verification branch: `verify/arch-8faa5f5`
- Last verified base: R39 `8c046f17dd61a942eedde01c070c0ac0dc3e29aa`
- Current batch: R40 / Batch 4D-3D
- Status: source candidate; **CI not yet run**

## Scope

1. Define and test the PathProvider config-directory override contract while preserving relative-path compatibility for Desktop/headless deployments.
2. Add a strict absolute-path resolver for host directories such as Windows `LOCALAPPDATA`.
3. Apply the resolver to WorkBuddy Windows auth discovery and avoid constructing desktop auth paths on Android/Linux.
4. Add two unit tests for the account-bootstrap HOME gate: missing HOME does not invoke importers; valid HOME allows the sequence.
5. Keep the historical `./.agate` default-config fallback explicitly documented and regression-tested; do not confuse it with legacy desktop-data discovery, which must never scan CWD.
6. Add a 15-minute timeout to the Windows Shared Core check and document required CI status checks.
7. Record the initial network/time/process platform capability audit; no generic NetworkProvider or ClockProvider abstraction is introduced without evidence.

## Verification state

Static review and workflow YAML parsing completed. This environment does not provide Cargo, rustc, rustfmt, or Gradle. No R40 compilation, tests, Windows check, Android ABI build, APK build, Instrumentation, emulator, or device test has been run. R39 remains the last verified baseline.

See:
- `docs/ANDROID_KOTLIN_RUST_MIGRATION_HANDOFF.md` §31
- `docs/CORE_WORKER_LIFECYCLE_AUDIT_20261104.md` §12
- `docs/CI_RELEASE_GATES.md`
- `docs/PLATFORM_CAPABILITY_AUDIT_4D3C_D.md`
- `docs/BATCH4D3D_R40_IMPLEMENTATION_REPORT.md`
