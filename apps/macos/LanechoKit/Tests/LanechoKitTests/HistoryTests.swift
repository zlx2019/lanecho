// History store tests: mirrors the Tauri history.rs test surface, plus golden
// samples pinning the two-way on-disk format compatibility

import Foundation
import Testing

@testable import LanechoKit

/// Temporary data directory
private func tempDataDir() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-history-\(UUID().uuidString)")
}

/// Default config (every type recorded)
private let cfg = HistoryConfig(maxEntries: 100)

/// Convenience wrapper for recording text
private func recordText(
    _ store: HistoryStore, _ text: String, at: UInt64,
    origin: String? = nil, sourceApp: String? = nil,
    config: HistoryConfig = cfg
) async -> RecordOutcome {
    await store.record(
        content: .text(text), contentHash: hashText(text), at: at,
        origin: origin, sourceApp: sourceApp, config: config)
}

/// Add and dedup: identical content only bumps the count and refreshes the
/// timestamps, and the origin refresh rules
@Test func recordAddsAndBumps() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    #expect(await recordText(store, "hello", at: 100, sourceApp: "Safari") == .added)
    #expect(await recordText(store, "hello", at: 200) == .bumped)
    let list = await store.list(sort: "recent")
    #expect(list.count == 1)
    #expect(list[0].copyCount == 2)
    #expect(list[0].firstCopiedAt == 100)
    #expect(list[0].lastCopiedAt == 200)
    // A local copy with no source application captured keeps the old value
    #expect(list[0].sourceApp == "Safari")

    // Remote overwrite: origin becomes the device name, the source
    // application is cleared
    #expect(await recordText(store, "hello", at: 300, origin: "Peer device") == .bumped)
    let after = await store.list(sort: "recent")
    #expect(after[0].origin == "Peer device")
    #expect(after[0].sourceApp == nil)

    // Back to a local copy with a source application captured: it overwrites
    // the source application and clears origin
    #expect(await recordText(store, "hello", at: 400, sourceApp: "Notes") == .bumped)
    let final = await store.list(sort: "recent")
    #expect(final[0].origin == nil)
    #expect(final[0].sourceApp == "Notes")
}

/// Type toggles and caps: a disabled type is skipped, oversized text is
/// skipped, and a skip-the-read event is always skipped
@Test func recordRespectsGatesAndCaps() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    let noText = HistoryConfig(maxEntries: 10, recordText: false)
    #expect(await recordText(store, "skip", at: 1, config: noText) == .skipped)

    let huge = String(repeating: "a", count: 5 * 1024 * 1024 + 1)
    #expect(await recordText(store, huge, at: 2) == .skipped)

    let unread = ClipboardContent.imageUnread
    #expect(
        await store.record(
            content: unread, contentHash: unread.hash(), at: 3,
            origin: nil, sourceApp: nil, config: cfg) == .skipped)
    #expect(await store.list(sort: "recent").isEmpty)
}

/// Eviction: over the cap, the oldest unpinned entry goes; pinned entries
/// never take part in eviction
@Test func evictionSkipsPinned() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)
    let small = HistoryConfig(maxEntries: 2)

    _ = await recordText(store, "one", at: 1, config: small)
    _ = await recordText(store, "two", at: 2, config: small)
    let oneId = try #require(await store.list(sort: "recent").last?.id)
    #expect(await store.setPinned(id: oneId, pinned: true))

    // Third entry arrives: "one" is pinned, so "two" is what gets evicted
    _ = await recordText(store, "three", at: 3, config: small)
    let texts = await store.list(sort: "recent").compactMap(\.text)
    #expect(texts.sorted() == ["one", "three"])
}

/// Sort order: pinned entries always on top; frequent by copy count, recent
/// by last copy time
@Test func sortOrders() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    _ = await recordText(store, "old-frequent", at: 1)
    _ = await recordText(store, "old-frequent", at: 2)
    _ = await recordText(store, "old-frequent", at: 3)
    _ = await recordText(store, "newest", at: 100)
    _ = await recordText(store, "pinned", at: 50)
    let pinnedId = try #require(
        await store.list(sort: "recent").first(where: { $0.text == "pinned" })?.id)
    _ = await store.setPinned(id: pinnedId, pinned: true)

    #expect(
        await store.list(sort: "recent").compactMap(\.text)
            == ["pinned", "newest", "old-frequent"])
    #expect(
        await store.list(sort: "frequent").compactMap(\.text)
            == ["pinned", "old-frequent", "newest"])
    // Slot order matches list order
    #expect(await store.entryIdAt(sort: "recent", n: 0) == pinnedId)
}

