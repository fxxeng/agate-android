# R43 · T2 Rust 编译覆盖与 Android 主线范围审计

- 日期：2026-11-07
- 基线：R42 `ad1bc9cef3a257d9776e1bcf89fac3da5528f649`（CI Run `42235540958` 通过）
- 分支：`verify/arch-8faa5f5`
- 性质：**只读审计**。本文件不改任何源码与 workflow；门禁调整见 §7，需委托方确认后另行提交。

## 1. 结论摘要

| 计划 §4.1 的问题 | 结论 | 关键证据 |
|---|---|---|
| ① bin 是否被 Android Phase 2 / 设备诊断 / 开发验收实际使用 | **否** | `Gateway.kt:28` 只 `System.loadLibrary("agate_server")`；`scripts/build-android-so.sh:31` 只 `--lib` |
| ② 是否存在独立于 `ServerState::bootstrap()` 的初始化流程 | **不存在** | `bin:81` 与 `jni_bridge.rs:221` 是同一入口，前后动作都在 bootstrap 之外只做宿主适配 |
| ③ 是否存在绕过统一数据目录锁的入口 | **当前无锁，故无旁路；T3 把锁放进 bootstrap 即同时覆盖两个宿主** | 两宿主均经 `bootstrap`；bin 不自开库，只用 `state.db()` |
| ④ bin 是否只服务桌面 Host | **否**：它是 headless / 容器宿主形态；但**本仓无 Dockerfile、无 CI 构建、无任何交付路径** | `bin:1-33` 模块头（0.0.0.0、`AGATE_PANEL_PORT`、`AGATE_SELF_UPDATE`、SIGTERM）；全仓 find 无 Dockerfile |

**门禁决定**：bin **不**纳入 Android 主线阻断门禁（按计划 §4.2 第二分支处理）。理由与备选 diff 见 §7。

## 2. ① bin 与 Android 的真实关系

| 事实 | 位置 |
|---|---|
| Android 只加载 cdylib `libagate_server.so` | `core-gateway/src/main/java/dev/agate/core/Gateway.kt:28` |
| 该 crate 同时声明 `crate-type = ["cdylib", "rlib"]` 与 `[[bin]] agate-server` | `agate-server/Cargo.toml` [lib] / [[bin]] |
| Android 构建脚本只编 `--lib`，只校验 `.so` | `scripts/build-android-so.sh:31`、`:34-38` |
| Gradle / Kotlin / workflow 中出现的 "agate-server" 全是**路径或注释**引用，无二进制调用 | `grep -rn agate-server --include=*.kt --include=*.gradle* --include=*.yml` |
| Phase 2 真机验收清单里没有"运行 bin"这一步 | `docs/PHASE2_REAL_DEVICE_ACCEPTANCE_CHECKLIST.md`（无 `agate-server` 命中） |
| 本仓无 Dockerfile / docker-compose，容器形态在本仓没有交付载体 | `find . -maxdepth 2 -iname 'Dockerfile*'` → 空 |

→ bin 是**上游 headless 形态的残留入口**：本仓既不构建、也不打包、也不在 CI 里编译它。它唯一实际使用者是"想在服务器上跑网关的人"。

## 3. ② 初始化流程是否统一

`agate-server/src/bin/agate-server.rs` 的主线：

```
40  fn main() -> ExitCode
50  ..  只解析 env：AGATE_PROXY_PORT / AGATE_HOST / AGATE_PANEL_PORT / AGATE_UI_DIR
81  let mut state = ServerState::bootstrap(port, host)      ← 与 Android 同一入口
94  access::set_panel_gate(true)                            ← 面板闸门（headless 专属）
158 state.set_ui_dir(ui_dir)                                ← 托管 ui/ 静态目录（headless 专属）
169 selfupdate::set_restart_hook(...)                       ← 容器内自更新重启（headless 专属）
174 let shutdown_tx = match start(&state)                   ← 与 Android 同一启动动作
212 shutdown_core(Duration::from_secs(8))                   ← 与 Android nativeStop 同一收尾
```

