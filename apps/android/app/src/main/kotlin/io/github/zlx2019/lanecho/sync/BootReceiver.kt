package io.github.zlx2019.lanecho.sync

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import io.github.zlx2019.lanecho.LanechoApplication

/**
 * Boot autostart: bring the sync service up after a reboot when the
 * background-online setting is on. BOOT_COMPLETED is exempt from the
 * background FGS-launch restriction, and connectedDevice is not on the
 * Android 15 boot-launch denylist.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val app = context.applicationContext as LanechoApplication
        if (app.appState.engine.settings.current.backgroundOnline) {
            SyncService.start(context)
        }
    }
}
