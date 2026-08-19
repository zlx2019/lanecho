package io.github.zlx2019.lanecho.core.util

// Minimal pluggable logging seam: the core module must not depend on
// android.util.Log, and java.lang.System.Logger is not available on every
// supported Android API level. The app module installs a logcat sink
object Log {
    @Volatile
    var sink: (level: String, message: String) -> Unit = { level, message ->
        println("[$level] $message")
    }

    fun info(message: String) = sink("info", message)
    fun warn(message: String) = sink("warn", message)
}
