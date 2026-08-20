package io.github.zlx2019.lanecho.core.discovery

import io.github.zlx2019.lanecho.core.DEFAULT_DISCOVERY_PORT
import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.Collections
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

// Multicast tests: two Kotlin channels finding each other on an ephemeral
// port, and the real-port two-way interop with lanecho-cli (its discovery
// port is fixed at 42525). All gated on the loopback probe
class DiscoveryInteropTest {

    @TempDir
    lateinit var dir: Path

    private val channels = mutableListOf<UdpDiscoveryChannel>()
    private val processes = mutableListOf<Process>()

    @AfterEach
    fun tearDown() {
        channels.forEach { runCatching { it.stop() } }
        processes.forEach { it.destroy() }
        processes.forEach { if (it.isAlive) it.destroyForcibly() }
    }

    private fun info(fp: String, name: String) =
        PeerInfo(deviceId = "d-$name", name = name, fingerprint = fp, platform = "android")

    private fun channel(
        fp: String,
        name: String,
        port: Int,
        registry: PeerRegistry,
        passive: Boolean = false,
    ): UdpDiscoveryChannel {
        val ch = UdpDiscoveryChannel(fp, { info(fp, name) }, { 42524 }, port, registry, passive)
        channels.add(ch)
        ch.start()
        return ch
    }

    private fun waitUntil(what: String, timeoutMs: Long = 10_000, condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (condition()) return
            Thread.sleep(50)
        }
        fail("timed out waiting for $what")
    }

    @Test
    fun twoChannelsDiscoverEachOtherAndGoodbyeTakesDown() {
        assumeTrue(MulticastAvailability.available, MulticastAvailability.SKIP_REASON)
        // Ephemeral shared port so the test never collides with a real node
        val port = java.net.DatagramSocket(0).use { it.localPort }

        val eventsA = Collections.synchronizedList(mutableListOf<PeerEvent>())
        val registryA = PeerRegistry("fp-a") { eventsA.add(it) }
        channel("fp-a", "alice", port, registryA)

        val registryB = PeerRegistry("fp-b") { }
        val channelB = channel("fp-b", "bob", port, registryB)

        // Announce + unicast response make both sides see each other fast
        waitUntil("alice seeing bob") { registryA.find("fp-b") != null }
        waitUntil("bob seeing alice") { registryB.find("fp-a") != null }
        assertEquals("bob", registryA.find("fp-b")!!.info.name)

        channelB.stop() // Sends goodbye
        waitUntil("bob going down") { registryA.find("fp-b") == null }
        // Poll for the event too: goodbye() removes the entry before it fires
        // the callback, so asserting right after the entry disappears races
        // that gap and fails under load
        waitUntil("bob's down event") {
            eventsA.any { it is PeerEvent.Down && it.fingerprint == "fp-b" }
        }
    }

    @Test
    fun passiveChannelReceivesButNeverAnnounces() {
        assumeTrue(MulticastAvailability.available, MulticastAvailability.SKIP_REASON)
        val port = java.net.DatagramSocket(0).use { it.localPort }

        val registryActive = PeerRegistry("fp-active") { }
        channel("fp-active", "active", port, registryActive)
        val registryPassive = PeerRegistry("fp-passive") { }
        channel("fp-passive", "passive", port, registryPassive, passive = true)

        waitUntil("passive seeing active") { registryPassive.find("fp-active") != null }
        // Give the active side a heartbeat's worth of chances to (wrongly)
        // learn about the passive node
        Thread.sleep(1_000)
        assertTrue(registryActive.find("fp-passive") == null, "passive node must stay invisible")
    }

    // Two-way discovery with the Rust CLI on the real 42525 (its port is not
    // configurable): the CLI's announces land in our registry, and the
    // resident listener's /peers snapshot must list us. A short-lived `scan`
    // cannot serve the reverse check — with several sockets sharing 42525 via
    // SO_REUSEPORT the unicast responses land on an arbitrary process
    @Test
    fun cliAndKotlinDiscoverEachOtherOnTheRealPort() {
        assumeTrue(MulticastAvailability.available, MulticastAvailability.SKIP_REASON)
        val binary = cliBinary()
        assumeTrue(Files.isExecutable(binary), "lanecho-cli not built")

        val kotlinIdentity = DeviceIdentity.loadOrCreate(Files.createDirectories(dir.resolve("kotlin")))
        val registry = PeerRegistry(kotlinIdentity.fingerprint) { }
        val kotlinInfo = PeerInfo(kotlinIdentity.deviceId, "kotlin-disco", kotlinIdentity.fingerprint, "android")
        val ch = UdpDiscoveryChannel(
            kotlinIdentity.fingerprint, { kotlinInfo }, { 42524 },
            DEFAULT_DISCOVERY_PORT, registry,
        )
        channels.add(ch)
        ch.start()

        // CLI resident listener announces on 42525
        val cliDir = Files.createDirectories(dir.resolve("cli"))
        val listen = ProcessBuilder(
            binary.toString(), "--data-dir", cliDir.toString(),
            "listen", "--port", "0", "--yes", "--no-clipboard",
        ).redirectErrorStream(true).start()
        processes.add(listen)
        val lines = Collections.synchronizedList(mutableListOf<String>())
        Thread({
            listen.inputStream.bufferedReader().forEachLine { lines.add(it) }
        }, "cli-stdout").apply { isDaemon = true }.start()

        waitUntil("kotlin seeing the CLI", timeoutMs = 15_000) {
            registry.snapshot().any { it.info.fingerprint != kotlinIdentity.fingerprint }
        }

        // Reverse direction: our announces (initial + 5s heartbeat) must land
        // in the resident listener's registry; poll its /peers snapshot
        val stdin = listen.outputStream.bufferedWriter()
        waitUntil("CLI listing kotlin-disco in /peers", timeoutMs = 15_000) {
            stdin.write("/peers\n")
            stdin.flush()
            Thread.sleep(300)
            synchronized(lines) { lines.any { "kotlin-disco" in it } }
        }
    }

    private fun cliBinary(): Path {
        val fromEnv = System.getenv("LANECHO_CLI_BIN")
        if (fromEnv != null) return Path.of(fromEnv)
        return Path.of("../../../target/debug/lanecho-cli").normalize()
    }
}
