#!/usr/bin/env bash
# Instrumentation runner with EXIT-trap diagnostics capture.
# Kept as a committed file because android-emulator-runner@v2 executes the
# `script:` input line-by-line via `sh -c` (observed in run 41215022452:
# a multi-line function definition broke with "end of file unexpected"),
# so the whole logic must live in a single `bash <file>` line in the workflow.
set +e
cd "$(git rev-parse --show-toplevel)"

capture_instrumentation_diagnostics() {
  test_exit=$?
  trap - EXIT
  diag_dir="app/build/outputs/agate-instrumentation-diagnostics"
  mkdir -p "$diag_dir"

  adb logcat -b all -d -v threadtime > "$diag_dir/logcat-all.txt" 2>&1 || true
  adb logcat -b crash -d -v threadtime > "$diag_dir/logcat-crash.txt" 2>&1 || true
  adb shell getprop > "$diag_dir/emulator-getprop.txt" 2>&1 || true
  adb shell dumpsys dropbox --print data_app_native_crash > "$diag_dir/dropbox-native-crash.txt" 2>&1 || true
  adb shell dumpsys dropbox --print data_app_crash > "$diag_dir/dropbox-app-crash.txt" 2>&1 || true
  printf '%s\n' "$test_exit" > "$diag_dir/gradle-exit-code.txt"
  exit "$test_exit"
}
trap capture_instrumentation_diagnostics EXIT

./gradlew :app:connectedDebugAndroidTest -PagateInstrumentationAbi=x86_64 --no-daemon --stacktrace