Android 侧（`agate-server/src/jni_bridge.rs`）：

```
219 crate::paths::set_config_dir_override(data_dir)          ← 数据目录由宿主注入
221 ServerState::bootstrap(port, IpAddr::V4(Ipv4Addr::LOCALHOST))
251 fn core_init(...)   →  nativeStart / nativeStop / nativeSchedulerSet
```

`Db::open` / `config::init` / `logging::init_store` / `spawn_worker` 的调用点全部在 `ServerState::bootstrap` 内部（`server/mod.rs:330/363/463/799`），**bin 与 jni_bridge 都没有第二套**。
唯一差异是数据目录来源：bin 读 env（`AGATE_PROXY_HOME`），Android 读 JNI 注入的 `set_config_dir_override`。

## 4. ③ 锁的覆盖面（对 T3 的硬约束）

- 两宿主都必然经过 `ServerState::bootstrap` ⇒ **锁必须落在 bootstrap 内**，落在 bin 或 jni_bridge 里都会漏掉另一条路（T3 §5.7 已如此要求，此处补上证据）。
- 退出路径有两条，锁释放要覆盖两者：bin 的 SIGTERM / Ctrl+C / 自更新重启（`bin:270` → `bin:210-212 shutdown_core`）；Android 的 `nativeStop`（`jni_bridge.rs:753 nativeStop`）。
- Android 的 Core 是进程级单例（`jni_bridge.rs:197 static CORE: Mutex<Option<Core>>`，`ensure_core` 幂等）⇒ 同进程重复 bootstrap 必须幂等返回，**不得**因为锁已被"自己"持有而报失败；跨进程才失败（T3 §5.5 的「第二进程失败」只针对不同进程）。
- 反例护栏：不得把 Android FGS / Gateway stop 当 Core shutdown（`bin` 模块头 :211 已写明同一语义）。

## 5. ④ §4.3 WorkBuddy 桌面路径审计

| 位置 | 平台门 | Android 是否可达 |
|---|---|---|
| `account_store/workbuddy_import.rs:37-47` `auth_directory_windows()`：`LOCALAPPDATA` → `resolve_absolute_path` → `CodeBuddyExtension/…/auth` | `#[cfg(windows)]` | 否 |
| `:50-62` `auth_directory_macos()`：`user_home_dir()` → `Library/Application Support/CodeBuddyExtension/…` | `#[cfg(target_os = "macos")]` | 否 |
| `:64-77` `auth_directory()` | `cfg(not(any(windows, macos)))` → **`None`** | 是，但返回 `None`（不伪造桌面目录） |
| `paths.rs:159-160` `resolve_absolute_path()` | `#[cfg_attr(not(windows), allow(dead_code))]` + `pub(crate)` | 非 Windows 下 dead |

结论与处理（**保持现状，不删不改**）：

1. `resolve_absolute_path` 的唯一生产调用点就是 `auth_directory_windows()`，本身已在 `cfg(windows)` 内 ⇒ 现有 `cfg_attr(not(windows), allow(dead_code))` 是精确标注，不是"为消 warning 留死代码"。
2. 不能删：`accounts.rs:1306-1310` 的 `importDesktop` 契约与 UI 文案仍引用这条导入链路（容器/桌面宿主仍会用到），删掉等于破坏公共契约。
3. 不能改成 `#[cfg(windows)]`：Windows CI job 已在 R42 移除（S1），把函数和它的单测一起 cfg 掉，会让 `paths.rs:295-314` 那 4 条字符串级用例在 Linux 上也不再编译 —— 那是当前**唯一**还活着的覆盖。按 §4.4，宁可保留并把边界写清楚（见 §6）。
4. 未新增任何 `pub`：函数仍是 `pub(crate)`，测试不需要生产 API 公开。

## 6. §4.4 测试边界（明确写死）

