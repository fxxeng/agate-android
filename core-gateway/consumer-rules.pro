# consumer-rules.pro：JNI 类 dev.agate.core.Gateway 的 native 方法名由 JVM 按
# 「类名+方法名」解析；宿主 app release 已开启 R8。这里保留最小规则，保证
# 单独集成 core-gateway 的使用方也不会把 JNI 入口裁掉（R50/T7 修正过时注释）。
-keepclasseswithmembernames class * { native <methods>; }
-keep class dev.agate.core.Gateway { *; }
