pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
        google()
    }
}

dependencyResolutionManagement {
    repositories {
        mavenCentral()
        google()
    }
}

rootProject.name = "lanecho-android"

// :core is pure Kotlin/JVM (protocol/transport/history); the Android app
// module arrives with the UI milestone and requires a local Android SDK.
include(":core")