- 保留的用例（`paths.rs:295-314`）：`None` → `None`；空白 → `None`；相对路径 → `None`；含 `\0` → `None`；` /var/lib/agate ` 去掉首尾空白后成立；` C:\Users\agate\AppData\Local ` 作为**字符串**被识别为绝对路径。
- **这些只证明字符串处理，不证明 Windows 运行环境**：Linux 上 `Path::is_absolute()` 的语义与 Windows 不同，Windows runner 真跑门禁已随 R42 Android-only 收敛移除。该结论在本仓标注为「未验证」，不计入任何"Windows 通过"。
- 不给测试开 `pub`，不新增跨平台依赖，不为桌面路径测试制造新 crate 依赖。

## 7. 编译门禁：决定与备选

**现状**：bin 在**所有** CI 中都不编译 —— `android-build.yml:33` 只 `cargo check --lib`、`:35` 只 `cargo test --lib`；android job 用 `cargo ndk … --lib`。

**决定 A（本次采纳，按计划 §4.2 第二分支）**：不改 workflow。bin 不属于 Android 诊断 / Phase 2 所需，也不是本仓的交付产物，按计划要求「先交证据、由委托方确认后再动 workflow」。

> 【R44 补记】本判断随后被推翻：R43 实施时已把 `--bins` 收进 CI（`rust-tests` 与 check 两处，见 R43 报告偏差 D1），本节「决定 A」仅存档不再生效。

**决定 B（备选，未执行，需要你一句话）**：把 check 步骤放宽一位，只为防腐化：

```diff
-      - name: Portable Rust library check
-        run: cargo check --manifest-path agate-server/Cargo.toml --locked --lib
+      - name: Portable Rust library + aux bin check
+        run: cargo check --manifest-path agate-server/Cargo.toml --locked --lib --bins
```

- 收益：bin 是 `bootstrap` / `start` / `shutdown_core` 的第二宿主，R43 正要改这些接口；不编译它，T3 的签名漂移会在无人察觉的情况下把 bin 弄断。
- 成本：库已编译，bin 只有 287 行、仅链接，预计 CI +30~60s；测试步骤仍 `--lib`，不加测试时长；**不重新引入 Windows job**；`--bins` 只保证编译，**不代表 APK 含该 bin**（APK 里始终只有 `libagate_server.so`）。
- 残余风险（不确定项，不掩饰）：bin 里 `wait_for_shutdown_signal` 用 `#[cfg(unix)]` 走 SIGTERM，Linux runner 能编过；但该 bin 依赖 `io-util` / `signal` / `process` / `selfupdate`，若库侧将来把某 feature 收缩，bin 会先断。这正是采纳 B 的理由，也是 B 唯一的边际收益。

## 8. 三态

- **已实现**：无（本文件为只读审计，代码零改动）。
- **已验证**：§1–§6 全部结论均由 R42 树上的 grep / 读码得出，复现命令见附录。
- **待验证**：若采纳决定 B，`cargo check --locked --lib --bins` 必须由 GitHub CI 实跑才算通过；本机无 Rust 工具链，未执行。

> 【R44 补记】「本机无 Rust 工具链」已过时：R43 T2 后本机（Termux）已装 Rust 并实际跑过 `cargo check --lib --bins`（2026-11-07，本节撰写时点的事实已不成立）。

## 附录：复现命令

```bash
find agate-server/src/bin -maxdepth 2 -type f -print
grep -RInE "fn main|ServerState::bootstrap|Db::open|config::init|logging::init_store|spawn_worker" agate-server/src
grep -RInE "auth_directory_from_localappdata|auth_directory_from_macos_home|LOCALAPPDATA|Library/Application Support/CodeBuddyExtension" agate-server/src
grep -n "loadLibrary" core-gateway/src/main/java/dev/agate/core/Gateway.kt
sed -n '26,40p' scripts/build-android-so.sh
sed -n '26,36p' .github/workflows/android-build.yml
```
