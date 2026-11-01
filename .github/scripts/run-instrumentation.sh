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
  adb shell dumpsys dropbox --print data_app_anr > "$diag_dir/dropbox-app-anr.txt" 2>&1 || true
  adb shell dumpsys dropbox --print system_app_anr > "$diag_dir/dropbox-system-anr.txt" 2>&1 || true
  adb shell cat /proc/pressure/cpu > "$diag_dir/psi-cpu.txt" 2>&1 || true
  printf '%s\n' "$test_exit" > "$diag_dir/gradle-exit-code.txt"
  exit "$test_exit"
}
trap capture_instrumentation_diagnostics EXIT

# ---- Boot settle gate -------------------------------------------------------
# Round 6 (run 41250851141) evidence: connected tests started while the guest
# was still churning on first boot (GMS setup, dexopt, boot broadcasts).
# /proc/pressure/cpu showed `some avg60=87.39`, BOOT_COMPLETED dispatch latency
# was 38.8s, and even com.android.phone ANR'd with "failed to complete
# startup" -- our app process was collateral damage, not the culprit.
# Wait for a quiet guest before launching Gradle; never hard-fail here.
echo "[settle] waiting for boot to settle..."

boot_ok=0
for i in $(seq 1 24); do
  bp=$(adb shell getprop sys.boot_completed | tr -d '\r')
  ba=$(adb shell getprop init.svc.bootanim | tr -d '\r')
  if [ "$bp" = "1" ] && [ "$ba" = "stopped" ]; then
    echo "[settle] boot completed + bootanim stopped (iteration $i)"
    boot_ok=1
    break
  fi
  sleep 5
done
[ "$boot_ok" = "1" ] || echo "[settle] WARN: boot props not settled after 120s, continuing"

psi_ok=0
for i in $(seq 1 48); do
  psi=$(adb shell cat /proc/pressure/cpu 2>/dev/null | head -n 1)
  avg10=$(printf '%s' "$psi" | sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' | tr -d '\r')
  if [ -n "$avg10" ]; then
    quiet=$(awk -v v="$avg10" 'BEGIN{print (v < 20) ? 1 : 0}')
    if [ "$quiet" = "1" ]; then
      echo "[settle] CPU pressure quiet (avg10=$avg10, iteration $i)"
      psi_ok=1
      break
    fi
    echo "[settle] CPU pressure avg10=$avg10, waiting... (iteration $i)"
  fi
  sleep 5
done
[ "$psi_ok" = "1" ] || echo "[settle] WARN: CPU pressure still high after 240s cap, continuing"

sleep 15
echo "[settle] done, launching Gradle"

./gradlew :app:connectedDebugAndroidTest -PagateInstrumentationAbi=x86_64 --no-daemon --stacktrace
