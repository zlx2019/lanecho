// Cross-implementation interop tests: the Swift engine against a real Rust
// lanecho-cli process.
//
// Covers: BLAKE3 fingerprints agreeing across implementations, mutual
// TLS 1.3 (NIOSSL ↔ rustls), the frame protocol, the Hello gate, the
// pairing transaction (CLI --yes auto-accepts), and the sync transaction
// with byte-for-byte delivery. Both directions are covered: the Swift engine
// driving the CLI, and the CLI discovering the Swift node and syncing back.
//
// Prerequisite: `cargo build -p lanecho-cli` at the repository root; this
// group skips when the binary is missing. CI's macos-native.yml runs it.

import Foundation
import Testing

@testable import LanechoKit

/// Repository root, derived from this file's path; binaries live under
/// target/debug.
private let repoRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // LanechoKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // LanechoKit
    .deletingLastPathComponent()  // macos
    .deletingLastPathComponent()  // apps
    .deletingLastPathComponent()  // repo root

/// Path to the lanecho-cli debug binary.
private let cliBinary = repoRoot.appendingPathComponent("target/debug/lanecho-cli")

/// Line buffer: the background pipe callback feeds it, assertions poll it
/// for the lines they need.
private final class LineBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var lines: [String] = []
    private var partial = ""

    func feed(_ data: Data) {
        guard !data.isEmpty, let chunk = String(data: data, encoding: .utf8) else { return }
        lock.withLock {
            partial += chunk
            while let idx = partial.firstIndex(of: "\n") {
                lines.append(String(partial[..<idx]))
                partial = String(partial[partial.index(after: idx)...])
            }
        }
    }

    func snapshot() -> [String] {
        lock.withLock { lines }
    }

    /// Wait for the first line containing the substring.
    func waitFor(_ substring: String, timeout: Duration = .seconds(10)) async throws -> String {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let hit = snapshot().first(where: { $0.contains(substring) }) {
                return hit
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout("CLI output: \(substring)")
    }

    /// Wait for a line whose last whitespace-delimited field is a port.
    func waitForPort(timeout: Duration = .seconds(10)) async throws -> UInt16 {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            for line in snapshot() {
                if let token = line.split(whereSeparator: \.isWhitespace).last,
                    let port = UInt16(token)
                {
                    return port
                }
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout("CLI listening port")
    }
}

/// Run a CLI subcommand synchronously and take its stdout; for short
/// commands such as id.
private func runCLI(_ args: [String]) throws -> String {
    let process = Process()
    process.executableURL = cliBinary
    process.arguments = args
    let stdout = Pipe()
    process.standardOutput = stdout
    process.standardError = Pipe()
    try process.run()
    process.waitUntilExit()
    return String(decoding: stdout.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
}

/// Extract the 64-character lowercase hexadecimal fingerprint from CLI output
/// without depending on localized field labels.
private func extractCLIFingerprint(_ output: String) -> String? {
    let hexDigits = CharacterSet(charactersIn: "0123456789abcdef")
    for line in output.split(separator: "\n") {
        guard let candidate = line.split(whereSeparator: \.isWhitespace).last,
            candidate.count == 64,
            candidate.unicodeScalars.allSatisfy({ hexDigits.contains($0) })
        else {
            continue
        }
        return String(candidate)
    }
    return nil
}

/// Collector for applyRemote events: a drain task feeds it, assertions poll
/// it against a deadline.
private final class ApplyBucket: @unchecked Sendable {
    private let lock = NSLock()
    private var texts: [String] = []
    private var images: [(width: Int, height: Int, rgba: [UInt8])] = []

    func record(_ event: EngineEvent) {
        switch event {
        case .applyRemote(.text(let text), _, _, _):
            lock.withLock { texts.append(text) }
        case .applyRemote(.image(let width, let height, let rgba), _, _, _):
            lock.withLock { images.append((width, height, rgba)) }
        default:
            break
        }
    }

    /// Wait for the remote-apply event carrying the given text.
    func waitFor(_ text: String, timeout: Duration = .seconds(15)) async throws {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if lock.withLock({ texts.contains(text) }) { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout("applyRemote: \(text)")
    }

    /// Wait for the first remote image to land.
    func waitForImage(timeout: Duration = .seconds(15)) async throws -> (
        width: Int, height: Int, rgba: [UInt8]
    ) {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let hit = lock.withLock({ images.first }) { return hit }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout("applyRemote: image")
    }
}

/// Reverse interop: the CLI discovers the Swift node over real UDP
/// multicast (production port 42525); after pairing, the CLI injects text
/// and the Swift engine receives applyRemote — the Rust client → Swift
/// server inbound sync path, plus mutual discovery in both directions.
///
/// On ports: lanecho-cli's discovery port is fixed at 42525 with no flag to
/// change it, so this test has to use the real port; both sides set
/// SO_REUSEPORT, so it coexists with a resident instance on this machine.
@Test(
    .enabled(if: FileManager.default.fileExists(atPath: cliBinary.path)),
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(2)))
func interopRustCLIDiscoversSwiftAndSyncsBack() async throws {
    // 1. CLI identity + resident listener (random TCP port; discovery
    //    fixed at 42525)
    let cliDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-rev-cli-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: cliDir) }
    let idOutput = try runCLI(["--data-dir", cliDir.path, "id"])
    let cliFingerprint = try #require(extractCLIFingerprint(idOutput))

    let listen = Process()
    listen.executableURL = cliBinary
    listen.arguments = [
        "--data-dir", cliDir.path, "listen", "--port", "0", "--yes", "--no-clipboard",
    ]
    let stdout = Pipe()
    let buffer = LineBuffer()
    stdout.fileHandleForReading.readabilityHandler = { handle in
        buffer.feed(handle.availableData)
    }
    let stdinPipe = Pipe()
    listen.standardOutput = stdout
    listen.standardError = Pipe()
    listen.standardInput = stdinPipe
    try listen.run()
    defer {
        listen.terminate()
        stdout.fileHandleForReading.readabilityHandler = nil
    }
    _ = try await buffer.waitForPort()

    // 2. Swift engine + discovery service (same identity directory);
    //    registry changes feed the engine
    let swiftDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-rev-swift-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: swiftDir) }
    let (engine, engineEvents) = try await SyncEngine.start(
        config: EngineConfig(dataDir: swiftDir, tcpPort: 0))
    defer { Task { await engine.shutdown() } }
    let bucket = ApplyBucket()
    let drain = Task { for await event in engineEvents { bucket.record(event) } }
    defer { drain.cancel() }

    let swiftIdentity = try DeviceIdentity.loadOrCreate(dir: swiftDir)
    let (discovery, discoveryStream) = try await DiscoveryService.start(
        identity: swiftIdentity, tcpPort: await engine.port(), bonjour: false)
    defer { Task { await discovery.shutdown() } }
    let bridge = engine.attachDiscovery(discoveryStream)
    defer { bridge.cancel() }

    // 3. Swift discovers the CLI over UDP heartbeats (one beat every ≤5s,
    //    with a deadline as fallback)
    let deadline = ContinuousClock.now + .seconds(20)
    while ContinuousClock.now < deadline {
        if await engine.peers().contains(where: { $0.info.fingerprint == cliFingerprint }) {
            break
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    #expect(
        await engine.peers().contains { $0.info.fingerprint == cliFingerprint },
        "Swift failed to discover the CLI node over UDP")

    // 4. Pair over the discovered address, not a hand-injected one
    try await engine.pair(fingerprint: cliFingerprint)

    // 5. CLI injects text, equivalent to one local copy → broadcast →
    //    Swift receives applyRemote
    let text = "reverse-🔁-byte-for-byte"
    stdinPipe.fileHandleForWriting.write(Data("\(text)\n".utf8))
    try await bucket.waitFor(text)
}

