plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.agate.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.agate.android"
        minSdk = 26
        targetSdk = 35
        // 主版本号与上游内核同步溯源（方案 §2）；M0 从 0.3.0 起步（对齐内核 3.0.x 线）
        versionCode = 1
        versionName = "0.3.0-m0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
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
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
}
