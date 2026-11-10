# Phase 2 真机验收执行单（Agate）

> 适用基线：R32 / `91dd6c26e54080e266dafa0e3f3911e9a93c0754`。本文件是执行记录模板，不代表任何真机场景已经通过。
>
> **Android-only 范围（2026-11-10 更新）**：不考虑旧数据兼容；headless、mihomo Sidecar、内核下载属桌面/headless
> 路径，Android 产品不交付（`sub_kernel::asset_for_platform()` 对 `android/*` 返回 `None`），对应条目标「不适用」，
> 不计入通过率。
> 验收对象：Android Kotlin 生命周期控制面与 Rust Core/Gateway 的协作。不要将此验收与 RapidOCR AutoJS Plugin 或 OCR Fusion Engine 混用。
>
> **滚动验收策略（R32 后修订）：** 本清单不再是后续代码批次的前置阻塞条件。用户可在正常安装/使用 APP 的过程中逐步覆盖场景；每次发现异常即保存日志与操作时间线，按单场景记录累积证据。开发批次与设备观察并行推进。没有实际执行的场景仍标记 NOT RUN，设备限制标记 BLOCKED；Phase 2 的最终关闭仍需真实设备证据，但不阻止继续推进独立的 Native Bridge / Rust 边界工作。

## 0. 验收规则

- CI 双绿、JVM 91/91、Instrumentation 2/2 只证明对应自动化检查通过，不能替代真实设备场景。
- 每次测试记录设备型号、Android 版本/API level、ROM/厂商、电池优化状态、应用版本/commit、网络状态、日期时间及操作者。
- 对可控的重复操作场景，目标至少执行 10 次；竞争/恢复类场景建议 20 次。日常使用中自然发生的场景可先按实际次数累计，不要求为凑次数而制造高风险操作。出现一次错误即记录失败，不用多数通过覆盖偶发失败。
- 每次执行前清空或标记 logcat 起点；保留完整原始日志，不只截取异常行。
- 测试时不要同时手动操作其它 Gateway 控制入口，除非该场景明确要求并发。
- 任何需要强杀进程/系统回收的场景，区分 `force-stop`、进程被杀、Service 被销毁和设备重启；它们不是同一种生命周期事件。

## 1. 设备与构建信息

| 字段 | 记录 |
|---|---|
| 设备型号/代号 | 待填写 |
| Android 版本 / API Level | 待填写 |
| ROM / 厂商版本 | 待填写 |
| Agate APK 版本 / commit | 待填写 |
| ABI | 待填写 |
| 电池优化 | 未优化 / 已优化 / 厂商后台限制：待填写 |
| 网络 | Wi-Fi / 蜂窝 / 离线；待填写 |
| 日期与时区 | 待填写 |

## 2. 场景 A：START / STOP 意图竞争

**目的**：确认新 START 不会被旧 STOP 或旧 Service 清理路径误停。

步骤：
1. 冷启动应用，确认 Gateway 初始状态并记录。
2. 连续快速执行 START → STOP（至少 10 轮），每轮间隔可在 0–500ms 随机变化。
3. 执行 STOP 后立即 START（至少 20 轮），观察最终状态。
4. 在 Gateway 已运行时触发 STOP，并尽可能在旧 Service 正在销毁/清理的窗口内再次 START。
5. 测试结束后等待至少 30 秒，复查 Gateway health/status、前台通知及 Service 状态。

预期：
- 最后一次有效用户意图决定最终状态；旧 STOP 不得停止新 START 启动的 Gateway。
- 不出现 Gateway 已运行但 FGS/通知无故消失的状态。
- 不出现重复 FGS、重复 Gateway 实例或状态长期卡在 Starting/Stopping。

记录：每轮动作时间戳、最终 `Gateway.Status`、通知状态、Service startId/相关日志、失败轮次。

## 3. 场景 B：Scheduler 与 Core 未就绪/恢复

**目的**：确认 Scheduler desired state 可在 Core 未就绪时保留，并在 Core 恢复后重新对齐。

