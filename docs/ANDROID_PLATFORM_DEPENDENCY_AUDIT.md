# Android 平台依赖审计（第一轮）

> 范围：`agate-server/src` 中环境变量、用户目录和桌面客户端文件探测。本文记录源码事实与风险，不把“可能在 Android 上失败”写成已复现故障。当前环境没有 Rust/Android 构建工具，本审计尚未经过设备验证。

## 结论

1. Android 主配置目录已通过 `paths::set_config_dir_override()` 显式注入；`config_dir()` 不再依赖 Android 的 `HOME` 回退。这只覆盖 Agate 自身主数据目录。
2. Provider 的 Token、Cookie、Endpoint、开关、模型选择等环境变量读取，属于既有配置能力，不应为了 Android 化而整体删除。
3. 桌面客户端导入、桌面安装目录探测、用户主目录下旧文件迁移，是另一类平台依赖。它们需要逐个确认 Android 是否应该支持、是否只应显示“不支持”，不能和 Provider 环境配置混为一谈。
4. 已确认账号启动迁移 `account_bootstrap::run()` 在 `ServerState::bootstrap()` 的正常路径被调用；它会触发 Raccoon、CatPaw、AutoClaw 等旧数据导入。部分路径解析在 `HOME` / `USERPROFILE` 缺失时回退到相对路径 `.`。这是本轮最值得优先处理的风险点：在没有用户主目录的运行环境中，文件探测目标会变成当前工作目录，而不是明确的用户数据目录。
5. 目前不建议直接把所有 `home_dir()` 改成 `config_dir()`：这会改变桌面端导入协议与文件布局，也可能把旧数据迁移到错误位置。
6. 已在 `paths::user_home_dir()` 增加“不回退到 `.`、只接受绝对目录”的解析，并在 `account_bootstrap::run()` 的三家 Provider 旧数据导入之前增加统一门禁：没有有效 `HOME` / `USERPROFILE` 时跳过这组三家自动导入，且不调用它们的导入函数，因此不写 `raccoonImported` / `catpawImported` / `autoclawImported` 标记。该门禁不影响前面的 Agate 主账号迁移、凭证加密和模型规则迁移。

## 分类清单

| 类别 | 源码位置 / 代表变量 | 语义 | Android 处理建议 |
|---|---|---|---|
| Agate 主数据目录 | `paths.rs`: `AGATE_PROXY_HOME`, `WORKBUDDY_PROXY_HOME`, `CONFIG_DIR_OVERRIDE` | SQLite、日志、配置、备份的根目录 | 已完成 Android 显式注入；保留桌面/headless 环境变量优先级 |
| Provider 运行配置 | `server/config/parse.rs`、各 `providers/*` 中 `*_TOKEN`、`*_COOKIE`、`*_BASE_URL`、`*_REGION`、开关等 | 用户显式配置的凭证/端点/行为开关 | 保留；后续统一配置入口时要保持 env override 语义 |
| 管理/服务运行配置 | `AGATE_HOST`、`AGATE_PANEL_PORT`、`AGATE_ADMIN_PASSWORD*`、`WORKBUDDY_UPDATE_*` | headless 服务监听、管理面板、更新 | 逐项判断 Android 是否由 Kotlin 设置页接管；不要全局删 env |
| 桌面用户目录 | `USERPROFILE` / `HOME`，见 `paths.rs`、`import_ccswitch.rs`、`clash.rs`、`qoder/risk.rs` 等 | 桌面用户目录推导、CLI 配置定位 | 每个调用点必须明确平台意图；Android 不应隐式回退到 `.` |
| Windows 桌面目录 | `APPDATA`、`LOCALAPPDATA`、`ProgramFiles`、`ProgramFiles(x86)` | AutoClaw、Kuku、OrcaTerm、Qoder 等 Windows 客户端数据/安装路径 | Windows 专用路径应由 `cfg(windows)` 或显式“不支持”结果保护；保留 Windows 行为 |
| 桌面进程/系统命令 | `qoder/risk.rs` `runtime-info` 子进程；`zcode/activation.rs` 的 `tzutil` / `reg` | 本机客户端能力、Windows 系统操作 | 不应把桌面可用性直接等同于 Android 可用性；接口应返回明确不支持/不可用 |

## 已确认的调用链风险

### 1. 启动期账号导入会运行在 Android Core bootstrap 中

- `server/mod.rs` 在 `!upgrade_pending` 时调用 `account_bootstrap::run(&store)`。
- `account_bootstrap.rs` 随后调用：
  - `store.import_legacy_raccoon_data()`
  - `store.import_legacy_catpaw_data()`
  - `store.import_legacy_autoclaw_data()`
