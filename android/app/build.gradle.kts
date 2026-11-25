plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "dev.assist.app"
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }

    defaultConfig {
        applicationId = "dev.assist.app"
        minSdk = 26
        targetSdk = 36
        versionCode = 2
        versionName = "0.6.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // sherpa-onnx's speech libraries, as its build script unpacked them from the archive
    // `just android` checked. Keep the version in step with sherpa-onnx-sys in Cargo.lock.
    sourceSets["main"].jniLibs.directories += "../../target/sherpa-onnx-prebuilt/jniLibs"

    packaging {
        jniLibs {
            // The core uses the C API only.
            excludes += listOf("**/libsherpa-onnx-jni.so", "**/libsherpa-onnx-cxx-api.so")
        }
    }

    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.material3)
    implementation(libs.compose.ui)
    implementation(libs.activity.compose)
    implementation(libs.coroutines.android)
    // Scanning the pairing code: CameraX frames, ZXing's decoder.
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.zxing.core)
    implementation(libs.unifiedpush)
    // The UniFFI Kotlin bindings call the Rust core through JNA.
    implementation("${libs.jna.get()}@aar")
}