步骤：
1. 记录当前 Scheduler 持久化开关状态。
2. 在 Core 尚未 ready 或模拟初始化失败的条件下，切换 Scheduler 开/关；记录 UI、持久化设置与 Native status。
3. 恢复网络/依赖或重新初始化 Core。
4. 观察 Scheduler 是否按持久化设置对齐，不要求在 Core 不可用期间伪造 Native 成功状态。
5. 分别测试 desired=true 与 desired=false；每种至少 10 次。

预期：
- Core 未就绪时，失败被明确记录，不把“未能应用”报告为 Native 已成功。
- Core 恢复后，Scheduler 最终与持久化 desired state 一致。
- 重试不产生重复任务/重复调度。

限制：如果当前构建没有可控的 Core-init failure 注入入口，不要为验收临时增加生产开关；记录可复现的自然失败条件或将该子项标记为 BLOCKED，并说明原因。

## 4. 场景 C：Foreground Service destroy 与 Tick 清理

**目的**：确认 Service 正常停止、系统销毁及异常退出时，WakeLock、pending tick 和 Gateway 承载关系安全。

分别执行：
1. 正常停止：无活动 tick 时 STOP。
2. Tick 中停止：让 `checkinRunAndWait()` 正在执行时触发 STOP，确认取消请求不会被长业务阻塞。
3. 系统回收：应用退后台并在系统允许的条件下触发 Service reclaim；不要把开发者选项“立即销毁活动”当作唯一证据。
4. 异常退出：仅在可控测试环境触发进程异常终止，记录是否有系统重启/重建。

预期：
- WakeLock 最终释放；无永久持锁。
- pending tick 最终归零或按设计完成；不会因旧 startId 清理新 Service。
- Gateway 仍运行时，不因一次 Native status 读取异常而错误停止 FGS。
- Gateway 明确 stopped 后，Service 能按当前策略收敛退出。

证据：WakeLock acquire/release 日志、pending tick 计数/日志、Service onStartCommand/onDestroy、Gateway status、通知栏变化。若无法直接读取 WakeLock，明确标记“间接证据”，不要写成已直接验证。

## 5. 场景 D：进程回收、重建与 Boot/Alarm

**目的**：确认进程死亡后没有幽灵任务、重复 Service 或错误恢复。

分别执行：
1. 普通后台进程回收后，从 Launcher 重新启动应用。
2. 系统允许的条件下，验证 Boot receiver 路径（设备重启后观察）。
3. 验证 Alarm/调度触发路径；记录触发来源与时间。
4. 对比应用进程重建前后的 Gateway、Scheduler、FGS 与持久化设置。

预期：
- 不出现重复 Service、重复 Alarm/任务或幽灵 Gateway 状态。
- 恢复行为符合持久化设置和产品策略；不得仅凭进程重启就假定 Gateway 一定应自动启动。
- 权限/后台限制阻止恢复时，应有可诊断日志，而不是静默卡死。

重要：`adb shell am force-stop` 会施加 force-stop 状态并抑制部分隐式启动/广播行为，不能单独作为普通进程回收或 BOOT 恢复的通过证据。

## 6. 场景 E：SSE 临时断开、自动重连与旧回调

**目的**：验证 UI 说明与实际行为一致，并观察旧 EventSource 回调不会污染新连接。

步骤：
1. Gateway running 时确认 SSE connected。
2. 在控制页执行“临时断开事件流”，等待至少两个 5 秒探活周期。
3. 确认在 Gateway 仍 running 时自动重连（当前产品语义是临时断开，不是永久禁用）。
4. 通过网络切换/短暂断网制造断流，再恢复网络，观察 retry/poll 恢复。
5. 在连接切换窗口观察旧连接迟到回调是否改变新连接状态、重复累计 failure 或触发额外重连。

预期：
- Gateway running 时临时断开后，按当前 5 秒探活行为自动重连。
- 旧 EventSource 回调不能覆盖新连接状态。
- Native status 暂时不可用时，不得将 Unavailable 伪装成 stopped。

注：本场景设备侧能验证端到端行为，但不能仅凭 UI 观察证明每条旧回调身份分支；该分支另有 JVM 单测与 M1 mutation 证据。

## 7. 日志采集建议（ADB）

先确认设备授权并记录设备信息：

```bash
adb devices -l
adb shell getprop ro.product.model
adb shell getprop ro.build.version.release
adb shell getprop ro.build.version.sdk
adb shell dumpsys deviceidle
adb shell dumpsys package <应用包名>
```

