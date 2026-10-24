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
    // libagate_server.so 经 src/main/jniLibs/arm64-v8a/ 进入 APK（build-android-so.sh 产出）
    // M0 无其它依赖；Retrofit/kotlinx.serialization 在 M3 接数据面时加入
}
