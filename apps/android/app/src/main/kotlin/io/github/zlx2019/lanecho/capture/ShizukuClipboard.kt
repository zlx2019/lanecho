package io.github.zlx2019.lanecho.capture

import android.content.ClipData
import android.content.Context
import android.os.Build
import android.os.IBinder
import io.github.zlx2019.lanecho.core.util.Log
import org.lsposed.hiddenapibypass.HiddenApiBypass
import rikka.shizuku.Shizuku
import rikka.shizuku.ShizukuBinderWrapper
import rikka.shizuku.SystemServiceHelper

/**
 * Clipboard access through Shizuku's privileged service: calls IClipboard
 * directly as shell (uid 2000), so no overlay and no focus stealing.
 *
 * IClipboard.getPrimaryClip is a hidden API whose signature gained parameters
 * across releases; rather than pin one shape we pick the first overload whose
 * argument list we can satisfy. Every call is reflective and guarded — a
 * signature change on a future release degrades to "unavailable", never a
 * crash.
 */
object ShizukuClipboard {

    private const val PERMISSION_REQUEST_CODE = 4242

    init {
        // Reflection onto @hide members is blocked from Android 9 on unless
        // the caller opts out of the denylist
        if (Build.VERSION.SDK_INT >= 28) {
            runCatching { HiddenApiBypass.addHiddenApiExemptions("") }
        }
    }

    /** True when Shizuku is running and has granted us permission. */
    fun isReady(): Boolean = runCatching {
        Shizuku.pingBinder() && Shizuku.checkSelfPermission() == android.content.pm.PackageManager.PERMISSION_GRANTED
    }.getOrDefault(false)

    /** True when the service is up but the user has not approved us yet. */
    fun needsPermission(): Boolean = runCatching {
        Shizuku.pingBinder() && Shizuku.checkSelfPermission() != android.content.pm.PackageManager.PERMISSION_GRANTED
    }.getOrDefault(false)

    fun requestPermission() {
        runCatching { Shizuku.requestPermission(PERMISSION_REQUEST_CODE) }
    }

    /** Read the primary clip text, or null when unavailable/empty/non-text. */
    fun readText(context: Context): String? = runCatching {
        val service = clipboardService() ?: return null
        val clip = invokeFirstMatching(service, "getPrimaryClip") as? ClipData ?: return null
        if (clip.itemCount == 0) return null
        clip.getItemAt(0).coerceToText(context)?.toString()?.takeIf { it.isNotEmpty() }
    }.onFailure { Log.warn("shizuku clipboard read failed: ${it.message}") }.getOrNull()

    /** Write text through the privileged service; false when unavailable. */
    fun writeText(text: String): Boolean = runCatching {
        val service = clipboardService() ?: return false
        val clip = ClipData.newPlainText("lanecho", text)
        invokeFirstMatching(service, "setPrimaryClip", clip)
        true
    }.onFailure { Log.warn("shizuku clipboard write failed: ${it.message}") }.getOrDefault(false)

    // The wrapped binder makes the transaction cross as shell rather than as us
    private fun clipboardService(): Any? = runCatching {
        val binder: IBinder = ShizukuBinderWrapper(SystemServiceHelper.getSystemService("clipboard"))
        val stub = Class.forName("android.content.IClipboard\$Stub")
        stub.getMethod("asInterface", IBinder::class.java).invoke(null, binder)
    }.onFailure { Log.warn("shizuku clipboard service unavailable: ${it.message}") }.getOrNull()

    /**
     * Call [name] on the hidden interface, filling trailing arguments the
     * running release added (callingPackage, attributionTag, userId,
     * deviceId): Strings get our package/null, ints get 0.
     */
    private fun invokeFirstMatching(service: Any, name: String, vararg leading: Any?): Any? {
        val candidates = service.javaClass.methods
            .filter { it.name == name && it.parameterTypes.size >= leading.size }
            .sortedBy { it.parameterTypes.size }
        for (method in candidates) {
            val types = method.parameterTypes
            val args = arrayOfNulls<Any?>(types.size)
            leading.forEachIndexed { index, value -> args[index] = value }
            var satisfied = true
            for (index in leading.size until types.size) {
                args[index] = when (types[index]) {
                    String::class.java -> if (index == leading.size) PACKAGE else null
                    Int::class.javaPrimitiveType -> 0
                    else -> { satisfied = false; break }
                }
            }
            if (!satisfied) continue
            runCatching { return method.invoke(service, *args) }
                .onFailure { Log.warn("hidden $name(${types.size}) failed: ${it.cause?.message ?: it.message}") }
        }
        return null
    }

    private const val PACKAGE = "io.github.zlx2019.lanecho"
}
