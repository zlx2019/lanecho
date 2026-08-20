package io.github.zlx2019.lanecho.capture

import android.os.Handler
import android.os.HandlerThread

/**
 * Timed detection: ticks on its own thread and asks for a clipboard read
 * every interval. Zero setup — the price is a focus grab per tick (which can
 * blink an IME) and up to one interval of latency after a copy.
 */
class PollingDetector(
    private val intervalMs: Long,
    private val onTick: () -> Unit,
) : ClipboardCapture.Detector {

    private var thread: HandlerThread? = null
    private var handler: Handler? = null

    private val tick = object : Runnable {
        override fun run() {
            onTick()
            handler?.postDelayed(this, intervalMs)
        }
    }

    override fun start() {
        val worker = HandlerThread("lanecho-clip-poll").apply { start() }
        thread = worker
        handler = Handler(worker.looper).apply { postDelayed(tick, intervalMs) }
    }

    override fun stop() {
        handler?.removeCallbacksAndMessages(null)
        handler = null
        thread?.quitSafely()
        thread = null
    }
}
