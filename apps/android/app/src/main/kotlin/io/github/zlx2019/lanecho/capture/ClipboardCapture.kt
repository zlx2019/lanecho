package io.github.zlx2019.lanecho.capture

import android.content.Context
import io.github.zlx2019.lanecho.core.sync.BackgroundReadMethod
import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.platform.ClipboardPort
import io.github.zlx2019.lanecho.state.AppState

/**
 * Background clipboard capture (K6): turns "you copied something" into a
 * broadcast without the app being in the foreground.
 *
 * Two halves, mixed per method (same split UniClipboard documents):
 *  - detection — when to look: a timer (polling), system copy events in
 *    logcat (logs), or Shizuku's own event callback
 *  - access — how to read: an overlay window that briefly takes focus
 *    (polling/logs), or the privileged service (shizuku)
 */
class ClipboardCapture(private val state: AppState) {

    private val context: Context = state.appContext
    private val overlay = FocusOverlay(context)
    private var detector: Detector? = null
    private var method: String = BackgroundReadMethod.OFF

    /** Detection strategies share this contract. */
    interface Detector {
        fun start()
        fun stop()
    }

    /** Apply the configured method; safe to call repeatedly. */
    fun refresh() {
        val settings = state.engine.settings.current
        val wanted = if (settings.backgroundOnline) settings.backgroundReadMethod else BackgroundReadMethod.OFF
        if (wanted == method) return
        stop()
        method = wanted
        detector = when (wanted) {
            BackgroundReadMethod.POLLING -> PollingDetector(settings.pollingIntervalMs, ::capture)
            BackgroundReadMethod.LOGS -> LogcatDetector(::capture)
            BackgroundReadMethod.SHIZUKU -> ShizukuDetector(context, ::capture)
            else -> null
        }
        detector?.start()
        Log.info("clipboard capture method: $wanted")
    }

    fun stop() {
        detector?.stop()
        detector = null
        method = BackgroundReadMethod.OFF
    }

    /**
     * Read the clipboard the way the current method dictates and broadcast
     * anything new. Dedupe and echo suppression live in the engine
     * (broadcastTextIfNew), so a re-read of unchanged content costs nothing.
     */
    private fun capture() {
        // Nothing gets copied with the screen off, so skip the work entirely:
        // saves both the wakeup and a pointless focus grab
        if (power?.isInteractive == false) return
        when (method) {
            BackgroundReadMethod.SHIZUKU -> handle(ShizukuClipboard.readText(context))
            BackgroundReadMethod.POLLING, BackgroundReadMethod.LOGS ->
                overlay.withFocus { handle(ClipboardPort.readText(context)) }
        }
    }

    /** Forward a read result; logs the length only, never the content. */
    private fun handle(text: String?) {
        if (text == null) {
            // Empty clipboard is normal, but a persistent null also covers
            // "the read was blocked" — log the transition, not every tick
            if (!lastReadWasEmpty) {
                lastReadWasEmpty = true
                Log.info("background read came back empty")
            }
            return
        }
        lastReadWasEmpty = false
        if (text == lastSeen) return
        lastSeen = text
        Log.info("background read got ${text.length} chars via $method")
        state.broadcastCaptured(text)
    }

    private var lastReadWasEmpty = false

    // Suppresses repeat work between ticks; the engine still owns real dedupe
    private var lastSeen: String? = null
    private val power = context.getSystemService(android.os.PowerManager::class.java)

    /** Whether the configured method is actually usable right now. */
    fun readiness(): Readiness {
        val settings = state.engine.settings.current
        return when (settings.backgroundReadMethod) {
            BackgroundReadMethod.POLLING ->
                if (FocusOverlay.canDraw(context)) Readiness.READY else Readiness.NEEDS_OVERLAY
            BackgroundReadMethod.LOGS -> when {
                !LogcatDetector.hasReadLogs(context) -> Readiness.NEEDS_READ_LOGS
                !FocusOverlay.canDraw(context) -> Readiness.NEEDS_OVERLAY
                else -> Readiness.READY
            }
            BackgroundReadMethod.SHIZUKU -> when {
                ShizukuClipboard.isReady() -> Readiness.READY
                ShizukuClipboard.needsPermission() -> Readiness.NEEDS_SHIZUKU_PERMISSION
                else -> Readiness.NEEDS_SHIZUKU_SERVICE
            }
            else -> Readiness.READY
        }
    }

    enum class Readiness {
        READY,
        NEEDS_OVERLAY,
        NEEDS_READ_LOGS,
        NEEDS_SHIZUKU_SERVICE,
        NEEDS_SHIZUKU_PERMISSION,
    }
}
