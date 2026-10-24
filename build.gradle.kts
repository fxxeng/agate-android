// 根构建脚本：只声明插件版本（AGP/Kotlin 与用户其它项目一致的可验证组合）
plugins {
    id("com.android.application") version "8.7.3" apply false
    id("com.android.library") version "8.7.3" apply false
    id("org.jetbrains.kotlin.android") version "2.1.0" apply false
    // M2 设计系统：Kotlin 2.x 自带的 Compose 编译器插件（版本与 Kotlin 一致）
    id("org.jetbrains.kotlin.plugin.compose") version "2.1.0" apply false
}
