package io.github.zlx2019.lanecho.core.sync

import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.core.history.ImageCodec
import io.github.zlx2019.lanecho.core.history.RgbaImage
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.transport.SyncReceiver
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.Collections
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

// Two engines talking over the loopback: pairing, LWW, echo guard and the
// history pipeline all end to end (no discovery: peers are injected into the
// registry the way the UDP channel would)
class EngineLoopbackTest {

    @TempDir
    lateinit var dir: Path

    private val engines = mutableListOf<Engine>()
    private val receivers = mutableListOf<SyncReceiver>()

    @AfterEach
    fun tearDown() {
        receivers.forEach { runCatching { it.stop() } }
    }

    // Deterministic fake codec: "decodes" any PNG to a fixed-size RGBA whose
    // bytes derive from the PNG content (hash baseline still content-driven)
    private object FakeCodec : ImageCodec {
        override fun decodeRgba(png: ByteArray): RgbaImage =
            RgbaImage(2, 2, ByteArray(16) { png[it % png.size] })
    }

    private class Events : EngineListener {
        val applied = Collections.synchronizedList(mutableListOf<Pair<HistoryEntry, String>>())
        val pairRequests = Collections.synchronizedList(mutableListOf<PeerInfo>())
        override fun onRemoteApplied(entry: HistoryEntry, fromName: String) {
            applied.add(entry to fromName)
        }
        override fun onPairRequest(remote: PeerInfo) {
            pairRequests.add(remote)
        }
    }

    private fun engine(name: String): Pair<Engine, Int> {
        val engine = Engine(
            dataDir = Files.createDirectories(dir.resolve(name)),
            imageCodec = FakeCodec,
            defaultName = name,
            osVersion = "test",
        )
        engine.initialize()
        engines.add(engine)
        // Start only the receiver (no UDP in loopback tests); mirror what
        // goOnline wires up
        val receiver = SyncReceiver(engine.identity, engine::localInfo, engine)
        receivers.add(receiver)
        val port = receiver.start(0)
        return engine to port
    }

    private fun introduce(engine: Engine, other: Engine, otherPort: Int) {
        engine.registry.seenUdp(other.localInfo(), "127.0.0.1", otherPort, System.currentTimeMillis())
    }

    private fun waitUntil(what: String, timeoutMs: Long = 5_000, condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (condition()) return
            Thread.sleep(20)
        }
        fail("timed out waiting for $what")
    }

    @Test
    fun pairBroadcastAndLwwAcrossTwoEngines() {
        val (alice, alicePort) = engine("alice")
        val (bob, bobPort) = engine("bob")
        val bobEvents = Events()
        bob.addListener(bobEvents)
        introduce(alice, bob, bobPort)
        introduce(bob, alice, alicePort)

        // Inbound pairing verdict flows through the listener + resolvePair
        val approver = Thread {
            waitUntil("bob seeing the pair request") { bobEvents.pairRequests.isNotEmpty() }
            bob.resolvePair(alice.identity.fingerprint, accepted = true)
        }
        approver.start()
        val remote = alice.pairWith(bob.identity.fingerprint)
        approver.join()
        assertEquals(bob.identity.fingerprint, remote.fingerprint)
        assertTrue(bob.paired.isPaired(alice.identity.fingerprint), "receiver records the pairing too")

        // Broadcast lands in bob's history with the origin label
        val delivered = alice.broadcastText("from alice 🚀", timestampMs = 1_000)
        assertEquals(1, delivered)
        waitUntil("bob applying the sync") { bobEvents.applied.isNotEmpty() }
        val (entry, fromName) = bobEvents.applied[0]
        assertEquals("from alice 🚀", entry.text)
        assertEquals("alice", entry.origin)
        assertEquals("alice", fromName)

        // LWW: an older timestamp is acked but ignored (no new history entry)
        alice.broadcastText("stale", timestampMs = 500)
        Thread.sleep(300)
        assertTrue(bob.history.list().none { it.text == "stale" }, "stale content must not land")

        // Newer content lands and bumps the baseline
        alice.broadcastText("fresh", timestampMs = 2_000)
        waitUntil("fresh landing") { bob.history.list().any { it.text == "fresh" } }
    }

    @Test
    fun unpairedBroadcastReachesNobody() {
        val (alice, alicePort) = engine("alice2")
        val (bob, bobPort) = engine("bob2")
        introduce(alice, bob, bobPort)
        introduce(bob, alice, alicePort)

        val delivered = alice.broadcastText("nobody hears this", timestampMs = 100)
        assertEquals(0, delivered, "no paired peers = no targets")
        Thread.sleep(200)
        assertTrue(bob.history.list().isEmpty())
    }

    // DA3 echo guard: what a remote just handed us must not bounce back; an
    // explicit broadcast of the same content still goes out
    @Test
    fun echoGuardStopsOpportunisticRebroadcastOnly() {
        val (alice, alicePort) = engine("alice3")
        val (bob, bobPort) = engine("bob3")
        val bobEvents = Events()
        bob.addListener(bobEvents)
        introduce(alice, bob, bobPort)
        introduce(bob, alice, alicePort)
        val approver = Thread {
            waitUntil("pair request") { bobEvents.pairRequests.isNotEmpty() }
            bob.resolvePair(alice.identity.fingerprint, true)
        }
        approver.start()
        alice.pairWith(bob.identity.fingerprint)
        approver.join()

        alice.broadcastText("shared content", timestampMs = 1_000)
        waitUntil("bob receiving") { bob.history.list().any { it.text == "shared content" } }

        // Bob's foreground upstream reads the same content: guarded, no dial
        assertEquals(0, bob.broadcastTextIfNew("shared content", System.currentTimeMillis()))
        // But an explicit restore/share of that content still broadcasts
        val delivered = bob.broadcastText("shared content", System.currentTimeMillis())
        assertEquals(1, delivered)
    }

    @Test
    fun refusalsComeBackAsZeroDeliveriesNotCrashes() {
        val (alice, alicePort) = engine("alice4")
        val (bob, bobPort) = engine("bob4")
        val bobEvents = Events()
        bob.addListener(bobEvents)
        introduce(alice, bob, bobPort)
        introduce(bob, alice, alicePort)
        val approver = Thread {
            waitUntil("pair request") { bobEvents.pairRequests.isNotEmpty() }
            bob.resolvePair(alice.identity.fingerprint, true)
        }
        approver.start()
        alice.pairWith(bob.identity.fingerprint)
        approver.join()

        // Bob pauses text reception: alice's broadcast is refused (disabled)
        bob.settings.save(bob.settings.current.copy(receiveText = false))
        val delivered = alice.broadcastText("refused", timestampMs = 5_000)
        assertEquals(0, delivered)
        assertTrue(bob.history.list().none { it.text == "refused" })
    }
}
