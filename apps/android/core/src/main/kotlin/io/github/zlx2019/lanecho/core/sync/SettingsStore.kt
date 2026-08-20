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

@Serializable
data class Settings(
    /** Receive text syncs. */
    val receiveText: Boolean = true,
    /** Receive image syncs (decision DA5). */
    val receiveImages: Boolean = true,
    /** Write received content straight into the clipboard (DA2, default off). */
    val autoWriteClipboard: Boolean = false,
    /** Broadcast the clipboard when the app comes to the foreground (DA3). */
    val sendOnOpen: Boolean = true,
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
