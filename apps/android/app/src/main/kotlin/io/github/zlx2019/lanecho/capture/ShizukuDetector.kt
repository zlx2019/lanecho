package io.github.zlx2019.lanecho.capture

import android.content.ClipboardManager
import android.content.Context
import io.github.zlx2019.lanecho.core.util.Log

/**
 * Event detection for the Shizuku method: the app can register a primary-clip
 * listener freely (the system only gates *reading* the content, not the
 * change callback), and the privileged read that follows needs no focus.
 *
 * OEMs that suppress the background callback would leave this quiet, so a
 * slow safety poll runs alongside — privileged reads are cheap and invisible,
 * unlike the overlay path.
 */
class ShizukuDetector(
    context: Context,
    private val onEvent: () -> Unit,
) : ClipboardCapture.Detector {

    private val manager = context.getSystemService(ClipboardManager::class.java)
    private val listener = ClipboardManager.OnPrimaryClipChangedListener { onEvent() }
    private val safetyPoll = PollingDetector(SAFETY_INTERVAL_MS, onEvent)

    override fun start() {
        runCatching { manager.addPrimaryClipChangedListener(listener) }
            .onFailure { Log.warn("clip listener registration failed: ${it.message}") }
        safetyPoll.start()
    }

    override fun stop() {
        runCatching { manager.removePrimaryClipChangedListener(listener) }
        safetyPoll.stop()
    }

    private companion object {
        const val SAFETY_INTERVAL_MS = 15_000L
    }
}
