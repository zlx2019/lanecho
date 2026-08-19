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

// :core is pure Kotlin/JVM (protocol/transport/history) and tests without an
// Android SDK; :app is the Compose shell plus platform glue.
include(":core")
include(":app")
