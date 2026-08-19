package io.github.zlx2019.lanecho.core.discovery

import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import java.util.Collections
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class PeerRegistryTest {

    private val events = Collections.synchronizedList(mutableListOf<PeerEvent>())

    private fun registry(self: String = "self") = PeerRegistry(self) { events.add(it) }

    private fun info(fp: String, name: String = "peer-$fp") =
        PeerInfo(deviceId = "d-$fp", name = name, fingerprint = fp, platform = "macos")

    @Test
    fun udpSightingRaisesUpOnceUntilSomethingChanges() {
        val reg = registry()
        reg.seenUdp(info("a"), "192.168.1.2", 42524, nowMs = 0)
        reg.seenUdp(info("a"), "192.168.1.2", 42524, nowMs = 1_000)
        assertEquals(1, events.size)
        // A rename is a change and must surface
        reg.seenUdp(info("a", name = "renamed"), "192.168.1.2", 42524, nowMs = 2_000)
        assertEquals(2, events.size)
        assertEquals("renamed", (events[1] as PeerEvent.Up).peer.info.name)
    }

    @Test
    fun ownPacketsAreIgnored() {
        val reg = registry(self = "me")
        reg.seenUdp(info("me"), "192.168.1.2", 42524, nowMs = 0)
        assertTrue(events.isEmpty())
        assertTrue(reg.snapshot().isEmpty())
    }

    // Addresses only accumulate (multi-NIC / DHCP churn) and dial order puts
    // non-loopback IPv4 first
    @Test
    fun addressesAccumulateAndSortDialFirst() {
        val reg = registry()
        reg.seenUdp(info("a"), "192.168.1.2", 42524, nowMs = 0)
        reg.seenMdns(info("a"), listOf("127.0.0.1", "10.0.0.9"), 42524, nowMs = 0)
        reg.seenUdp(info("a"), "192.168.1.7", 42524, nowMs = 0)
        val peer = reg.find("a")!!
        assertEquals(listOf("192.168.1.2", "10.0.0.9", "192.168.1.7", "127.0.0.1").sorted(), peer.addresses.sorted())
        assertTrue(peer.addresses.last() == "127.0.0.1", "loopback must sort last")
    }

    @Test
    fun linkLocalV6WithoutScopeIsDropped() {
        val reg = registry()
        reg.seenMdns(info("a"), listOf("fe80::1", "192.168.1.2"), 42524, nowMs = 0)
        assertEquals(listOf("192.168.1.2"), reg.find("a")!!.addresses)
    }

    @Test
    fun goodbyeTakesPeerDownInstantly() {
        val reg = registry()
        reg.seenUdp(info("a"), "192.168.1.2", 42524, nowMs = 0)
        reg.goodbye("a")
        assertEquals(PeerEvent.Down("a"), events.last())
        assertTrue(reg.snapshot().isEmpty())
    }

    // UDP-only peers expire after 15s of silence; mDNS-alive peers are never
    // timed out (mDNS caches linger systemically, PROTOCOL.md §8)
    @Test
    fun sweepExpiresUdpOnlyButNeverMdnsAlive() {
        val reg = registry()
        reg.seenUdp(info("udp-only"), "192.168.1.2", 42524, nowMs = 0)
        reg.seenUdp(info("mdns-too"), "192.168.1.3", 42524, nowMs = 0)
        reg.seenMdns(info("mdns-too"), listOf("192.168.1.3"), 42524, nowMs = 0)

        reg.sweep(nowMs = 20_000)
        assertEquals(listOf("mdns-too"), reg.snapshot().map { it.info.fingerprint })
        assertTrue(events.contains(PeerEvent.Down("udp-only")))
    }

    @Test
    fun mdnsRemovalDropsTheAliveFlagThenTheNode() {
        val reg = registry()
        reg.seenMdns(info("a"), listOf("192.168.1.2"), 42524, nowMs = 0)
        // No UDP liveness at all: removal after the UDP window means gone
        reg.mdnsRemoved("a", nowMs = 20_000)
        assertEquals(PeerEvent.Down("a"), events.last())
    }

    // Engine fallback for the phone (no 30s probe loop): a failed dial drops
    // UDP-only peers but spares mDNS-alive ones
    @Test
    fun dialFailureDropsUdpOnlyPeers() {
        val reg = registry()
        reg.seenUdp(info("udp-only"), "192.168.1.2", 42524, nowMs = 0)
        reg.seenMdns(info("mdns-alive"), listOf("192.168.1.3"), 42524, nowMs = 0)

        reg.dialFailed("udp-only")
        reg.dialFailed("mdns-alive")
        assertEquals(listOf("mdns-alive"), reg.snapshot().map { it.info.fingerprint })
    }
}
