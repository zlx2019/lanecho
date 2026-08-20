package io.github.zlx2019.lanecho.core.sync

import io.github.zlx2019.lanecho.core.DEFAULT_TCP_PORT
import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.core.util.atomicWrite
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.nio.file.Files
import java.nio.file.Path

// Phone-side settings (settings.json, camelCase like the desktop file). The
// phone owns its data directory, so this is a subset tailored to the mobile
// feature set rather than a shared-file contract

/** How the phone notices and reads clipboard changes while backgrounded (K6). */
object BackgroundReadMethod {
    /** No background capture; upstream is share-sheet and app-open only. */
    const val OFF = "off"

    /** Overlay window grabs focus on a timer. */
    const val POLLING = "polling"

    /** Overlay window grabs focus when a system copy event appears in logcat. */
    const val LOGS = "logs"

    /** Shizuku privileged service reads directly; no overlay, event-driven. */
    const val SHIZUKU = "shizuku"
}

@Serializable
data class Settings(
    /** Receive text syncs. */
    val receiveText: Boolean = true,
    /** Receive image syncs (decision DA5). */
    val receiveImages: Boolean = true,
    /** Write received content straight into the clipboard (DA2, default off). */
    val autoWriteClipboard: Boolean = false,
    // sendOnOpen (broadcast on app open, DA3) removed 2026-08-20: background
    // capture (K6) makes it redundant. Unknown keys are ignored, so settings
    // files still carrying it load fine.
    /** Keep receiving in the background via a foreground service (K5). */
    val backgroundOnline: Boolean = true,
    /**
     * Background clipboard capture: "off" | "polling" | "logs" | "shizuku"
     * (K6). Polling by default so "copy syncs" holds out of the box; it stays
     * inert until the user grants the overlay permission the settings screen
     * asks for.
     */
    val backgroundReadMethod: String = BackgroundReadMethod.POLLING,
    /** Polling interval for the timed method, milliseconds. */
    val pollingIntervalMs: Long = 3_000,
    /** TCP listening port (must match across the LAN's lanecho nodes). */
    val port: Int = DEFAULT_TCP_PORT,
    /** History entry cap. */
    val historyLimit: Int = 200,
)

private val settingsJson = Json {
    ignoreUnknownKeys = true
    prettyPrint = true
    prettyPrintIndent = "  "
}

class SettingsStore(private val path: Path) {
    @Volatile
    var current: Settings = Settings()
        private set

    fun load() {
        if (!Files.exists(path)) return
        current = try {
            settingsJson.decodeFromString(Settings.serializer(), Files.readAllBytes(path).decodeToString())
        } catch (e: Exception) {
            Log.warn("settings.json unreadable, using defaults: ${e.message}")
            Settings()
        }
    }

    /** Persist first, apply after (the caller applies side effects on success). */
    fun save(settings: Settings) {
        val body = try {
            settingsJson.encodeToString(Settings.serializer(), settings)
        } catch (e: Exception) {
            Log.warn("settings serialization failed, keeping previous: ${e.message}")
            return
        }
        atomicWrite(path, body.encodeToByteArray())
        current = settings
    }
}
