# Round 3 — Batch 1 CI 验证增强

基线：`verify/arch-8faa5f5` 对应快照 `3e3b5c5`。本批次只增强验证门禁，不修改 Rust 业务逻辑、数据库 schema、Provider 行为或 Android Service 生命周期实现。

## 本批次改动

1. Rust CI 从仅 `--lib` 扩展为 `cargo check --all-targets` 与 `cargo test --all-targets`，把 headless binary target 纳入检查和测试编译范围。
2. Android arm64 `.so` 构建后，运行 `verify-jni-exports.py`，逐项比较 `Gateway.kt` 中 `private external fun` 声明与 Native 库导出的 `Java_dev_agate_core_Gateway_*` 符号；缺失或多余符号均失败。
3. Debug / Release APK 构建后，运行 `verify-android-apks.py`，检查 APK ZIP 完整性、Native 库是否存在，以及该库是否为 64-bit little-endian AArch64 ELF。
4. 保留全仓 rustfmt 历史债的非阻断策略；本批次不对 413 个历史文件做全量格式化。

## 本地静态验证

- Python 两个验证脚本通过 `py_compile`。
- 从当前 Kotlin 与 Rust 源码静态提取到 12 个 JNI 声明、12 个 JNI 导出，名称集合完全一致。
- JNI 检查脚本使用模拟 `llvm-nm` 输出通过正向测试。
- APK 检查脚本使用合成的 AArch64 ELF APK 通过正向测试。

## 尚未验证 / 需要 GitHub Actions 确认

当前工作环境没有可用的 Cargo、JDK/Android SDK 构建环境，因此本地没有执行 Rust 编译、Gradle 编译或真实 Android `.so` 检查。必须将本批次上传到验证分支，由 GitHub Actions 运行实际工具链验证。

本批次不声称完成真实 Android JVM 加载 JNI、Service 生命周期压力、WakeLock 或签到进程崩溃恢复测试；这些属于后续批次。