每个场景开始前：

```bash
adb logcat -c
adb logcat -v threadtime > phase2_<场景>_<日期时间>.log
```

在另一终端执行操作；结束采集时使用 Ctrl-C。若需要保存设备状态快照：

```bash
adb shell dumpsys activity services <应用包名> > phase2_services.txt
adb shell dumpsys power > phase2_power.txt
adb shell dumpsys alarm > phase2_alarm.txt
adb shell dumpsys notification --noredact > phase2_notifications.txt
```

日志过滤可作为辅助副本，不替代完整原始日志：

```bash
adb logcat -d -v threadtime | grep -iE 'Agate|Gateway|RuntimeController|ForegroundService|AndroidRuntime|WakeLock|Alarm'
```

将 `<应用包名>` 替换为实际 applicationId。不同 Android 版本的 `dumpsys` 输出可能不同；命令失败时记录原始错误，不据此推断功能失败。

## 8. 单场景结果记录模板

| 项目 | 记录 |
|---|---|
| 场景 / 子场景 | 待填写 |
| 执行次数 / 通过次数 / 失败次数 | 待填写 |
| 起止时间 | 待填写 |
| 操作时间线 | 待填写 |
| Gateway health/status | 待填写 |
| FGS / 通知 | 待填写 |
| Scheduler desired / observed | 待填写 |
| WakeLock / pending tick | 待填写 |
| 关键日志与文件名 | 待填写 |
| 是否可复现 / 复现步骤 | 待填写 |
| 结论 | PASS / FAIL / BLOCKED（附证据） |

## 9. Phase 2 关闭门槛

只有 A–D 四类主场景均有真实设备证据，且所有失败/阻塞项均有明确处置，才可讨论 Phase 2 关闭。E 为 SSE 端到端补充验收，建议一并完成。若某项因设备/系统限制无法执行，必须标记 BLOCKED，不得折算为 PASS。

Phase 2 设备验收继续按真实使用证据滚动推进；它不阻塞独立的 Phase 4 Rust 平台解耦批次。Phase 3 的 Doze、WakeLock、Alarm/WorkManager、重启/进程死亡恢复及 ROM 长稳仍需按单独清单验收。


## 10. R33 补充：Native 日志回调模拟器证据（2026-11-04）

R33 GitHub Actions Run `41830352114` 的 Instrumentation 2/2 通过，其中 `nativeLibraryCoreRouterAndGatewayLifecycleWork` 走真实 JNI listener 注册/撤销路径，并断言注册 `ok`、撤销 `ok && cleared`。这属于 Emulator / Android Runtime 证据，不等同于真实手机长期运行证据。真机日志回调继续在 APP 使用中观察；若出现未投递、重复投递、listener 泄漏或关闭后仍回调，需保留 logcat 与复现时间线。

## 11. R49 补充：运行时正确性（2026-11-09 立项，未执行）

本节逐条对齐 R49 任务书 §9.3（24 项）；证据只能来自真实设备，CI 全绿不代表本节通过。

### 11.1 Gateway 生命周期（9）

| # | 场景 | 期望 | 状态 |
|---|---|---|---|
| G1 | 正常 START / STOP | 通知与快照一致；停止后无端口监听 | 未执行 |
| G2 | 连续快速 START | 不产生第二个监听；快照最终 Running | 未执行 |
| G3 | 连续快速 STOP | 幂等；不残留前台服务 | 未执行 |
| G4 | START 与 STOP 交替 | 最终状态与最后一次操作一致 | 未执行 |
| G5 | 端口被占用后 STOP | 停止返回可诊断结果，不误退出前台 | 未执行 |
| G6 | Native STOP 失败时 FGS 保持 | 通知「停止失败…」；WakeLock 仍持有 | 未执行 |
| G7 | STOP 复核期间新 START | 旧 STOP 不杀新启动；复核自行放弃 | 未执行 |
| G8 | LAN 模式保活资源释放正确 | 确认停止后才释放 WifiLock；失败/超时保留 | 未执行 |
| G9 | 屏幕关闭后 Gateway 状态稳定 | 息屏 10 分钟后端口仍可达、通知仍在 | 未执行 |

### 11.2 AutoCheckin（6）

