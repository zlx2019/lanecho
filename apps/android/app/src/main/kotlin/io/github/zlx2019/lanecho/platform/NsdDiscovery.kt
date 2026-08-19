package io.github.zlx2019.lanecho.platform

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import io.github.zlx2019.lanecho.core.MDNS_SERVICE_TYPE
import io.github.zlx2019.lanecho.core.discovery.PeerRegistry
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.util.Log
import java.util.ArrayDeque
import java.util.concurrent.ConcurrentHashMap

// mDNS via the system NsdManager (no MulticastLock needed, the system daemon
// owns the traffic). TXT layout per PROTOCOL §7: id/name/fp/platform/osv;
// instance name = device_id (stable across renames). Resolution is strictly
// serial — concurrent resolveService calls are NsdManager's most famous trap
class NsdDiscovery(
    context: Context,
    private val registry: PeerRegistry,
    private val selfDeviceId: String,
    private val localInfo: () -> PeerInfo,
    private val tcpPort: () -> Int,
) {
    private val nsd = context.applicationContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val serviceType = "$MDNS_SERVICE_TYPE."

    private var registrationListener: NsdManager.RegistrationListener? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null

    // Serial resolve queue
    private val queue = ArrayDeque<NsdServiceInfo>()
    private var resolving = false
    private val lock = Any()

    // service instance name (device_id) → fingerprint, for removal events
    private val fingerprintsByInstance = ConcurrentHashMap<String, String>()

    fun start() {
        register()
        discover()
    }

    fun stop() {
        registrationListener?.let { runCatching { nsd.unregisterService(it) } }
        registrationListener = null
        discoveryListener?.let { runCatching { nsd.stopServiceDiscovery(it) } }
        discoveryListener = null
        synchronized(lock) {
            queue.clear()
            resolving = false
        }
    }

    private fun register() {
        val info = localInfo()
        val service = NsdServiceInfo().apply {
            serviceName = selfDeviceId
            serviceType = this@NsdDiscovery.serviceType
            port = tcpPort()
            setAttribute("id", info.deviceId)
            setAttribute("name", info.name)
            setAttribute("fp", info.fingerprint)
            setAttribute("platform", info.platform)
            info.osVersion?.let { setAttribute("osv", it) }
        }
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(registered: NsdServiceInfo) {}
            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.warn("mDNS registration failed: $errorCode")
            }
            override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {}
            override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
        }
        registrationListener = listener
        runCatching { nsd.registerService(service, NsdManager.PROTOCOL_DNS_SD, listener) }
    }

    private fun discover() {
        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.warn("mDNS discovery start failed: $errorCode")
            }
            override fun onDiscoveryStopped(serviceType: String) {}
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}

            override fun onServiceFound(service: NsdServiceInfo) {
                if (service.serviceName == selfDeviceId) return
                enqueueResolve(service)
            }

            override fun onServiceLost(service: NsdServiceInfo) {
                val fingerprint = fingerprintsByInstance[service.serviceName] ?: return
                registry.mdnsRemoved(fingerprint, System.currentTimeMillis())
            }
        }
        discoveryListener = listener
        runCatching { nsd.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener) }
    }

    private fun enqueueResolve(service: NsdServiceInfo) {
        synchronized(lock) {
            queue.addLast(service)
            if (!resolving) resolveNextLocked()
        }
    }

    private fun resolveNextLocked() {
        val next = queue.pollFirst() ?: run { resolving = false; return }
        resolving = true
        @Suppress("DEPRECATION") // resolveService: the 29+ compatible path
        nsd.resolveService(next, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                // FAILURE_ALREADY_ACTIVE and friends: retry once at the tail
                if (errorCode == NsdManager.FAILURE_ALREADY_ACTIVE) {
                    synchronized(lock) { queue.addLast(serviceInfo) }
                }
                synchronized(lock) { resolveNextLocked() }
            }

            override fun onServiceResolved(resolved: NsdServiceInfo) {
                handleResolved(resolved)
                synchronized(lock) { resolveNextLocked() }
            }
        })
    }

    private fun handleResolved(resolved: NsdServiceInfo) {
        val attributes = resolved.attributes ?: return
        fun text(key: String): String? = attributes[key]?.let { String(it, Charsets.UTF_8) }
        // Fields missing = not a lanecho service; ignore (PROTOCOL §7)
        val info = PeerInfo(
            deviceId = text("id") ?: return,
            name = text("name") ?: return,
            fingerprint = text("fp") ?: return,
            platform = text("platform") ?: return,
            osVersion = text("osv"),
        )
        fingerprintsByInstance[resolved.serviceName] = info.fingerprint
        val addresses = buildList {
            @Suppress("DEPRECATION")
            resolved.host?.hostAddress?.let { add(it) }
        }
        if (addresses.isEmpty()) return
        registry.seenMdns(info, addresses, resolved.port, System.currentTimeMillis())
    }
}