- 这些导入器定位旧数据时，部分实现读取 `USERPROFILE` / `HOME`，且在变量缺失时使用 `PathBuf::from(".")`。
- `raccoon_import.rs` 的 `legacy_directory()` 明确采用 `USERPROFILE` → `HOME` → `.`；`raccoon/credentials.rs` 的 `home_dir()` 同样回退 `.`。
- `autoclaw/credentials.rs` 的 `home_dir()` 也回退 `.`；AutoClaw 的 Windows `APPDATA` 来源有 `cfg!(windows)` 保护，但备用 `~/.openclaw-autoclaw/openclaw.json` 路径仍依赖 HOME。

**风险判断：** 当 Android 进程没有 `HOME` / `USERPROFILE` 时，旧实现可能探测相对路径。当前已在启动期三家 Provider 自动导入入口加门禁：无有效绝对主目录时整组跳过且不落导入标记。底层 Provider 路径函数及手动导入路径仍有 `.` 回退，尚未全面收敛；当前没有设备证据证明 Android 运行时一定缺少 HOME，因此这属于已加防护但仍待真机确认的平台边界风险，而不是已复现事故。

### 2. 主配置目录与 Provider 私有目录不是同一个概念

`paths::config_dir()` 已优先使用 Android 注入目录；但 Provider 的 `home_dir()`、`desktop_auth_file()`、`gateway_config_file()` 等仍有各自路径规则。例如：

- `providers/raccoon/credentials.rs`：`~/.box-agent/config/auth.json`
- `providers/catpaw/credentials.rs`：`~/.meituan-catpaw/...`
- `providers/autoclaw/credentials.rs`：`%APPDATA%/AutoClaw` 或 `~/.openclaw-autoclaw/openclaw.json`
- `providers/qoder/risk.rs`：用户目录 `.qoder*` 下的组件探测

不要仅凭 `config_dir()` 已注入，就认为上述路径也已迁移到 Android app-private data。

## 下一步实施顺序

### P1-A：先收紧自动导入路径，不改变桌面行为

1. **部分完成**：新增 `paths::user_home_dir()`，无有效绝对 HOME 时返回 `None`，不回退到 `.`；目前仅启动期自动导入入口使用它，Provider 内部旧路径函数尚未全部替换。
2. **部分完成**：启动期没有有效用户目录时，跳过 Raccoon / CatPaw / AutoClaw 整组旧数据导入，不调用导入器、不写三个一次性导入标记，并记录日志；尚未为每个导入器提供结构化 `skipped: no_user_home` 返回值。
3. 对 Windows 客户端来源保持现有 `cfg(windows)` / DPAPI 语义；Android 不尝试模拟 `%APPDATA%`。
4. 对 `HOME` 明确存在但指向 Android app-private 目录的情况，仍需产品决策：是否允许用户把旧文件主动放到该目录；不要静默把 `config_dir()` 与 HOME 合并。
5. **新增纯函数单元测试源码**：覆盖 USERPROFILE/HOME 选择、缺失、空白和相对路径拒绝；Cargo 不可用，尚未实际执行。

### P1-B：桌面能力可用性边界

- Qoder `runtime-info` 子进程调用在 Android 是否支持，需要设备上验证可执行文件格式、权限、SELinux 与 ABI；不能只看 Rust 的 `Command::spawn()` 编译成功。
- ZCode 的 `tzutil` / `reg` 是 Windows 命令，应确保调用前的能力探测不会在 Android 暴露成可用状态。
- 对仅限 Windows/macOS 的客户端登录态导入，API 应返回“当前平台不支持桌面客户端自动导入”，而不是泛化成“未找到文件”。

### P2：环境变量契约收敛

- 为每个变量标记 `host runtime config`、`provider credential/config`、`desktop path discovery`、`test-only`。
- 只有第一类需要 Kotlin 宿主配置对象替代；第二类保留兼容入口；第三类走平台 adapter；第四类不进入 Release 运行配置。
- 对 Android API Key 和 config-dir override 增加真实 Rust 并发/回归测试。

## 本轮未做的事

- 未批量替换 `std::env::var`。
- 未把 Provider 凭证搬入 Kotlin。
- 未把桌面导入路径改成 Android 数据目录。
- 未声称 Android HOME 环境变量的实际值；需要设备日志采样确认。
- 未运行 Cargo、Gradle、Android SDK 或真机测试。


## 本轮实施记录（启动期导入防护）

- 新增 `paths::user_home_dir()`：优先 USERPROFILE，其次 HOME；拒绝空白、相对路径和 NUL，不回退 `.`。
- `account_bootstrap::run()` 在 Raccoon / CatPaw / AutoClaw 自动导入前检查主目录；无有效目录时记录提示并直接跳过三家导入，避免错误写入一次性标记。
- 新增解析逻辑测试源码，覆盖有效绝对路径、HOME fallback、缺失、空白和相对路径。
- 验证：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 通过；Cargo/Rustc/rustfmt 不可用，未编译、未运行 Rust 测试；未做 Android 真机验证。
- 限制：这是启动期自动导入的入口防护，不代表 Provider 手动导入或所有桌面路径探测都已去除 `.` fallback。显式旧目录配置支持也尚未扩展。