| # | 场景 | 期望 | 状态 |
|---|---|---|---|
| C1 | 多计划启动补签 | 按条目顺序串行执行，全部落账 | 未执行 |
| C2 | 补签中关闭调度 | 当前计划收尾后不再启动下一条 | 未执行 |
| C3 | 当前账号收尾 | 在途 Provider 请求不被中断 | 未执行 |
| C4 | 后续计划不再启动 | 日志含「停止启动后续补签计划」 | 未执行 |
| C5 | 重新开启后正常恢复 | 按最新配置重新计算待补签，旧任务不复活 | 未执行 |
| C6 | 重复开关不产生重复任务 | 任务名带代次，无同名覆盖/重复执行 | 未执行 |

### 11.3 Sidecar（4）

| # | 场景 | 期望 | 状态 |
|---|---|---|---|
| S1 | Gateway 正常启动后自动拉起 | 订阅启用且有内核时后台拉起一次 | 不适用（Android 无 Sidecar） |
| S2 | Gateway 端口冲突时不触发自动拉起 | `ps` 无新增内核进程 | 不适用（Android 无 Sidecar） |
| S3 | 手动关闭内核后 Gateway 重启不自动拉起 | `is_manually_halted` 生效 | 不适用（Android 无 Sidecar） |
| S4 | 已运行内核不被重复重启 | 控制端口不变、进程 PID 不变 | 不适用（Android 无 Sidecar） |

> 依据：`agate-server/src/server/core/sub_kernel/mod.rs::asset_for_platform()` 只认
> `windows/macos/linux`，`android/*` 返回 `None`；`unsupported_reason()` 明确提示
> 「Android 暂不支持代理子内核」。这 4 项保留仅为历史追溯，不再要求真机执行。

### 11.4 端口（5）

| # | 场景 | 期望 | 状态 |
|---|---|---|---|
| P1 | 默认 8787 | 控制页探测 8787 | 未执行 |
| P2 | 修改端口 | 设置页保存后 `GatewayPrefs.port` 生效 | 未执行 |
| P3 | 重启 Gateway | 网关在新端口监听 | 未执行 |
| P4 | 控制页 probe 使用正确端口 | 运行态优先实际端口；停止时用配置端口 | 未执行 |
| P5 | 其他 API 页面使用正确端口 | 控制台/KernelOps 不再访问旧端口 | 未执行 |

## 12. R50 补充：运行时加固回归（2026-11-10 立项，未执行）

R50 的 STOP 相位、管理面缓存、TICK、签到超时、日志、闹钟、异步 API 与 ABI 变更需要设备侧回归；
下列 10 项不计入 §11 的 24 项。证据只能来自真实设备。

| # | 场景 | 期望 | 状态 |
|---|---|---|---|
| R50-1 | STOP 相位 | 点停止后界面进入「停止中」，最终 Stopped；不提前报已停止 | 未执行 |
| R50-2 | 管理面缓存失效 | 改端口 / host / 本地面板后，管理 API 立即走新目标，无需杀进程 | 未执行 |
| R50-3 | TICK 非粘性 | 停止网关后重启手机，网关不自动复活；手动启动正常 | 未执行 |
| R50-4 | 签到 15min 上限 | 上游挂住时 15min 返回 timeout，不无限持 WakeLock，随后可 START/STOP | 未执行 |
| R50-5 | 日志转发 | emoji 原样；监听断开重连后继续收日志；高频日志不 ANR/OOM | 未执行 |
| R50-6 | 精确闹钟权限 | 授予/撤销精确闹钟权限后，签到闹钟自动重排，不丢链 | 未执行 |
| R50-7 | 异步 API 取消 | 取消长请求后 Rust 任务结束（日志/CPU 不再增长），无「Kotlin 超时、内核还在跑」 | 未执行 |
| R50-8 | 16 MiB 响应上限 | 超限返回 `response body exceeds 16 MiB`；正常大响应可用 | 未执行 |
| R50-9 | 进程回收重建 | `adb shell am kill dev.agate.android` 后重开：Service/Core 可重建，无崩溃循环 | 未执行 |
| R50-10 | Release 交付形态 | Release APK 仅 `arm64-v8a`；版本页脚 `0.5.0-r50`；debug 可覆盖安装 | 未执行 |
