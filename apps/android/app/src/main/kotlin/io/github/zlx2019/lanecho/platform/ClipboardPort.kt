package io.github.zlx2019.lanecho.platform

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.PersistableBundle
import androidx.core.content.FileProvider
import io.github.zlx2019.lanecho.core.history.HistoryStore
import java.io.File

// Clipboard access is foreground-only on Android 10+ — every call here runs
// on explicit user-visible actions, never from the background
object ClipboardPort {

    /**
     * Read the current clipboard text; null when empty, non-text, or flagged
     * sensitive (password managers set EXTRA_IS_SENSITIVE — such content must
     * never leave this device, PROTOCOL §9).
     */
    fun readText(context: Context): String? {
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = manager.primaryClip ?: return null
        if (clip.itemCount == 0) return null
        if (isSensitive(clip.description)) return null
        return clip.getItemAt(0).coerceToText(context)?.toString()?.takeIf { it.isNotEmpty() }
    }

    fun writeText(context: Context, text: String) {
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        manager.setPrimaryClip(ClipData.newPlainText("lanecho", text))
    }

    /** Copy an image entry: hand the blob out as a content URI with a read grant. */
    fun writeImageBlob(context: Context, history: HistoryStore, blobHash: String): Boolean {
        val file = history.blobPath(blobHash).toFile()
        if (!file.exists()) return false
        val uri = fileProviderUri(context, file)
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        manager.setPrimaryClip(ClipData.newUri(context.contentResolver, "lanecho image", uri))
        return true
    }

    fun fileProviderUri(context: Context, file: File): Uri =
        FileProvider.getUriForFile(context, "io.github.zlx2019.lanecho.fileprovider", file)

    private fun isSensitive(description: ClipDescription): Boolean {
        val extras: PersistableBundle = description.extras ?: return false
        return if (Build.VERSION.SDK_INT >= 33) {
            extras.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE, false)
        } else {
            extras.getBoolean("android.content.extra.IS_SENSITIVE", false)
        }
    }
}
