// Live clipboard tests: they overwrite the system clipboard, so they are
// skipped by default and run by hand only (matching the ignored convention of
// the Rust clipboard_roundtrip test):
//
//   LANECHO_CLIPBOARD_TESTS=1 swift test --filter Pasteboard
//
// The whole suite is .serialized: NSPasteboard is a global singleton,
// mutating it concurrently segfaults, and the two tests would clobber each
// other's content.

import AppKit
import Foundation
import Testing

@testable import LanechoKit

/// Whether the manual opt-in is set; CI and ordinary local runs skip these
private let clipboardTestsEnabled =
    ProcessInfo.processInfo.environment["LANECHO_CLIPBOARD_TESTS"] == "1"

/// Event collector: fed by a drain task, awaited by bounded polling
private final class EventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [ClipboardEvent] = []

    func append(_ event: ClipboardEvent) {
        lock.withLock { events.append(event) }
    }

    func snapshot() -> [ClipboardEvent] {
        lock.withLock { events }
    }

    /// Wait for the first event matching the predicate
    func waitFor(
        _ describe: String, timeout: Duration = .seconds(5),
        where predicate: (ClipboardEvent) -> Bool
    ) async throws -> ClipboardEvent {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let hit = lock.withLock({ events.first(where: predicate) }) {
                return hit
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout(describe)
    }
}

/// Live clipboard test suite (serialized)
@Suite(.serialized) struct PasteboardManualTests {
    /// Watcher loop: writing text emits an event → rewriting the same text is
    /// deduped → a concealed marker is skipped → skipping the read emits
    /// ImageUnread → with "record images" on, all pixels are read
    @Test(.enabled(if: clipboardTestsEnabled), .timeLimit(.minutes(1)))
    func watchRoundtrip() async throws {
        let readImages = AtomicFlag(false)
        let (watcher, stream) = PasteboardWatcher.start(
            readImages: readImages, resetBaseline: AtomicFlag(false))
        defer { watcher.stop() }
        let log = EventLog()
        let drain = Task { for await event in stream { log.append(event) } }
        defer { drain.cancel() }
        // Let the baseline settle: startup racing the first write swallows the
        // new content as the baseline
        try await Task.sleep(for: .milliseconds(600))

        // 1. Write text → the event matches byte-for-byte
        let text = "lanecho-test-🚀 with spaces "
        try await writeClipboardText(text)
        let event = try await log.waitFor("text event") {
            if case .text(let seen) = $0.content { seen == text } else { false }
        }
        #expect(event.hash == hashText(text))

        // 2. Rewrite the same text (changeCount moves, content does not) →
        //    deduped, no further text event
        let before = log.snapshot().count
        try await writeClipboardText(text)
        try await Task.sleep(for: .seconds(1))
        #expect(log.snapshot().count == before, "Rewriting identical content must not emit another event")

        // 3. Concealed marker → skipped entirely
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(
            "hunter2", forType: NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType"))
        pasteboard.setString("hunter2", forType: .string)
        try await Task.sleep(for: .seconds(1))
        #expect(
            log.snapshot().allSatisfy {
                if case .text(let seen) = $0.content { seen != "hunter2" } else { true }
            }, "Sensitive content must not emit an event")

        // 4. Write an image with "record images" off → skip-the-read event,
        //    carrying no pixels
        try await writeClipboardImage(
            width: 2, height: 2, rgba: [UInt8](repeating: 128, count: 16))
        _ = try await log.waitFor("read-skipped event") {
            if case .imageUnread = $0.content { true } else { false }
        }

        // 5. Turn "record images" on and write a different image → full-pixel
        //    event at the original dimensions
        readImages.set(true)
        try await writeClipboardImage(
            width: 3, height: 2, rgba: [UInt8](repeating: 200, count: 24))
        let imageEvent = try await log.waitFor("image event") {
            if case .image = $0.content { true } else { false }
        }
        guard case .image(let width, let height, let rgba) = imageEvent.content else { return }
        #expect(width == 3 && height == 2)
        #expect(rgba.count == 24)
    }

    /// Write/read round trip: text byte-for-byte, file lists intact, image
    /// dimensions preserved
    @Test(.enabled(if: clipboardTestsEnabled), .timeLimit(.minutes(1)))
    func writeReadRoundtrip() async throws {
        let text = "loopback\nsecond line\ttab trailing space "
        try await writeClipboardText(text)
        #expect(await readClipboard() == .text(text))

        let paths = ["/tmp", "/usr"]
        try await writeClipboardFiles(paths)
        guard case .files(let seen) = await readClipboard() else {
            Issue.record("The file list should be read back")
            return
        }
        #expect(Set(seen.map { $0.hasSuffix("/") ? String($0.dropLast()) : $0 }) == Set(paths))

        try await writeClipboardImage(width: 4, height: 3, rgba: [UInt8](repeating: 66, count: 48))
        guard case .image(let width, let height, _) = await readClipboard() else {
            Issue.record("The image should be read back")
            return
        }
        #expect(width == 4 && height == 3, "Restore must preserve the original resolution")
    }
}
