# Batch 4D-3E / 4E 资源所有权审计 — R41 候选

- 基线：R40 `d3e049e89b932325e49c57b5f6a5a23c825bcca8`
- 分支：`verify/arch-8faa5f5`
- 状态：源码审计与候选修正；**未经过 CI**
- 候选归属：R41 本地候选（未推送远端；代码改动已合入 R42 ad1bc9cef3）

## 1. 本批 R40 收口

| 问题 | R41 候选处理 | 验证边界 |
|---|---|---|
| Windows-only absolute-path 测试未执行 | Windows job 增加 `cargo test --lib` | 待 Windows CI 实跑 |
| Linux `resolve_absolute_path` dead_code warning | `cfg_attr(not(windows), allow(dead_code))` 精确限定 | 待 Linux CI warning 对账 |
| `.agate` fallback 文字与 auth.json 读取路径不一致 | 明确区分 HOME-discovered Provider imports 与 config-dir-local `auth.json` migration | 静态调用链核对；未改变运行行为 |
| R40 文档过期状态/数字 | 回填 Run `42199123476` 与 772/772 | 文档静态检查 |

## 2. 资源所有权现状

### 2.1 Core 实例

源码中大量 `OnceLock` 与 static Mutex/RwLock 表示当前 Shared Core 是进程级状态集合。`CONFIG_DIR_OVERRIDE`、日志 Store、模型快照、数据库句柄、Sidecar sender/状态等都不是按可销毁 `CoreInstance` 对象统一持有。因此不能假设同一进程内可安全地释放后重新 bootstrap 一个完全独立的 Core。

### 2.2 数据目录

`config_dir()` / `kernel_dir()` 负责解析路径；没有发现它们自身提供跨进程 owner lock。SQLite 自身的数据库锁不能保护同目录下的 `config.json`、`kernel-state.json`、provider YAML、Sidecar `config.yaml` 等普通文件。两个进程若指向同一 config dir，仍可能出现迁移并发、文件覆盖、状态快照互相踩踏。

### 2.3 Sidecar

`sub_kernel` 已有 `TaskScope::Sidecar`、sender take/shutdown barrier、`SHUTTING_DOWN` 与 `runtime::CHILD`。这些机制约束单进程内部 worker 和 Child 的生命周期；它们不是跨进程互斥。第二个 Agate 进程仍可能独立启动 worker 并尝试操作同一 kernel_dir。

### 2.4 端口

`runtime::port_free()` 使用 `TcpListener::bind()` 探测后立即 drop。探测成功与 mihomo 后续 bind 之间存在 TOCTOU 窗口；该函数只提供瞬时可用性观察，不构成 reservation。固定控制端口/测试端口/节点端口需要明确谁持有监听 socket、谁负责传递实际端口。

### 2.5 SQLite

数据库连接和 SQLite 文件锁可以处理数据库层并发访问，但无法代替整个 data-dir owner lock。不要把「SQLite 可并发」推导为「同目录所有状态都支持多实例」。

## 3. 4E 推荐实施切片

### 4E-A：Data-dir owner lock（优先）

- 在任何数据库迁移、配置写入、Sidecar worker spawn 之前取得锁。
- 锁范围是规范化后的实际 data-dir 身份；路径尚不存在时需定义稳定父目录/文件身份策略，不能仅靠字符串比较。
- 同进程重复 bootstrap：明确为幂等复用同一 owner，或返回稳定的 already-initialized 结果；不允许重复启动第二套后台任务。
- 跨进程同目录第二 owner：快速失败，返回稳定错误，并保证失败路径不触发迁移、写配置、监听端口或启动 Sidecar。
- 不同 data-dir：允许独立进程并行。

### 4E-B：崩溃恢复与平台实现

- 优先使用 OS 文件锁/原语，避免只创建一个永不自动释放的 lock 文件。
- 若协议需要记录 PID，PID 只能作诊断信息，不能单独作为 stale 判断依据；必须考虑 PID reuse、进程启动时间/boot identity 和锁内核持有状态。
- 覆盖 Android/Linux、Windows；macOS/Desktop 可沿用同一抽象但必须单独 CI 编译验证。
- 正常退出释放锁；SIGKILL/崩溃后由 OS 自动释放 owner lock。

### 4E-C：端口 reservation

- 对动态端口优先由最终监听者 bind `127.0.0.1:0` 并持有 listener，或让 mihomo 自己选择端口并通过受控通道回报。
- 若业务必须固定端口，不能继续把 bind-probe/drop 当成保留；需要在锁保护下完成分配，并接受外部进程仍可抢占时的 bind 失败重试。

### 4E-D：Sidecar 状态归属

- `kernel-state.json`、`providers/*.yaml`、`config.yaml` 的写者必须是 data-dir owner。
- 现有 Sidecar supervisor 保持单进程任务取消与 Child join；不要把它替代为文件锁，也不要将 Gateway stop/Android Service onDestroy 等同 Core shutdown。

## 4. 必须新增的测试矩阵

| 场景 | 预期 |
|---|---|
| 同进程并发 bootstrap 同目录 | 只有一个 Core owner / 一组后台任务 |
| 两个进程竞争同目录 | 一个成功，另一个稳定拒绝；拒绝方不写数据 |
| 两进程不同目录 | 均可启动 |
| owner 正常退出 | 第二进程可取得锁 |
| owner 被强杀 | OS 释放锁，第二进程可取得锁 |
| 锁目录路径别名/符号链接 | 按同一真实目录识别，避免绕过 |
| Sidecar 同目录第二 owner | 第二 owner 不得读写 state/config/cache 或 spawn child |
| 端口被外部进程抢占 | 明确失败/重试，不误报已保留 |

## 5. 本批不做的事

- 不引入进程内全局 Mutex 并声称实现跨进程互斥。
- 不删除 `./.agate` 历史 fallback；迁移方案未定义。
- 不把 Android Gateway/Foreground Service 生命周期映射为 Core release。
- 不把真机验收作为 4E-A 代码开发前置阻塞；CI 证据与真机运行证据分开记录。

## 6. R41 验证状态

R41 本地候选（未推送远端；代码改动已合入 R42 ad1bc9cef3）。未执行 Cargo、Windows test、Android ABI、Gradle/JVM、Instrumentation、模拟器或真机验证。R40 Run `42199123476` 是最后已验证基线。


## 7. R42 主线范围调整

R41 的 4E 审计仍可作为 Android 数据目录所有权设计输入，但后续不再实现跨平台锁抽象。Agate 当前唯一产品目标为 Android：4E-A 优先实现 Android/Linux `flock` 类 OS advisory lock，覆盖 Android app-private data dir；Linux host 只用于可移植单测。Windows/macOS/Desktop 锁实现不属于当前主线，不得为此引入跨平台锁依赖或阻塞 Android 进度。

Windows Shared Core CI 已从 Android workflow 移除。桌面端条件编译代码仅在确认 Android 不需要且删除不影响共享 Provider/数据兼容后，才逐项隔离或移除。
