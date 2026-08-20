package io.github.zlx2019.lanecho.capture

import android.content.Context
import android.graphics.PixelFormat
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import io.github.zlx2019.lanecho.core.util.Log

/**
 * A 1x1 transparent overlay that briefly takes window focus so the app counts
 * as "focused" for the clipboard service (Android 10+ blocks background
 * reads). This is the mechanism behind the polling and logcat methods —
 * UniClipboard uses the same trick.
 *
 * The window is added, the clipboard work runs once focus lands, and the
 * window is removed again; total on-screen time is a few frames and nothing
 * is drawn, but the focus change is real and can blink an IME candidate bar.
 */
class FocusOverlay(private val context: Context) {

    private val mainHandler = Handler(Looper.getMainLooper())
    private val windowManager = context.getSystemService(WindowManager::class.java)
    private var view: FocusView? = null

    /**
     * Run [work] while the overlay holds focus, then tear the overlay down.
     * [work] runs on the main thread; it must not block.
     */
    fun withFocus(work: () -> Unit) {
        mainHandler.post {
            if (!canDraw(context)) {
                Log.warn("overlay permission missing, skipping focused clipboard access")
                return@post
            }
            if (view != null) return@post // an access is already in flight
            val overlay = FocusView(context) { granted ->
                if (granted) {
                    runCatching(work).onFailure { Log.warn("focused clipboard work failed: ${it.message}") }
                    dismiss()
                }
            }
            runCatching {
                windowManager.addView(overlay, layoutParams())
                view = overlay
                // The window manager only hands focus to a window that asks
                overlay.requestFocus()
                // Safety net: if focus never lands (some OEM overlays), do not
                // leave the window hanging around
                mainHandler.postDelayed(::dismiss, FOCUS_TIMEOUT_MS)
            }.onFailure { Log.warn("overlay add failed: ${it.message}") }
        }
    }

    private fun dismiss() {
        val current = view ?: return
        // Reaching here with the work still pending means focus never landed
        // — the whole read path goes silent, so say so once per occurrence
        if (!current.sawFocus) Log.warn("overlay never gained focus; clipboard not read")
        view = null
        mainHandler.removeCallbacks(::dismiss)
        runCatching { windowManager.removeView(current) }
    }

    private fun layoutParams(): WindowManager.LayoutParams {
        val type = if (Build.VERSION.SDK_INT >= 26) {
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY
        } else {
            @Suppress("DEPRECATION")
            WindowManager.LayoutParams.TYPE_SYSTEM_ALERT
        }
        return WindowManager.LayoutParams(
            1, 1, type,
            // FLAG_NOT_FOCUSABLE is deliberately absent: focus is the point
            WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL or
                WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            x = 0
            y = 0
        }
    }

    /** Reports focus changes so the caller can act the moment focus lands. */
    private class FocusView(context: Context, val onFocus: (Boolean) -> Unit) : View(context) {
        var sawFocus = false
            private set

        init {
            isFocusable = true
            isFocusableInTouchMode = true
        }

        override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
            super.onWindowFocusChanged(hasWindowFocus)
            if (hasWindowFocus) sawFocus = true
            onFocus(hasWindowFocus)
        }
    }

    companion object {
        private const val FOCUS_TIMEOUT_MS = 1_500L

        fun canDraw(context: Context): Boolean = Settings.canDrawOverlays(context)
    }
}
