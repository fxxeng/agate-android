#!/usr/bin/env bash
# Android Emulator instrumentation runner.
#
# CI-Fix7 changes are evidence-driven from Round 8:
# - Use the AOSP "default" image to avoid Google Play Services first-boot churn.
# - Restart adbd as root so /proc/pressure/cpu is readable.
# - Fail closed if PSI cannot be read or the guest never becomes quiet.
# - Require three consecutive quiet PSI samples before Gradle starts.
# - Preserve diagnostics and the original test/settle exit code.
set +e
cd "$(git rev-parse --show-toplevel)"

capture_instrumentation_diagnostics() {
  test_exit=$?
  trap - EXIT
  diag_dir="app/build/outputs/agate-instrumentation-diagnostics"
  mkdir -p "$diag_dir"

  adb devices -l > "$diag_dir/adb-devices.txt" 2>&1 || true
  adb logcat -b all -d -v threadtime > "$diag_dir/logcat-all.txt" 2>&1 || true
  adb logcat -b crash -d -v threadtime > "$diag_dir/logcat-crash.txt" 2>&1 || true
  adb shell getprop > "$diag_dir/emulator-getprop.txt" 2>&1 || true
  adb shell id > "$diag_dir/emulator-id.txt" 2>&1 || true
  adb shell dumpsys dropbox --print data_app_native_crash > "$diag_dir/dropbox-native-crash.txt" 2>&1 || true
  adb shell dumpsys dropbox --print data_app_crash > "$diag_dir/dropbox-app-crash.txt" 2>&1 || true
  adb shell dumpsys dropbox --print data_app_anr > "$diag_dir/dropbox-app-anr.txt" 2>&1 || true
  adb shell dumpsys dropbox --print system_app_anr > "$diag_dir/dropbox-system-anr.txt" 2>&1 || true
  adb shell cat /proc/pressure/cpu > "$diag_dir/psi-cpu.txt" 2>&1 || true
  printf '%s\n' "$test_exit" > "$diag_dir/gradle-exit-code.txt"
  exit "$test_exit"
}
trap capture_instrumentation_diagnostics EXIT

fail() {
  code="$1"
  shift
  echo "[settle] ERROR: $*" >&2
  exit "$code"
}

# ---- ADB root + device readiness -------------------------------------------
echo "[settle] waiting for adb device..."
adb wait-for-device || fail 20 "adb wait-for-device failed"

echo "[settle] restarting adbd as root..."
adb root || fail 21 "adb root failed; PSI cannot be trusted without root"
adb wait-for-device || fail 22 "device did not return after adb root"

uid="$(adb shell id -u 2>/dev/null | tr -d '\r\n')"
[ "$uid" = "0" ] || fail 23 "adb root did not take effect (uid=${uid:-empty})"
echo "[settle] adb root confirmed (uid=0)"

# ---- Boot readiness ---------------------------------------------------------
echo "[settle] waiting for boot completion and stopped boot animation..."
boot_ok=0
for i in $(seq 1 36); do
  bp="$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n')"
  ba="$(adb shell getprop init.svc.bootanim 2>/dev/null | tr -d '\r\n')"
  if [ "$bp" = "1" ] && [ "$ba" = "stopped" ]; then
    echo "[settle] boot completed + bootanim stopped (iteration $i)"
    boot_ok=1
    break
  fi
  sleep 5
done
[ "$boot_ok" = "1" ] || fail 24 "boot properties did not settle within 180s"

# ---- PSI availability: fail closed -----------------------------------------
echo "[settle] validating CPU PSI access..."
psi_probe="$(adb shell cat /proc/pressure/cpu 2>&1)"
psi_status=$?
[ "$psi_status" -eq 0 ] || fail 25 "cannot read /proc/pressure/cpu: ${psi_probe}"
printf '%s\n' "$psi_probe" | grep -Eq '^some avg10=[0-9]+([.][0-9]+)?([[:space:]]|$)' \
  || fail 26 "CPU PSI output has no valid 'some avg10' field: ${psi_probe}"
echo "[settle] CPU PSI readable"

# ---- CPU settle gate ---------------------------------------------------------
# PSI avg10 is a rolling percentage. Require 3 consecutive samples below 20
# (10s apart) so one transient low reading cannot open the gate.
# Maximum wait is 900s (CI-Fix11; R26 green run needed 379s, R27 timed out
# at 480s on slower hosts). If still busy, fail before Gradle rather than launch
# instrumentation into the same system-wide ANR storm seen in Round 8.
echo "[settle] waiting for CPU pressure avg10 < 20 for 3 consecutive samples..."
quiet_streak=0
psi_ok=0
for i in $(seq 1 90); do
  psi="$(adb shell cat /proc/pressure/cpu 2>&1)"
  psi_status=$?
  [ "$psi_status" -eq 0 ] || fail 27 "CPU PSI became unreadable: ${psi}"
  avg10="$(printf '%s\n' "$psi" | sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' | head -n 1 | tr -d '\r')"
  [ -n "$avg10" ] || fail 28 "CPU PSI avg10 disappeared: ${psi}"
  valid="$(awk -v v="$avg10" 'BEGIN { if (v ~ /^[0-9]+([.][0-9]+)?$/) print 1; else print 0 }')"
  [ "$valid" = "1" ] || fail 29 "invalid CPU PSI avg10 value: ${avg10}"

  quiet="$(awk -v v="$avg10" 'BEGIN { print (v < 20) ? 1 : 0 }')"
  if [ "$quiet" = "1" ]; then
    quiet_streak=$((quiet_streak + 1))
    echo "[settle] CPU pressure avg10=${avg10}, quiet ${quiet_streak}/3 (iteration ${i})"
    if [ "$quiet_streak" -ge 3 ]; then
      psi_ok=1
      break
    fi
  else
    quiet_streak=0
    echo "[settle] CPU pressure avg10=${avg10}, still busy; streak reset (iteration ${i})"
  fi
  sleep 10
done

[ "$psi_ok" = "1" ] || fail 30 "CPU pressure did not settle below threshold within 900s"

sleep 10
echo "[settle] guest is ready; launching Android Instrumentation"

./gradlew :app:connectedDebugAndroidTest -PagateInstrumentationAbi=x86_64 --no-daemon --stacktrace
