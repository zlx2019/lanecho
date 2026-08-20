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
import io.github.zlx2019.lanecho.core.sync.PairedPeer
import io.github.zlx2019.lanecho.platform.AndroidImageCodec
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

    // ---- Demo seeding (UI review only) ----

    /**
     * Populate history and device lists with sample data for UI review.
     * Trigger: `adb shell am start -n <pkg>/.MainActivity --ez seed-demo true`.
     * History and pairings persist like real data; registry peers vanish on
     * restart. Peer addresses use TEST-NET (RFC 5737) so dials fail fast.
     */
    fun seedDemoData() {
        worker.execute {
            val now = System.currentTimeMillis()
            val history = engine.history
            val mac = "Zero 的 Mac mini"
            val win = "Windows 工作站"

            history.recordText("8f7a2c91-4b3e-4d17-9a60-5c2f8e1d0b42", mac, now - 26 * 3_600_000)
            history.recordText(
                "剪贴板同步的铁律：程序写入的内容永不广播，敏感标记的内容不出本机。" +
                    "任何地方不许 trim 或转义，文本必须逐字节一致地送达每一台设备。",
                null, now - 5 * 3_600_000,
            )
            seedImage(400, 560, 0xFFE8A0B4.toInt(), 0xFF7C4DAB.toInt(), win, now - 2 * 3_600_000)
            history.recordText("zero@example.com", win, now - 3_600_000)
            history.recordText("cargo nextest run --workspace", null, now - 26 * 60_000)
                ?.let { history.setPinned(it.id, true) }
            seedImage(640, 400, 0xFF2A9D8F.toInt(), 0xFFB7E9DF.toInt(), mac, now - 15 * 60_000)
            history.recordText("https://github.com/zlx2019/lanecho", mac, now - 8 * 60_000)
            history.recordText("会议改到周四下午 3 点，记得带上季度报表", mac, now - 2 * 60_000)

            val paired = engine.paired
            paired.upsert(PairedPeer("a1".repeat(32), "demo-mac", mac, now - 86_400_000))
            paired.upsert(PairedPeer("b2".repeat(32), "demo-win", win, now - 172_800_000))
            val registry = engine.registry
            registry.seenMdns(
                PeerInfo("demo-mac", mac, "a1".repeat(32), "macos", "macOS 15.3"),
                listOf("192.0.2.10"), 42524, now,
            )
            registry.seenMdns(
                PeerInfo("demo-air", "MacBook Air", "c3".repeat(32), "macos", "macOS 15.5"),
                listOf("192.0.2.11"), 42524, now,
            )
            registry.seenMdns(
                PeerInfo("demo-pixel", "Pixel 9 Pro", "d4".repeat(32), "android", "Android 16"),
                listOf("192.0.2.12"), 42524, now,
            )
            onHistoryChanged()
        }
    }

    /** Draw a gradient with a translucent disc, encode as PNG, and record it. */
    private fun seedImage(w: Int, h: Int, from: Int, to: Int, origin: String?, timestampMs: Long) {
        val bitmap = android.graphics.Bitmap.createBitmap(w, h, android.graphics.Bitmap.Config.ARGB_8888)
        val canvas = android.graphics.Canvas(bitmap)
        val paint = android.graphics.Paint().apply {
            shader = android.graphics.LinearGradient(
                0f, 0f, w.toFloat(), h.toFloat(), from, to, android.graphics.Shader.TileMode.CLAMP,
            )
        }
        canvas.drawRect(0f, 0f, w.toFloat(), h.toFloat(), paint)
        val disc = android.graphics.Paint().apply { color = 0x55FFFFFF }
        canvas.drawCircle(w * 0.68f, h * 0.32f, minOf(w, h) * 0.28f, disc)
        val out = java.io.ByteArrayOutputStream()
        bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
        val png = out.toByteArray()
        val rgba = AndroidImageCodec.decodeRgba(png) ?: return
        engine.history.recordImage(png, rgba, origin, timestampMs)
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
