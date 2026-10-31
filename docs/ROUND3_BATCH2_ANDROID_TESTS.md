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
