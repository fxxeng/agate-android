# Agate — Android AI Gateway

Rust `agent2api-server` 内核原样复用（交叉编译为 `libagate_server.so`）+ Kotlin / Jetpack Compose 原生壳。
设计基线：`123_v44.html` 交互规范 × agent2api 上游源码。完整方案与里程碑见 [docs/Agate移植方案-v3.0.md](docs/Agate移植方案-v3.0.md)。

**状态**：M0 实施中（2026-10-24）——上游 3.0.8 内核副本已入库（agent2api-server/）、JNI 桥（src/jni_bridge.rs）、core-gateway（Gateway.kt + 前台服务）、Gradle 骨架与 CI（.github/workflows/android-build.yml）就绪，CI 已改为仅手动触发（Actions 免费额度耗尽，额度每月重置后手动点构建），编译验证挂起待做。决策与偏差见 [docs/M0-决策记录.md](docs/M0-决策记录.md)。

## 目录结构（对应方案第 4 节）

```
app/                 Compose UI + ViewModel + Repository + Retrofit
  ui/theme           深/浅 ColorScheme + v44 语义色 + tnum 等宽数字
  ui/components      12 个通用组件（拖拽行/热力图/双轴图/手风琴卡…）
  ui/home            Tab1 控制台 + 新手向导 + 健康自检
  ui/provider        Tab2 提供商：列表/拖拽 + 详情三 Tab + 新增 + 选择器 Sheet
  ui/models          Tab3 模型：xp-card 清单 + 聚合/测速/映射/能力位/目录刷新
  ui/tasks           Tab4 任务中心：待办/福利/验证码池/定时签到/保活链/台账
  ui/quota|reports|logs|proxy|routing|assistant|settings   二级页
  navigation/        AgateNavHost 路由表
  data/              ApiService + GatewayRepository（单一数据源）
  auth/              AuthWebViewSheet + CaptchaBottomSheet
core-gateway/        JNI 桥接 + .so 加载 + AgateForegroundService + AlarmReceiver
agent2api-server/    上游 Rust 内核（+cdylib 与 jni_bridge.rs，<280 行非侵入式）
scripts/             M0 交叉编译脚本落点（build-android-so.sh）
docs/                方案文档
```

## 命名规范

| 维度 | 值 |
| :--- | :--- |
| Application ID | `dev.agate.android` |
| core-gateway 模块命名空间 / JNI 类 | `dev.agate.core` / `dev.agate.core.Gateway` |
| Native 库 | `libagate_server.so`（arm64-v8a，x86_64 视需要） |
| 数据目录 | `/data/data/dev.agate.android/files/agate/` |
| APK 产物 | `Agate-v{version}-arm64-v8a-release.apk`（主版本号与上游内核同步） |

## 开放问题（立项前拍板，详见方案第 1 节）

O1 与现有 aigw 的关系（当前按并行项目处理）· O2 上游版本口径（3.2/3.3 vs 本地实证 3.0.8）· O3 WebView OAuth 风控 · O4 Doze 离屏过码 · O5 rustls NDK 工具链 · O6 API 面完整性
