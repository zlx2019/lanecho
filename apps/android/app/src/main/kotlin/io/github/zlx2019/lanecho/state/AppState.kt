package io.github.zlx2019.lanecho.state

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.sync.Engine
import io.github.zlx2019.lanecho.core.sync.EngineListener
import io.github.zlx2019.lanecho.platform.ClipboardPort
import io.github.zlx2019.lanecho.platform.MulticastLockHolder
import io.github.zlx2019.lanecho.platform.NsdDiscovery
import java.util.concurrent.Executors

/** A received-content banner awaiting the user (DA2: history + tap-to-copy). */
data class Banner(val entry: HistoryEntry, val fromName: String, val autoWritten: Boolean)

/**
 * Application-scoped UI state: bridges engine callbacks (connection threads)
 * onto the main thread as Compose state, and drives the foreground-online
 * lifecycle (Activity start/stop with a debounce so rotation does not flap
 * the whole network stack).
 */
class AppState(val appContext: Context, val engine: Engine) : EngineListener {
    private val mainHandler = Handler(Looper.getMainLooper())
    private val worker = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "lanecho-ui-worker").apply { isDaemon = true }
    }
    private val multicastLock = MulticastLockHolder(appContext)
    private var nsd: NsdDiscovery? = null

    // Bumped counters drive recomposition; lists are re-read from the engine
    var historyVersion by mutableIntStateOf(0)
        private set
    var peersVersion by mutableIntStateOf(0)
        private set
    var online by mutableStateOf(false)
        private set
    var banner by mutableStateOf<Banner?>(null)
    var pairRequest by mutableStateOf<PeerInfo?>(null)
        private set
    var toast by mutableStateOf<String?>(null)

    private var offlineDebounce: Runnable? = null
    private var sentOnThisForeground = false

    init {
        engine.addListener(this)
    }

    // ---- Foreground-online lifecycle ----

    fun onForeground() {
        offlineDebounce?.let(mainHandler::removeCallbacks)
        offlineDebounce = null
        if (online) return
        worker.execute {
            multicastLock.acquire()
            engine.goOnline()
            if (engine.isOnline) {
                val discovery = NsdDiscovery(
                    appContext, engine.registry, engine.identity.deviceId,
                    engine::localInfo, { engine.settings.current.port },
                )
                discovery.start()
                nsd = discovery
            }
            mainHandler.post {
                online = engine.isOnline
                sentOnThisForeground = false
            }
        }
    }

    fun onBackground() {
        // Debounce: rotation restarts the Activity within milliseconds and
        // must not flap goodbye/announce on the LAN
        val task = Runnable {
            worker.execute {
                nsd?.stop()
                nsd = null
                engine.goOffline()
                multicastLock.release()
                mainHandler.post { online = false }
            }
        }
        offlineDebounce = task
        mainHandler.postDelayed(task, 800)
    }

    /** First window focus after coming to the foreground: DA3 upstream. */
    fun onFocused() {
        if (sentOnThisForeground || !engine.settings.current.sendOnOpen) return
        sentOnThisForeground = true
        // Clipboard reads require focus (Android 10+); broadcasting dials out
        val text = ClipboardPort.readText(appContext) ?: return
        worker.execute {
            val delivered = engine.broadcastTextIfNew(text, System.currentTimeMillis())
            if (delivered > 0) {
                showToast(appContext.getString(io.github.zlx2019.lanecho.R.string.toast_sent_to, delivered))
            }
        }
    }

    // ---- User actions ----

    /** Restore an entry: write the clipboard and broadcast (a restore is a copy). */
    fun copyEntry(entry: HistoryEntry) {
        when (entry.kind) {
            io.github.zlx2019.lanecho.core.history.EntryKind.TEXT -> {
                val text = entry.text ?: return
                ClipboardPort.writeText(appContext, text)
                engine.noteAppliedToClipboard(entry)
                worker.execute { engine.broadcastText(text, System.currentTimeMillis()) }
            }
            io.github.zlx2019.lanecho.core.history.EntryKind.IMAGE -> {
                val blobHash = entry.blobHash ?: return
                ClipboardPort.writeImageBlob(appContext, engine.history, blobHash)
                engine.noteAppliedToClipboard(entry)
                // v1 does not dial images out (share/send is text-only)
            }
        }
        showToast(appContext.getString(io.github.zlx2019.lanecho.R.string.toast_copied))
    }

    fun pairWith(fingerprint: String, onResult: (Result<PeerInfo>) -> Unit) {
        worker.execute {
            val result = runCatching { engine.pairWith(fingerprint) }
            mainHandler.post { onResult(result) }
        }
    }

    fun unpair(fingerprint: String) {
        worker.execute { engine.unpairWith(fingerprint) }
    }

    fun resolvePair(fingerprint: String, accepted: Boolean) {
        pairRequest = null
        worker.execute { engine.resolvePair(fingerprint, accepted) }
    }

    fun showToast(message: String) {
        mainHandler.post { toast = message }
    }

    // ---- EngineListener (connection threads → main) ----

    override fun onHistoryChanged() {
        mainHandler.post { historyVersion++ }
    }

    override fun onPeersChanged() {
        mainHandler.post { peersVersion++ }
    }

    override fun onRemoteApplied(entry: HistoryEntry, fromName: String) {
        val autoWrite = engine.settings.current.autoWriteClipboard
        mainHandler.post {
            if (autoWrite && entry.kind == io.github.zlx2019.lanecho.core.history.EntryKind.TEXT) {
                entry.text?.let { ClipboardPort.writeText(appContext, it) }
                engine.noteAppliedToClipboard(entry)
            }
            banner = Banner(entry, fromName, autoWritten = autoWrite)
        }
    }

    override fun onPairRequest(remote: PeerInfo) {
        mainHandler.post { pairRequest = remote }
    }
}
