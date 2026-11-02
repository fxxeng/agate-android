#!/usr/bin/env bash
# Agate M0：交叉编译 libagate_server.so 并放进 core-gateway 的 jniLibs。
#
# 用法（本地 Termux 无 NDK，实际在 GitHub Actions 上跑；本脚本 CI 与本地通用）：
#   ANDROID_NDK_HOME=/path/to/ndk bash scripts/build-android-so.sh [abi...]
#   支持一次编译多个 ABI（cargo ndk 多 -t 共享依赖编译，CI 省一遍全量）：
#     bash scripts/build-android-so.sh arm64-v8a x86_64
#   缺省 arm64-v8a。
#
# 产物：core-gateway/src/main/jniLibs/<abi>/libagate_server.so
#       （Gradle 自动打进 APK；该目录在 .gitignore 里，由构建现场生成）
# APK 的 ABI 纯净性由 app/build.gradle.kts 的 ndk.abiFilters 保证：
# jniLibs 里多躺一个 x86_64 .so 不会进常规 Debug/Release 包。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_ABIS=("${@:-arm64-v8a}")
OUT_DIR="$REPO_ROOT/core-gateway/src/main/jniLibs"

: "${ANDROID_NDK_HOME:?需要设置 ANDROID_NDK_HOME（NDK 根目录）}"
command -v cargo >/dev/null || { echo "❌ cargo 不在 PATH"; exit 1; }
command -v cargo-ndk >/dev/null || cargo install cargo-ndk --locked

ARGS=()
for ABI in "${TARGET_ABIS[@]}"; do
  ARGS+=(-t "$ABI")
done
echo "▶ cargo ndk ${ARGS[*]} build --release --lib（首次全量编译约 5-15 分钟）"
cd "$REPO_ROOT/agate-server"
cargo ndk "${ARGS[@]}" -o "$OUT_DIR" build --locked --release --lib

for ABI in "${TARGET_ABIS[@]}"; do
  SO="$OUT_DIR/$ABI/libagate_server.so"
  test -f "$SO" || { echo "❌ 产物未生成：$SO"; exit 1; }
  ls -lh "$SO"
done
echo "✅ 产物就位：${TARGET_ABIS[*]}（Gradle 自动并入 APK 的 jniLibs）"