/// mDNS cross-implementation interop: the CLI registers `_lanecho._tcp`
/// through mdns-sd's own responder, Swift browses, resolves and reads
/// addresses through the system mDNSResponder — the real discovery path
/// between the native and the Tauri client. The UDP port is isolated, so
/// anything discovered here can only have come over the mDNS channel.
@Test(
    .enabled(if: FileManager.default.fileExists(atPath: cliBinary.path)),
    .timeLimit(.minutes(2)))
func interopBonjourSeesRustCLI() async throws {
    // 1. CLI identity + resident listener (mdns-sd registers the service)
    let cliDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-mdns-cli-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: cliDir) }
    let idOutput = try runCLI(["--data-dir", cliDir.path, "id"])
    let cliFingerprint = try #require(extractCLIFingerprint(idOutput))

    let listen = Process()
    listen.executableURL = cliBinary
    listen.arguments = [
        "--data-dir", cliDir.path, "listen", "--port", "0", "--yes", "--no-clipboard",
    ]
    let stdout = Pipe()
    let buffer = LineBuffer()
    stdout.fileHandleForReading.readabilityHandler = { handle in
        buffer.feed(handle.availableData)
    }
    listen.standardOutput = stdout
    listen.standardError = Pipe()
    listen.standardInput = Pipe()
    try listen.run()
    defer {
        listen.terminate()
        stdout.fileHandleForReading.readabilityHandler = nil
    }
    let cliPort = try await buffer.waitForPort()

    // 2. Swift discovery: UDP on an isolated port, deaf to the CLI's 42525,
    //    leaving only mDNS
    let swiftDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-mdns-swift-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: swiftDir) }
    let identity = try DeviceIdentity.loadOrCreate(dir: swiftDir)
    let (discovery, _) = try await DiscoveryService.start(
        identity: identity, tcpPort: 51003,
        discoveryPort: UInt16.random(in: 42600...42999))
    defer { Task { await discovery.shutdown() } }

    // 3. Wait to browse the CLI: registration has to propagate, then
    //    resolve, then yield addresses — 20s to be safe
    let deadline = ContinuousClock.now + .seconds(20)
    var found: Peer?
    while ContinuousClock.now < deadline {
        if let hit = await discovery.peers().first(where: {
            $0.info.fingerprint == cliFingerprint
        }) {
            found = hit
            break
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    let peer = try #require(found, "Swift failed to discover the CLI through Bonjour")
    #expect(peer.port == cliPort, "The port must come from the SRV record")
    #expect(!peer.addrs.isEmpty)
    #expect(peer.info.name.isEmpty == false, "The TXT record must carry a name")
}

/// The Swift engine dials the Rust CLI: fingerprints agree → pairing →
/// sync delivers byte-for-byte.
@Test(
    .enabled(if: FileManager.default.fileExists(atPath: cliBinary.path)),
    .timeLimit(.minutes(2)))
func interopSwiftEngineTalksToRustCLI() async throws {
    // 1. The CLI's own identity + fingerprint
    let cliDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-cli-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: cliDir) }
    let idOutput = try runCLI(["--data-dir", cliDir.path, "id"])
    let cliFingerprint = try #require(extractCLIFingerprint(idOutput))
    #expect(cliFingerprint.count == 64, "The id output must contain a 64-character fingerprint: \(idOutput)")

    // 2. Resident CLI: random port, auto-accepts pairing, never touches the
    //    system clipboard
    let listen = Process()
    listen.executableURL = cliBinary
    listen.arguments = [
        "--data-dir", cliDir.path, "listen", "--port", "0", "--yes", "--no-clipboard",
    ]
    let stdout = Pipe()
    let buffer = LineBuffer()
    stdout.fileHandleForReading.readabilityHandler = { handle in
        buffer.feed(handle.availableData)
    }
    listen.standardOutput = stdout
    listen.standardError = Pipe()
    listen.standardInput = Pipe()  // keep stdin open: the CLI injects from it
    try listen.run()
    defer {
        listen.terminate()
        stdout.fileHandleForReading.readabilityHandler = nil
    }
    // The startup line ends with the listening port.
    let cliPort = try await buffer.waitForPort()

    // 3. The Swift engine, under its own identity, injects the CLI as an
    //    online peer and starts pairing
    let swiftDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-swift-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: swiftDir) }
    let (engine, _) = try await SyncEngine.start(
        config: EngineConfig(dataDir: swiftDir, tcpPort: 0))
    defer { Task { await engine.shutdown() } }
    await engine.peerUp(
        Peer(
            info: PeerInfo(
                deviceId: "cli", name: "cli",
                fingerprint: cliFingerprint, platform: "rust"),
            addrs: ["127.0.0.1"], port: cliPort))
    try await engine.pair(fingerprint: cliFingerprint)
    #expect(await engine.pairedList().map(\.fingerprint) == [cliFingerprint])

    // 4. Sync: the CLI output includes the text payload.
    let text = "interop-🚀-byte-for-byte"
    await engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    _ = try await buffer.waitFor(text)
}

