package io.github.zlx2019.lanecho.sync

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import io.github.zlx2019.lanecho.LanechoApplication
import io.github.zlx2019.lanecho.MainActivity
import io.github.zlx2019.lanecho.R

/**
 * Foreground service that keeps the sync engine online while the UI is gone
 * (K5, decided 2026-08-20): Android's only sanctioned way to hold a TCP
 * listener, the multicast heartbeat and the mDNS registration long-term.
 * Liveness ownership is shared with the Activity: the engine goes offline
 * only when both the UI and this service are gone.
 */
class SyncService : Service() {

    override fun onCreate() {
        super.onCreate()
        running = true
        startForegroundCompat()
        val state = (application as LanechoApplication).appState
        state.ensureOnline()
        // Background clipboard capture lives with the service (K6)
        state.capture.refresh()
    }

    // START_STICKY: the system restarts the service after killing it under
    // pressure, restoring background receiving without user action
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        running = false
        val state = (application as LanechoApplication).appState
        state.capture.stop()
        if (!state.uiVisible) state.goOfflineNow()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startForegroundCompat() {
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun buildNotification(): Notification {
        ensureChannels(this)
        val openApp = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, CHANNEL_SERVICE)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notif_service_title))
            .setContentIntent(openApp)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    companion object {
        const val CHANNEL_SERVICE = "service"
        const val CHANNEL_INCOMING = "incoming"
        private const val NOTIFICATION_ID = 1

        @Volatile
        var running: Boolean = false
            private set

        fun start(context: Context) {
            context.startForegroundService(Intent(context, SyncService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, SyncService::class.java))
        }

        /** Create both channels; a no-op when they already exist. */
        fun ensureChannels(context: Context) {
            val manager = context.getSystemService(NotificationManager::class.java) ?: return
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_SERVICE,
                    context.getString(R.string.notif_channel_service),
                    NotificationManager.IMPORTANCE_MIN,
                ),
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_INCOMING,
                    context.getString(R.string.notif_channel_incoming),
                    NotificationManager.IMPORTANCE_DEFAULT,
                ),
            )
        }
    }
}
