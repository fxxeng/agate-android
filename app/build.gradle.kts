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
}
