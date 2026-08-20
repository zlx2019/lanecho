plugins {
    alias(libs.plugins.android.application)
    // AGP 9 ships built-in Kotlin: org.jetbrains.kotlin.android must NOT be
    // applied (hard error). The compose compiler plugin still comes from the
    // Kotlin plugin family
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "io.github.zlx2019.lanecho"
    compileSdk = 37

    defaultConfig {
        applicationId = "io.github.zlx2019.lanecho"
        // TLS 1.3 is a protocol floor and the system stack enables it from
        // API 29 (decision DA4)
        minSdk = 29
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
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

    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(project(":core"))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.material3)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.material.icons.extended)
    implementation(libs.shizuku.api)
    implementation(libs.shizuku.provider)
    implementation(libs.hiddenapibypass)
}
