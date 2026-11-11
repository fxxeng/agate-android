plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.agate.core"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
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
    implementation("androidx.core:core:1.13.1")
    // libagate_server.so 经 src/main/jniLibs/arm64-v8a/ 进入 APK（build-android-so.sh 产出）
    // M0 无运行时依赖；Retrofit/kotlinx.serialization 在 M3 接数据面时加入
    testImplementation("junit:junit:4.13.2")
    // JVM 单测 classpath 需要真实 org.json 实现：否则拿到 android.jar stub，
    // 每个方法 throw RuntimeException("Stub!")（Batch 4B-2B-7 红跑根因，R24）
    testImplementation("org.json:json:20240303")
}