/// Search: lowercase containment over both preview and full text; an empty
/// query returns everything
@Test func searchMatchesPreviewAndFulltext() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    _ = await recordText(store, "Hello World\nLater line contains SECRET keyword", at: 1)
    _ = await recordText(store, "Another entry", at: 2)
    #expect(await store.search(query: "").count == 2)
    #expect(await store.search(query: "hello").count == 1)
    // The preview is only the first line and secret sits on a later one, so
    // the full text has to match
    #expect(await store.search(query: "secret").count == 1)
    #expect(await store.search(query: "no such content").isEmpty)
}

/// Entry text: truncation counted in unicode scalars plus the total, and the
/// preview generation rule
@Test func entryTextTruncatesByScalars() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    let text = "Café first line\nSecond line"
    _ = await recordText(store, text, at: 1)
    let id = try #require(await store.list(sort: "recent").first?.id)
    let full = try #require(await store.entryText(id: id, maxChars: 100))
    #expect(full.text == text)
    #expect(full.totalChars == text.unicodeScalars.count)
    let cut = try #require(await store.entryText(id: id, maxChars: 4))
    #expect(cut.text == "Café")
    #expect(cut.totalChars == text.unicodeScalars.count)

    // Preview: the first line, with an ellipsis (always present when the text
    // has more than one line)
    #expect(await store.list(sort: "recent").first?.preview == "Café first line…")
}

/// Preview cuts at every line-break flavor. The CRLF case is the regression
/// guard: "\r\n" is a single grapheme cluster, so a Character-level
/// firstIndex(of: "\n") never matched in text synced from Windows peers and
/// the multi-line prefix leaked into the panel row
@Test func previewCutsAtEveryLineBreakFlavor() {
    #expect(previewText("first\r\nsecond\r\nthird") == "first…")
    #expect(previewText("first\rsecond") == "first…")
    #expect(previewText("first\u{2028}second") == "first…")
    #expect(previewText("first\nsecond") == "first…")
    #expect(previewText("no break") == "no break")
}

/// Image records: the blob is content-addressed on disk, restores at the
/// original resolution, and deleting the entry reclaims it
@Test func imageBlobRoundtripAndDelete() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    let rgba = [UInt8](repeating: 180, count: 6 * 4 * 4)
    let content = ClipboardContent.image(width: 6, height: 4, rgba: rgba)
    let hash = content.hash()
    #expect(
        await store.record(
            content: content, contentHash: hash, at: 1,
            origin: nil, sourceApp: nil, config: cfg) == .added)

    // The blob file exists, addressed by its hash
    let blobPath = dir.appendingPathComponent("history/blobs/\(hash).png")
    #expect(FileManager.default.fileExists(atPath: blobPath.path))
    #expect(await store.diskUsage() > 0)

    // The restore path must give back the original resolution
    let restored = try #require(await store.loadImageRGBA(blobHash: hash))
    #expect(restored.width == 6)
    #expect(restored.height == 4)
    #expect(restored.rgba.count == 6 * 4 * 4)

    // Presentation version: small images pass through (≤800 means no
    // downsampling)
    let preview = try #require(await store.previewPNG(blobHash: hash))
    #expect(decodePNG(preview)?.width == 6)

    // Deleting takes the blob and the usage counter with it
    let id = try #require(await store.list(sort: "recent").first?.id)
    #expect(await store.delete(id: id))
    #expect(!FileManager.default.fileExists(atPath: blobPath.path))
    #expect(await store.list(sort: "recent").isEmpty)
}

/// Persistence: a reload after flush compares equal; clear takes the blobs
/// directory with it; orphan blobs are cleaned up on load
@Test func persistenceClearAndOrphanCleanup() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    _ = await recordText(store, "persist-me", at: 7, sourceApp: "App")
    let rgba = [UInt8](repeating: 66, count: 2 * 2 * 4)
    let image = ClipboardContent.image(width: 2, height: 2, rgba: rgba)
    _ = await store.record(
        content: image, contentHash: image.hash(), at: 8,
        origin: "Peer device", sourceApp: nil, config: cfg)
    await store.flush()

    // Reload: entries compare equal field by field
    let reloaded = await HistoryStore.load(dataDir: dir)
    #expect(await reloaded.list(sort: "recent") == store.list(sort: "recent"))

    // Orphan blob: drop in an unreferenced file by hand, a reload clears it
    let orphan = dir.appendingPathComponent("history/blobs/deadbeef.png")
    try Data([1, 2, 3]).write(to: orphan)
    _ = await HistoryStore.load(dataDir: dir)
    #expect(!FileManager.default.fileExists(atPath: orphan.path))

    // Clear: entries and blobs both go
    await store.clear()
    await store.flush()
    #expect(await store.list(sort: "recent").isEmpty)
    #expect(
        !FileManager.default.fileExists(
            atPath: dir.appendingPathComponent("history/blobs").path))
    #expect(await store.diskUsage() < 10)
}

