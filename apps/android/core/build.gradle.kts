plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.bcpkix)
    testImplementation(kotlin("test"))
}

tasks.test {
    useJUnitPlatform()
}
