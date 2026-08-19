// All plugin versions are declared once here (apply false); modules apply by
// alias without versions, otherwise Gradle sees the same artifact arriving
// twice and refuses with "already on the classpath"
plugins {
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.android.application) apply false
}
