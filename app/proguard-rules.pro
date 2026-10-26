# ── JNI 边界 ────────────────────────────────────────────────
# Gateway 的 native 方法是 external 声明，名字由 JNI 按「类名+方法名」解析；
# R8 改名会让 System.loadLibrary 之后的注册全部找不到符号。
-keepclasseswithmembernames class * { native <methods>; }
-keep class dev.agate.core.Gateway { *; }
-keep class dev.agate.core.AgateForegroundService { *; }

# 内核回调进 Kotlin 的入口（日志回调等）
-keep class dev.agate.core.** { *; }

# ── 序列化/反射面 ──────────────────────────────────────────
# 我们全程手写 org.json 解析，没有反射式 DTO，故不需要保留字段名。
# 若将来引入 kotlinx.serialization / Gson，请在此补 keep 规则。
-dontwarn org.json.**

# ── Compose ────────────────────────────────────────────────
# AGP 自带 compose 规则；此处仅保留运行时反射用到的入口
-keep class androidx.compose.runtime.** { *; }
