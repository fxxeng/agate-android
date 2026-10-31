# Round 3 · Batch 2 — Android Runtime / JNI / Service Instrumentation

## Scope

This batch adds runtime verification only. It does not change Rust business logic,
SQLite schema/migrations, Provider behavior, or production Service command semantics.

### Instrumented coverage

`app/src/androidTest/java/dev/agate/android/NativeRuntimeInstrumentedTest.kt`

1. Load the real `libagate_server.so` inside Android Runtime.
2. Call `Gateway.version()` and `Gateway.coreInit()` through JNI.
3. Repeat Core initialization and assert the Core remains ready, scheduler remains off,
   and the Gateway listener is not started implicitly.
4. Call `GET /health` through `Gateway.apiCall()` / in-memory Router while the TCP
   Gateway is stopped; require a successful JNI envelope and HTTP 200 response.
5. Start and stop the actual Gateway listener on a free loopback port.
6. Start the real `AgateForegroundService`, enqueue `STOP` immediately followed by
   `START`, and verify the new start intent survives the older `startId` stop path.
7. Stop the service and restore the temporary port/LAN preferences in `finally`;
   keep the scheduler disabled in the disposable emulator to prevent background check-in.

All runtime state is stored in the app's private sandbox on a disposable emulator.
The test explicitly disables the background scheduler; no user data or external
Provider credentials are installed or used.

### JVM coverage

`PendingTickRegistryTest` verifies that a STOP snapshot only targets TICK commands
accepted before that STOP, and that completing one TICK does not remove another.
Existing `RuntimeControllerTest` and `ServiceCommandCoalescerTest` remain enabled.

## CI ABI strategy

The production Debug/Release build remains `arm64-v8a` only. CI additionally builds
an `x86_64` Rust shared library and invokes `:app:connectedDebugAndroidTest` with
`-PagateInstrumentationAbi=x86_64` on an API 35 Google APIs emulator. This temporary
property affects only the instrumentation test build; the arm64 APK artifacts are
uploaded before the emulator build starts.

## Explicitly not claimed by this batch

- No real `ACTION_TICK` is triggered; it could execute a real account check-in.
- No Provider network request or credential is used.
- WakeLock acquisition/release under a real long-running check-in is not yet tested.
- Process kill/restart, Doze, exact-alarm delivery, and crash-recovery ledger behavior
  remain Batch 3 / device-level work.
- CI emulator success is not a substitute for validation on the user's OEM device.

## Acceptance

- Existing unit tests pass.
- `:app:connectedDebugAndroidTest` passes on API 35 x86_64 emulator.
- Normal CI Debug/Release APKs remain arm64-only and pass Batch 1 APK/JNI gates.


## Round 3 · Batch 2 diagnostic follow-up (CI-Fix4)

The first API 35 x86_64 emulator run compiled the Android test APK but the
instrumentation process exited before JUnit reported any tests (`tests=0`,
`Process crashed`). The failure is not classified as a Kotlin assertion failure.

CI-Fix4 intentionally does not change Rust, JNI, Gateway, Service, or test logic.
It adds failure-preserving diagnostics around `connectedDebugAndroidTest`:

- `logcat-all.txt`: complete emulator logcat (`-b all`, threadtime format).
- `logcat-crash.txt`: crash buffer, including native crash records when emitted.
- `emulator-getprop.txt`: emulator build/ABI properties.
- `dropbox-native-crash.txt` and `dropbox-app-crash.txt`: Android DropBox crash
  records when accessible.
- `gradle-exit-code.txt`: original Gradle test exit code.

The capture runs from an EXIT trap, so a non-zero Gradle result does not skip
log collection. The instrumentation artifact upload remains `if: always()` and
now includes this diagnostics directory.

### Diagnostic decision tree

1. `Fatal signal`, tombstone, `libagate_server.so`, or Rust panic evidence:
   investigate native initialization/ABI-specific behavior before changing code.
2. `linker`, `dlopen`, missing dependency, or ELF relocation evidence:
   compare x86_64 `DT_NEEDED`, SONAME, and symbol dependencies with the arm64 build.
3. `AndroidRuntime` Java exception:
   diagnose the first fatal exception and process owner; do not infer that
   `AgateApp`'s background initialization catch covers native signals.
4. No crash evidence:
   retain the logs and add a narrower startup probe in a subsequent batch;
   do not make speculative production-code changes.

### Acceptance for CI-Fix4

- Rust all-targets and normal arm64 APK jobs remain unchanged.
- The x86_64 instrumentation compile/run is attempted as before.
- Whether it passes or fails, the artifact contains the captured diagnostics
  whenever the emulator is available to the runner script.
- The original instrumentation exit code remains the step result.
