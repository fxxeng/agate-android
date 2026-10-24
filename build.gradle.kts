// 根构建脚本：只声明插件版本（AGP/Kotlin 与用户其它项目一致的可验证组合）
plugins {
    id("com.android.application") version "8.7.3" apply false
    id("com.android.library") version "8.7.3" apply false
    id("org.jetbrains.kotlin.android") version "2.1.0" apply false
}