/// Golden sample: a Rust serde-serialized index must load as-is, and the JSON
/// Swift writes back must match serde's key names and omission semantics
/// (camelCase, None never emits a key)
@Test func crossImplementationIndexFormat() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    // The real shape of serde_json::to_vec on the Rust side: compact,
    // camelCase, optional fields omitted
    let rustIndex = """
        [{"id":"6a1f0e1e-9c3f-4c6a-8b5e-000000000001","kind":"text",\
        "text":"hello world","preview":"hello world","contentHash":"abc",\
        "firstCopiedAt":1722700000000,"lastCopiedAt":1722700400000,\
        "copyCount":3,"sourceApp":"Safari","pinned":true},\
        {"id":"6a1f0e1e-9c3f-4c6a-8b5e-000000000002","kind":"image",\
        "blobHash":"ffee00","preview":"800×600","contentHash":"ffee00",\
        "firstCopiedAt":1,"lastCopiedAt":2,"copyCount":1,\
        "origin":"Peer device","pinned":false},\
        {"id":"6a1f0e1e-9c3f-4c6a-8b5e-000000000003","kind":"files",\
        "files":["/tmp/a.txt","/tmp/b.png"],"preview":"a.txt, b.png",\
        "contentHash":"f123","firstCopiedAt":3,"lastCopiedAt":4,\
        "copyCount":2,"pinned":false}]
        """
    let historyDir = dir.appendingPathComponent("history")
    try FileManager.default.createDirectory(at: historyDir, withIntermediateDirectories: true)
    try Data(rustIndex.utf8).write(to: historyDir.appendingPathComponent("index.json"))

    let store = await HistoryStore.load(dataDir: dir)
    let list = await store.list(sort: "recent")
    #expect(list.count == 3)
    let textEntry = try #require(list.first(where: { $0.kind == HistoryKind.text }))
    #expect(textEntry.text == "hello world")
    #expect(textEntry.sourceApp == "Safari")
    #expect(textEntry.pinned)
    let imageEntry = try #require(list.first(where: { $0.kind == HistoryKind.image }))
    #expect(imageEntry.blobHash == "ffee00")
    #expect(imageEntry.origin == "Peer device")
    let filesEntry = try #require(list.first(where: { $0.kind == HistoryKind.files }))
    #expect(filesEntry.files == ["/tmp/a.txt", "/tmp/b.png"])

    // Swift writes it back → reading it again compares equal (stable format),
    // and nil fields emit no key
    await store.flush()
    let written = try String(
        contentsOf: historyDir.appendingPathComponent("index.json"), encoding: .utf8)
    #expect(written.contains("\"contentHash\""), "Keys must use camelCase")
    #expect(!written.contains("\"text\":null"), "None fields must be omitted instead of encoded as null")
    #expect(!written.contains("\"blobHash\":null"))
    let reloaded = await HistoryStore.load(dataDir: dir)
    #expect(await reloaded.list(sort: "recent") == list)

    // Missing-field tolerance (serde default): a file written before new
    // fields existed still loads
    let minimal = #"[{"id":"x","kind":"text","text":"t"}]"#
    try Data(minimal.utf8).write(to: historyDir.appendingPathComponent("index.json"))
    let tolerant = await HistoryStore.load(dataDir: dir)
    let entry = try #require(await tolerant.list(sort: "recent").first)
    #expect(entry.copyCount == 1)
    #expect(!entry.pinned)
}

// MARK: - Regressions