/// Blob interop (CLI → Swift image): the full cross-implementation chain of
/// image offer + raw stream + checksum. The deterministic pattern the CLI
/// injects (the formula mirrors the CLI's /image implementation) must come
/// back as byte-for-byte identical RGBA on the Swift side — the pattern is
/// fully opaque, so premultiplied alpha rewrites nothing.
@Test(
    .enabled(if: FileManager.default.fileExists(atPath: cliBinary.path)),
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(2)))
func interopRustCLISendsImageToSwift() async throws {
    let cliDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-img-cli-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: cliDir) }
    let idOutput = try runCLI(["--data-dir", cliDir.path, "id"])
    let cliFingerprint = try #require(extractCLIFingerprint(idOutput))

    let listen = Process()
    listen.executableURL = cliBinary
    listen.arguments = [
        "--data-dir", cliDir.path, "listen", "--port", "0", "--yes", "--no-clipboard",
    ]
    let stdout = Pipe()
    let buffer = LineBuffer()
    stdout.fileHandleForReading.readabilityHandler = { handle in
        buffer.feed(handle.availableData)
    }
    let stdinPipe = Pipe()
    listen.standardOutput = stdout
    listen.standardError = Pipe()
    listen.standardInput = stdinPipe
    try listen.run()
    defer {
        listen.terminate()
        stdout.fileHandleForReading.readabilityHandler = nil
    }
    _ = try await buffer.waitForPort()

    let swiftDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-img-swift-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: swiftDir) }
    let (engine, engineEvents) = try await SyncEngine.start(
        config: EngineConfig(dataDir: swiftDir, tcpPort: 0))
    defer { Task { await engine.shutdown() } }
    let bucket = ApplyBucket()
    let drain = Task { for await event in engineEvents { bucket.record(event) } }
    defer { drain.cancel() }

    let swiftIdentity = try DeviceIdentity.loadOrCreate(dir: swiftDir)
    let (discovery, discoveryStream) = try await DiscoveryService.start(
        identity: swiftIdentity, tcpPort: await engine.port(), bonjour: false)
    defer { Task { await discovery.shutdown() } }
    let bridge = engine.attachDiscovery(discoveryStream)
    defer { bridge.cancel() }

    let deadline = ContinuousClock.now + .seconds(20)
    while ContinuousClock.now < deadline {
        if await engine.peers().contains(where: { $0.info.fingerprint == cliFingerprint }) {
            break
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    try await engine.pair(fingerprint: cliFingerprint)

    // The CLI injects a deterministic 8×6 pattern → blob transaction →
    // Swift applyRemote
    stdinPipe.fileHandleForWriting.write(Data("/image 8x6\n".utf8))
    let image = try await bucket.waitForImage()
    #expect(image.width == 8)
    #expect(image.height == 6)
    var expected = [UInt8]()
    for pixel in 0..<8 * 6 {
        expected.append(contentsOf: [
            UInt8(pixel * 7 % 251), UInt8(pixel * 11 % 241), UInt8(pixel * 13 % 239), 0xFF,
        ])
    }
    #expect(image.rgba == expected, "Cross-implementation PNG coding must restore byte-identical RGBA")
}

/// Blob interop (Swift → CLI, images and files): the Swift dialer's
/// offer / raw stream / footer are accepted by the Rust receiver, and files
/// land byte-for-byte under sync-files in the CLI's data directory.
@Test(
    .enabled(if: FileManager.default.fileExists(atPath: cliBinary.path)),
    .timeLimit(.minutes(2)))
func interopSwiftSendsBlobsToRustCLI() async throws {
    let cliDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-blob-cli-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: cliDir) }
    let idOutput = try runCLI(["--data-dir", cliDir.path, "id"])
    let cliFingerprint = try #require(extractCLIFingerprint(idOutput))

    let listen = Process()
    listen.executableURL = cliBinary
    listen.arguments = [
        "--data-dir", cliDir.path, "listen", "--port", "0", "--yes", "--no-clipboard",
    ]
    let stdout = Pipe()
    let buffer = LineBuffer()
    stdout.fileHandleForReading.readabilityHandler = { handle in
        buffer.feed(handle.availableData)
    }
    listen.standardOutput = stdout
    listen.standardError = Pipe()
    listen.standardInput = Pipe()
    try listen.run()
    defer {
        listen.terminate()
        stdout.fileHandleForReading.readabilityHandler = nil
    }
    let cliPort = try await buffer.waitForPort()

    // Turn on file sync in the Swift engine (off by default) and inject the
    // CLI as a directly reachable peer
    let swiftDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-interop-blob-swift-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: swiftDir) }
    let (engine, _) = try await SyncEngine.start(
        config: EngineConfig(
            dataDir: swiftDir, tcpPort: 0,
            syncTypes: SyncTypes(text: true, images: true, files: true)))
    defer { Task { await engine.shutdown() } }
    await engine.peerUp(
        Peer(
            info: PeerInfo(
                deviceId: "cli", name: "cli",
                fingerprint: cliFingerprint, platform: "rust"),
            addrs: ["127.0.0.1"], port: cliPort))
    try await engine.pair(fingerprint: cliFingerprint)

    // Image: the CLI output includes the invariant dimensions.
    var rgba = [UInt8]()
    for pixel in 0..<8 * 6 {
        rgba.append(contentsOf: [
            UInt8(pixel * 7 % 251), UInt8(pixel * 11 % 241), UInt8(pixel * 13 % 239), 0xFF,
        ])
    }
    let image = ClipboardContent.image(width: 8, height: 6, rgba: rgba)
    await engine.clipboardChanged(
        ClipboardEvent(content: image, hash: image.hash(), timestampMs: nowMs()))
    _ = try await buffer.waitFor("8×6")

    // Files: on the CLI side they land under <data>/sync-files/<batch>/,
    // byte-for-byte identical
    let payload = Data("interop-file-📦-bytes".utf8)
    let source = swiftDir.appendingPathComponent("payload.txt")
    try payload.write(to: source)
    let files = ClipboardContent.files([source.path])
    await engine.clipboardChanged(
        ClipboardEvent(content: files, hash: files.hash(), timestampMs: nowMs()))
    _ = try await buffer.waitFor("×1")

    let syncRoot = cliDir.appendingPathComponent("sync-files")
    let batches = try FileManager.default.contentsOfDirectory(
        at: syncRoot, includingPropertiesForKeys: nil)
    #expect(batches.count == 1, "There must be exactly one received batch directory")
    let landed = try #require(batches.first?.appendingPathComponent("payload.txt"))
    #expect(try Data(contentsOf: landed) == payload, "Landed content must be byte-for-byte identical")
}
