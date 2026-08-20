package io.github.zlx2019.lanecho.capture

import android.content.Context
import android.content.pm.PackageManager
import io.github.zlx2019.lanecho.core.util.Log
import java.io.BufferedReader
import java.io.InputStreamReader

/**
 * Event detection off the system log: ClipboardService logs every clipboard
 * transaction, so a line from it means "something touched the clipboard" —
 * the cue to go read it. Only the tag matters here; no log content is
 * inspected beyond that, and the read itself still goes through the overlay.
 *
 * READ_LOGS cannot be requested at runtime:
 *   adb shell pm grant io.github.zlx2019.lanecho android.permission.READ_LOGS
 * and the app must be restarted afterwards.
 */
class LogcatDetector(private val onEvent: () -> Unit) : ClipboardCapture.Detector {

    @Volatile
    private var running = false
    private var process: Process? = null
    private var thread: Thread? = null

    override fun start() {
        running = true
        thread = Thread({ pump() }, "lanecho-clip-logs").apply { isDaemon = true; start() }
    }

    override fun stop() {
        running = false
        runCatching { process?.destroy() }
        process = null
        thread = null
    }

    // -T 1 starts at the tail: never replay history on (re)start
    private fun pump() {
        while (running) {
            val started = runCatching {
                ProcessBuilder("logcat", "-T", "1", "-s", TAG)
                    .redirectErrorStream(true)
                    .start()
            }.getOrElse {
                Log.warn("logcat detector could not start: ${it.message}")
                return
            }
            process = started
            runCatching {
                BufferedReader(InputStreamReader(started.inputStream)).use { reader ->
                    while (running) {
                        val line = reader.readLine() ?: break
                        if (line.contains(TAG)) onEvent()
                    }
                }
            }
            // Without READ_LOGS the stream closes immediately; a tight respawn
            // loop would spin the CPU, so back off before retrying
            if (running) Thread.sleep(RETRY_DELAY_MS)
        }
    }

    companion object {
        private const val TAG = "ClipboardService"
        private const val RETRY_DELAY_MS = 5_000L

        fun hasReadLogs(context: Context): Boolean =
            context.checkSelfPermission(android.Manifest.permission.READ_LOGS) ==
                PackageManager.PERMISSION_GRANTED
    }
}