/// Regression: clearing history while an image encodes — nothing may survive
/// the clear
///
/// The old implementation appended right after `await encodePNG`, so a
/// `clear()` landing in between was stepped over: history kept an entry
/// pointing into the already-deleted blobs directory (a broken image), and
/// blobBytes stayed permanently inflated. Asserting `list` is empty holds for
/// both orderings (a clear landing after the record must wipe it too), so
/// correctness does not depend on winning the race; winning it is what
/// additionally catches the old defect.
@Test func clearDuringImageEncodeLeavesNothing() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    // Big enough that PNG encoding takes tens of milliseconds, so clear has a
    // chance to slip into that suspension point
    let (width, height) = (2400, 1600)
    let rgba = [UInt8](repeating: 0x7f, count: width * height * 4)
    let hash = hashText("big-image-blob")

    async let recorded = store.record(
        content: .image(width: width, height: height, rgba: rgba), contentHash: hash,
        at: 100, origin: nil, sourceApp: nil, config: cfg)
    try await Task.sleep(for: .milliseconds(3))
    await store.clear()
    _ = await recorded

    #expect(await store.list(sort: "recent").isEmpty, "Clearing must leave no entries behind")
    // No blob may be left behind either: it would be an orphan and the usage
    // stats would no longer add up
    let blobs =
        (try? FileManager.default.contentsOfDirectory(
            atPath: dir.appendingPathComponent("history/blobs").path)) ?? []
    #expect(blobs.isEmpty, "Clearing must leave no blobs behind")
}

/// Mechanism test: the snapshot mailbox clears on take
///
/// This is the invariant that rules out "a stale snapshot persisted over a
/// newer one" — when two writers race for ioLock, whichever takes it first
/// writes the latest state and the other finds the mailbox empty and skips.
///
/// **Why there is no end-to-end test**: that race needs GCD to schedule the
/// later-dispatched write first (flushLoop enters `lock.withLock` first, so
/// under normal timing it necessarily takes the lock first and the order is
/// correct by construction). Reverting to the old implementation for 200
/// rounds, with hand-placed suspension points, still never triggered it — the
/// race is theoretical and this fix is defensive. An end-to-end test that
/// cannot catch the old defect is no regression guard, so none is written.
@Test func snapshotBoxHandsOutLatestStateOnlyOnce() {
    let box = SnapshotBox()
    let a = HistoryEntry(
        id: "a", contentHash: "ha", firstCopiedAt: 1, lastCopiedAt: 1,
        copyCount: 1, origin: nil, sourceApp: nil)
    let b = HistoryEntry(
        id: "b", contentHash: "hb", firstCopiedAt: 2, lastCopiedAt: 2,
        copyCount: 1, origin: nil, sourceApp: nil)

    box.put([a])
    box.put([a, b])  // Newer state overwrites the older one nobody took
    #expect(box.take()?.count == 2, "The value taken must be the latest state")
    #expect(box.take() == nil, "Taking clears the box; a later writer must not rewrite old state")
}

/// Persistence smoke test: after a run of changes, disk state must equal
/// memory state
@Test func flushKeepsDiskInSyncWithMemory() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)
    _ = await recordText(store, "A", at: 100)
    _ = await recordText(store, "B", at: 200)
    await store.flush()

    let onDisk = await HistoryStore.load(dataDir: dir)
    #expect(await onDisk.list(sort: "recent").count == 2)
}

/// Bulk eviction: when several entries are over the cap at once they go
/// oldest first, and pinned entries always stay
///
/// Covers lowering the cap from a large value to a small one — the next record
/// has to evict a lot of entries in one go. (The old implementation did a
/// filter+min full scan per entry, 190 passes for 200→10; it is now computed
/// in a single pass.)
@Test func evictsManyAtOnceAndKeepsPinned() async throws {
    let dir = tempDataDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let store = await HistoryStore.load(dataDir: dir)

    // Fill 10 entries under a roomy cap, timestamps increasing
    let roomy = HistoryConfig(maxEntries: 100)
    for i in 0..<10 {
        _ = await recordText(store, "e\(i)", at: UInt64(100 + i), config: roomy)
    }
    // Pin the two oldest — they have to survive the bulk eviction
    let all = await store.list(sort: "recent")
    for text in ["e0", "e1"] {
        let id = try #require(all.first(where: { $0.text == text })?.id)
        #expect(await store.setPinned(id: id, pinned: true))
    }

    // Cap drops to 4: the next record has to evict several entries at once
    let tight = HistoryConfig(maxEntries: 4)
    #expect(await recordText(store, "fresh", at: 200, config: tight) == .added)

    let kept = Set(await store.list(sort: "recent").compactMap(\.text))
    #expect(kept.count == 4, "The set must converge to the cap; got \(kept.count)")
    #expect(kept.contains("e0") && kept.contains("e1"), "Pinned entries must not be evicted")
    #expect(kept.contains("fresh"), "The newly recorded entry must remain")
    // Among the unpinned, only the newest one (e9) should remain
    #expect(kept.contains("e9"), "The newest unpinned entry must remain")
    #expect(!kept.contains("e2"), "The older unpinned entry must be evicted")
}
