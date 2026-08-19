package io.github.zlx2019.lanecho.core.transport

import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.protocol.ReasonCode
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.Collections
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlin.test.fail

// Cross-implementation interop against the real Rust CLI (the same pattern as
// the Swift InteropTests): the CLI is the reference peer already pinned by the
// desktop test suites. Kotlin dials; the reverse direction (CLI discovers us
// over multicast) arrives with the discovery milestone.
//
// Skipped when the CLI binary is absent — build it with
// `cargo build -p lanecho-cli`, or point LANECHO_CLI_BIN at it.
class RustCliInteropTest {

    @TempDir
    lateinit var dir: Path

    private val processes = mutableListOf<Process>()

    @AfterEach
    fun tearDown() {
        processes.forEach { it.destroy() }
        processes.forEach { if (it.isAlive) it.destroyForcibly() }
    }

    private fun cliBinary(): Path {
        val fromEnv = System.getenv("LANECHO_CLI_BIN")
        if (fromEnv != null) return Path.of(fromEnv)
        // Test working directory is the core module dir (apps/android/core)
        return Path.of("../../../target/debug/lanecho-cli").normalize()
    }

    private fun runCli(binary: Path, vararg args: String): String {
        val process = ProcessBuilder(listOf(binary.toString()) + args)
            .redirectErrorStream(true).start()
        val output = process.inputStream.readBytes().decodeToString()
        check(process.waitFor() == 0) { "cli ${args.joinToString(" ")} failed: $output" }
        return output
    }

    /** 64 lowercase hex chars as the last field of any line (locale-proof). */
    private fun extractFingerprint(output: String): String {
        for (line in output.lines()) {
            val token = line.trim().split(Regex("\\s+")).lastOrNull() ?: continue
            if (token.length == 64 && token.all { it in "0123456789abcdef" }) return token
        }
        fail("no fingerprint in CLI output: $output")
    }

    private class LineSink {
        val lines: MutableList<String> = Collections.synchronizedList(mutableListOf())

        fun waitForLine(what: String, timeoutMs: Long = 15_000, match: (String) -> Boolean): String {
            val deadline = System.currentTimeMillis() + timeoutMs
            while (System.currentTimeMillis() < deadline) {
                synchronized(lines) { lines.firstOrNull(match) }?.let { return it }
                Thread.sleep(50)
            }
            fail("timed out waiting for $what; CLI output so far: ${lines.joinToString("\n")}")
        }
    }

    /** Start `listen --port 0 --yes --no-clipboard`, returning its bound port and stdout sink. */
    private fun startListen(binary: Path, dataDir: Path): Pair<Int, LineSink> {
        val process = ProcessBuilder(
            binary.toString(), "--data-dir", dataDir.toString(),
            "listen", "--port", "0", "--yes", "--no-clipboard",
        ).redirectErrorStream(true).start()
        processes.add(process)
        val sink = LineSink()
        Thread({
            process.inputStream.bufferedReader().forEachLine { sink.lines.add(it) }
        }, "cli-stdout").apply { isDaemon = true }.start()
        // The listen banner ends a line with the bound port (same heuristic as
        // the Swift interop suite)
        val portLine = sink.waitForLine("CLI listening port") { line ->
            line.trim().split(Regex("\\s+")).lastOrNull()?.toIntOrNull() in 1..65535
        }
        val port = portLine.trim().split(Regex("\\s+")).last().toInt()
        return port to sink
    }

    private fun kotlinInfo(identity: DeviceIdentity) = PeerInfo(
        deviceId = identity.deviceId,
        name = "kotlin-interop",
        fingerprint = identity.fingerprint,
        platform = "android",
        osVersion = "test",
    )

    @Test
    fun pairsAndSyncsTextWithRustCli() {
        val binary = cliBinary()
        assumeTrue(Files.isExecutable(binary), "lanecho-cli not built")
        val cliDir = Files.createDirectories(dir.resolve("cli"))
        val cliFingerprint = extractFingerprint(runCli(binary, "--data-dir", cliDir.toString(), "id"))
        val (port, sink) = startListen(binary, cliDir)

        val identity = DeviceIdentity.loadOrCreate(Files.createDirectories(dir.resolve("kotlin")))
        val target = DialTarget(listOf("127.0.0.1"), port, cliFingerprint)

        // Pairing: --yes auto-accepts, the response must carry the CLI identity
        val remote = Sessions.pair(identity, kotlinInfo(identity), target)
        assertEquals(cliFingerprint, remote.fingerprint)

        // Text sync: protocol-level Ack plus the CLI actually printing the
        // received payload (short enough to survive its preview truncation)
        val payload = "互通 interop🚀"
        Sessions.syncText(identity, kotlinInfo(identity), target, seq = 1, timestampMs = System.currentTimeMillis(), text = payload)
        val received = sink.waitForLine("CLI receiving the payload") { payload in it }
        assertTrue("kotlin-interop" in received, "sender name missing in: $received")
    }

    @Test
    fun unpairedSyncIsRefusedByRustCli() {
        val binary = cliBinary()
        assumeTrue(Files.isExecutable(binary), "lanecho-cli not built")
        val cliDir = Files.createDirectories(dir.resolve("cli-unpaired"))
        val cliFingerprint = extractFingerprint(runCli(binary, "--data-dir", cliDir.toString(), "id"))
        val (port, _) = startListen(binary, cliDir)

        val identity = DeviceIdentity.loadOrCreate(Files.createDirectories(dir.resolve("kotlin-unpaired")))
        val target = DialTarget(listOf("127.0.0.1"), port, cliFingerprint)

        val e = assertFailsWith<SessionException.Rejected> {
            Sessions.syncText(identity, kotlinInfo(identity), target, 1, System.currentTimeMillis(), "should be refused")
        }
        assertEquals(ReasonCode.NOT_PAIRED, e.reasonCode)
    }

    @Test
    fun unpairDropsThePairingOnRustCli() {
        val binary = cliBinary()
        assumeTrue(Files.isExecutable(binary), "lanecho-cli not built")
        val cliDir = Files.createDirectories(dir.resolve("cli-unpair"))
        val cliFingerprint = extractFingerprint(runCli(binary, "--data-dir", cliDir.toString(), "id"))
        val (port, _) = startListen(binary, cliDir)

        val identity = DeviceIdentity.loadOrCreate(Files.createDirectories(dir.resolve("kotlin-unpair")))
        val target = DialTarget(listOf("127.0.0.1"), port, cliFingerprint)

        Sessions.pair(identity, kotlinInfo(identity), target)
        Sessions.syncText(identity, kotlinInfo(identity), target, 1, System.currentTimeMillis(), "before unpair")

        Sessions.unpair(identity, kotlinInfo(identity), target)
        // The pairing check lives on the receiving side: the next sync must
        // be refused as not_paired
        val e = assertFailsWith<SessionException.Rejected> {
            Sessions.syncText(identity, kotlinInfo(identity), target, 2, System.currentTimeMillis(), "after unpair")
        }
        assertEquals(ReasonCode.NOT_PAIRED, e.reasonCode)
    }
}
