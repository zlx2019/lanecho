package io.github.zlx2019.lanecho.platform

import android.content.Context
import android.net.wifi.WifiManager

// Most Wi-Fi drivers filter multicast unless a MulticastLock is held; the
// lock lives exactly as long as the foreground-online session (battery)
class MulticastLockHolder(context: Context) {
    private val lock: WifiManager.MulticastLock =
        (context.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager)
            .createMulticastLock("lanecho-discovery")
            .apply { setReferenceCounted(false) }

    fun acquire() = runCatching { lock.acquire() }

    fun release() = runCatching { if (lock.isHeld) lock.release() }
}
