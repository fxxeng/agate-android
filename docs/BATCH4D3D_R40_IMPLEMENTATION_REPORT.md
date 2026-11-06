# Batch 4D-3D / R40 实施记录

- 日期：2026-11-06
- 基线：R39 `8c046f17dd61a942eedde01c070c0ac0dc3e29aa`
- 分支目标：`verify/arch-8faa5f5`
- 状态：候选源码已整理；等待 GitHub CI；不是已验证版本。

## 本批修改

1. `agate-server/src/paths.rs`
   - config override 保留非空相对路径兼容，过滤空白/NUL；新变量无效时继续回退旧变量。
   - 增加 `resolve_absolute_path()`，用于必须是绝对路径的宿主目录。
   - 明确 `default_config_dir()` 的 `./.agate` 是历史默认配置落点，暂不擅自迁移；增加回归测试。
   - 增加相对路径兼容、NUL fallback、宿主绝对路径与默认目录行为测试。
2. `agate-server/src/server/core/account_store/workbuddy_import.rs`
   - Windows `LOCALAPPDATA` 改用严格绝对路径解析。
   - WorkBuddy auth 路径按 Windows/macOS 条件构造；Android/Linux 不再拼接桌面客户端目录。
3. `agate-server/src/server/account_bootstrap.rs`
   - 将有效 HOME 门禁抽为可单测的纯决策函数；门禁仍位于 Raccoon/CatPaw/AutoClaw importer 调用之前。
   - 新增缺失 HOME 时不进入导入序列的测试。
4. `.github/workflows/android-build.yml`
   - Windows Shared Core job 增加 15 分钟 timeout。
5. 文档
   - 修正主 HANDOFF 当前基线/候选状态，新增 §31。
   - 更新 Core Worker Lifecycle Audit 状态并新增 §12。
   - 新增 `CI_RELEASE_GATES.md` 与 `PLATFORM_CAPABILITY_AUDIT_4D3C_D.md`。

## 审计决定

- 不把 Desktop/headless 环境变量相对路径一刀切改成绝对路径：保留 R39 前兼容语义。
- 不把默认 `./.agate` 存储 fallback 与旧桌面数据扫描混为一谈；若要移除，先定义无 HOME 的稳定持久目录及迁移策略。
- 不新增通用 NetworkProvider / ClockProvider；当前 std/Tokio 时间和网络 API 已跨平台，真实 OS 差异由 `cfg` 与 ProcessAdapter 管理。
- Windows check 应成为 required status check，但 GitHub branch protection/ruleset 需要仓库管理员远端配置；本源码无法证明该设置已启用。

## 验证状态

已完成：源码差异核对、YAML 解析、LF/文件尾检查、Patch dry-run/apply、Patch 应用后 771/771 文件逐文件 SHA-256 一致。

未完成：Cargo/rustc/rustfmt/Gradle 在本执行环境不可用；未运行 Rust 编译/测试、Windows 编译、Android ABI、Gradle/JVM、APK、Instrumentation、模拟器或真机测试。R39 仍是最后已验证基线。
