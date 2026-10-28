plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "dev.agate.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.agate.android"
        minSdk = 26
        targetSdk = 35
        // 主版本号与上游内核同步溯源（方案 §2）
        versionCode = 3
        versionName = "0.4.0-m3"

        // ── 只打 arm64-v8a ────────────────────────────────────
        // 内核 .so 只有 arm64 一份（`.github/workflows/android-build.yml` 的
        // cargo-ndk 目标 + `scripts/build-android-so.sh` 都只编 aarch64）。
        //
        // 不加这行的话，AGP 会按依赖库里的 ABI 打出四架构 APK，而
        // armeabi-v7a / x86 / x86_64 里**没有 libagate_server.so** ——
        // `Gateway.ensureLoaded` 直接 `System.loadLibrary`，外面没有
        // try/catch，于是那些设备的表现是「装得上、一开就崩」，比装不上更难查。
        //
        // 限定之后：32 位设备在安装阶段就被挡下（INSTALL_FAILED_NO_MATCHING_ABIS），
        // 失败点更靠前、原因更明确；顺带把另外三个 ABI 的 androidx .so 从包里去掉。
        // 将来若要支持其它架构，得先把 build-android-so.sh 的 target 补上，
        // 再改这里 —— 两处必须一起改。
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    // ── 固定签名（debug）──────────────────────────────────────
    // 为什么必须有：debug 构建默认用 `~/.android/debug.keystore`，那是**机器本地
    // 自动生成**的 —— CI 每次跑在全新 runner 上，等于每次换一把新证书。Android
    // 只允许「同签名」覆盖安装，签名一变就只能卸载重装，**账号与配置全丢**，
    // 调试时无法连续迭代。
    //
    // 这里把 keystore 提交进仓库（debug 用，非发布凭证），本地与 CI 因此签出
    // 同一个证书，`adb install -r` / 直接点安装都能覆盖升级、数据保留。
    signingConfigs {
        create("stableDebug") {
            storeFile = file("agate-debug.jks")
            storePassword = "agatedebug"
            keyAlias = "agate"
            keyPassword = "agatedebug"
        }
    }

    buildTypes {
        release {
            // 体积控制：R8 裁掉未用代码（material-icons-extended 那类
            // 「几千个图标对象」在 release 里会被按可达性剔掉）+ 资源收缩。
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
        debug {
            // debug 保持不优化：Compose 预览/调试信息齐全，体积不代表发布体积
            isMinifyEnabled = false
            // 固定签名：覆盖安装不丢数据（见上面 signingConfigs 的说明）
            signingConfig = signingConfigs.getByName("stableDebug")
        }
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation(project(":core-gateway"))

    // M2 设计系统（版本线与 aigw fork 一致：BOM 2024.12.01 / Compose 1.7.x）
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.navigation:navigation-compose:2.8.5")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    // M3 SSE 事件流（/api/events，方案 §3.2）
    implementation("com.squareup.okhttp3:okhttp-sse:4.12.0")

    // 纯数据层解析（SSE 报文 → 思维链 / 工具调用）用 JVM 单测覆盖：
    // 这类解析的输入是「可能被截断的历史报文」，边界比界面多得多。
    // 必须补一份**真实**的 org.json：android.jar 里的 JSONObject 在单元测试中
    // 只是一组抛 "not mocked" 的桩，不补的话所有解析断言都跑不起来。
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
}
