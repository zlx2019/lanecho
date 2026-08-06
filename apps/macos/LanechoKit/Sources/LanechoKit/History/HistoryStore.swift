// History storage engine (mirrors HistoryStore in the Tauri client's
// history.rs)
//
// On-disk layout (byte-for-byte the same format as the Tauri client, read and
// written by both):
// - <data_dir>/history/index.json    entry array, compact JSON, atomic write
// - <data_dir>/history/blobs/<content_hash>.png   image bytes, content-addressed
// - <data_dir>/history/appicons/<hashText(app name)>.png  source app icon cache
//
// Concurrency model: actor isolation replaces the Rust Mutex; disk IO leaves
// the cooperative thread pool through runBlocking; persistence runs on a dirty
// flag plus a single-writer loop (reset the flag before taking the snapshot,
// so a burst of changes collapses into one full write), and the write itself
// is serialized by ioLock (mutually exclusive with the direct write in flush,
// so interleaved writes cannot tear index.json).
// A serialization failure **never writes to disk** (an overwrite with empty
// bytes silently wipes the history).

import Foundation

/// Per-entry text cap (5MB; anything larger is not recorded)
private let maxTextBytes = 5 * 1024 * 1024
/// Per-entry image PNG cap (16MB; a 4K fullscreen screenshot has to fit)
private let maxImagePNGBytes = 16 * 1024 * 1024
/// Rough raw-pixel cap checked before encoding (an image certain to exceed the
/// limit is not worth seconds of wasted encoding)
private let maxRawImageBytes = 128 * 1024 * 1024
/// Capacity of the preview card downsample cache (content-addressed, a small
/// LRU ring that never expires)
private let previewCacheCap = 12
/// Long-edge cap of the preview card's display version (pixels)
private let previewMaxPixel = 800

/// Type toggles and capacity snapshot used while recording (taken from
/// settings)
public struct HistoryConfig: Sendable {
    /// Entry cap
    public var maxEntries: Int
    /// Record text
    public var recordText: Bool
    /// Record images
    public var recordImages: Bool
    /// Record file references
    public var recordFiles: Bool

    /// Field-by-field init
    public init(
        maxEntries: Int, recordText: Bool = true,
        recordImages: Bool = true, recordFiles: Bool = true
    ) {
        self.maxEntries = maxEntries
        self.recordText = recordText
        self.recordImages = recordImages
        self.recordFiles = recordFiles
    }
}

/// Mailbox for the snapshot awaiting persistence (posted after an actor
/// mutation, taken by the writing thread **inside** ioLock)
///
/// Taking it empties it: when two writers race for ioLock, whichever gets the
/// lock first writes the latest state and the other finds the mailbox empty
/// and skips. That eliminates an older snapshot landing after a newer one
/// (taking the snapshot outside the lock is unsafe: NSLock is not fair, a
/// later snapshot can win the lock first, and flushLoop does not recover).
final class SnapshotBox: @unchecked Sendable {
    private let lock = NSLock()
    private var snapshot: [HistoryEntry]?

    /// Posts the latest state (overwriting a value not yet taken)
    func put(_ entries: [HistoryEntry]) {
        lock.withLock { snapshot = entries }
    }

    /// Takes the pending snapshot (nil when nothing is pending)
    func take() -> [HistoryEntry]? {
        lock.withLock {
            defer { snapshot = nil }
            return snapshot
        }
    }
}

/// Outcome of a record call
public enum RecordOutcome: Sendable, Equatable {
    /// A new entry was added
    case added
    /// An existing entry was hit (only the count and timestamps refreshed)
    case bumped
    /// Skipped by a type toggle or a size cap
    case skipped
}

/// History storage: in-memory table plus disk persistence
public actor HistoryStore {
    /// History directory (<data_dir>/history)
    private let dir: URL
    /// Landing root for files received from remotes (`<data_dir>/sync-files`,
    /// **not** below dir): it has to share the root the engine lands files in.
    /// Deriving it from the store directory instead points cascade deletion,
    /// startup sweeping and usage accounting at an empty directory, where all
    /// three spin for nothing.
    private let syncRoot: URL
    /// Entry table (no fixed order; sorting happens in list)
    private var entries: [HistoryEntry]
    /// Dirty flag for persistence (the single-writer loop resets it before
    /// taking the snapshot)
    private var savePending = false
    /// Whether the single writer is currently in its loop
    private var saving = false
    /// Serialization lock for disk writes (shared by the async loop and the
    /// direct write in flush; acquired on a blocking thread)
    private let ioLock = NSLock()
    /// Mailbox for the snapshot awaiting persistence (see [`SnapshotBox`]; the
    /// snapshot must be taken inside ioLock)
    private let pendingSnapshot = SnapshotBox()
    /// Clear generation (bumped by clear; record checks it after resuming
    /// across an await to decide whether to discard its work)
    private var clearGeneration: UInt64 = 0
    /// Running total of the bytes blobs occupy (initialized on load, adjusted
    /// on write and delete)
    private var blobBytes: UInt64
    /// Preview card downsample cache (key = blob_hash; newest at the head)
    private var previewCache: [(hash: String, png: Data)] = []

    private init(dir: URL, syncRoot: URL, entries: [HistoryEntry], blobBytes: UInt64) {
        self.dir = dir
        self.syncRoot = syncRoot
        self.entries = entries
        self.blobBytes = blobBytes
    }

    /// Loads from the data directory (a missing or corrupt file yields an
    /// empty table); also cleans up orphan blobs and initializes the usage
    /// counter
    public static func load(dataDir: URL) async -> HistoryStore {
        let dir = dataDir.appendingPathComponent("history")
        let (entries, blobBytes) = await runBlocking { () -> ([HistoryEntry], UInt64) in
            let entries =
                (try? Data(contentsOf: dir.appendingPathComponent("index.json")))
                .flatMap { try? JSONDecoder().decode([HistoryEntry].self, from: $0) } ?? []
            // Orphan blob cleanup: blobs and the index are not persisted
            // atomically, so a crash race can leave unreferenced files behind
            // (reported usage inflated, never reclaimed); the same pass
            // accumulates the initial usage total
            let referenced = Set(entries.compactMap(\.blobHash))
            var blobBytes: UInt64 = 0
            let blobsDir = dir.appendingPathComponent("blobs")
            let files =
                (try? FileManager.default.contentsOfDirectory(
                    at: blobsDir, includingPropertiesForKeys: [.fileSizeKey])) ?? []
            for file in files {
                let hash = file.deletingPathExtension().lastPathComponent
                if referenced.contains(hash) {
                    let size = (try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
                    blobBytes += UInt64(max(0, size))
                } else {
                    try? FileManager.default.removeItem(at: file)
                }
            }
            return (entries, blobBytes)
        }
        return HistoryStore(
            dir: dir, syncRoot: dataDir.appendingPathComponent(syncFilesDirName),
            entries: entries, blobBytes: blobBytes)
    }

    /// Sweeps orphan sync-files batches (called once at startup)
    ///
    /// With "record files" off, an incoming remote file is still written to
    /// disk and to the clipboard but gets no history entry — nothing
    /// references it, so cascade deletion never fires. That case, plus `.part`
    /// leftovers from a crash, is what this fallback cleans up.
    public func sweepOrphanSyncFiles() async {
        let root = syncRoot
        let rootPrefix = root.path + "/"
        let referenced = Set(
            entries.compactMap(\.files).joined()
                .filter { $0.hasPrefix(rootPrefix) }
                .map { URL(fileURLWithPath: $0).deletingLastPathComponent().path })
        await runBlocking {
            guard
                let children = try? FileManager.default.contentsOfDirectory(
                    at: root, includingPropertiesForKeys: [.isDirectoryKey])
            else { return }
            for child in children {
                let isDir =
                    (try? child.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory
                    ?? false
                guard isDir, !referenced.contains(child.path) else { continue }
                try? FileManager.default.removeItem(at: child)
            }
        }
    }

    // MARK: - Recording

    /// Records one clipboard change (the caller passes the hash so a large
    /// image is not hashed twice)
    ///
    /// A dedup hit only bumps the count. Origin refresh rules: a remote write
    /// has a definite origin (the device name, and the source application is
    /// cleared); a local copy overwrites only when a source application was
    /// **captured** — nil (capture failed, restore write, or our own app in
    /// front) keeps the old value so the original source is not lost.
    public func record(
        content: ClipboardContent, contentHash: String, at: UInt64,
        origin: String?, sourceApp: String?, config: HistoryConfig
    ) async -> RecordOutcome {
        // Type toggles (a skip-the-read event only arises with
        // recordImages = false and carries no content to record)
        let enabled =
            switch content {
            case .text: config.recordText
            case .image: config.recordImages
            case .files: config.recordFiles
            case .imageUnread: false
            }
        guard enabled else { return .skipped }

        // Dedup: a hit only refreshes count, timestamps and origin
        if let index = entries.firstIndex(where: { $0.contentHash == contentHash }) {
            bump(index: index, at: at, origin: origin, sourceApp: sourceApp)
            return .bumped
        }

        // New entry
        var entry = HistoryEntry(
            id: UUID().uuidString.lowercased(), contentHash: contentHash,
            firstCopiedAt: at, lastCopiedAt: at, copyCount: 1,
            origin: origin, sourceApp: sourceApp)
        switch content {
        case .text(let text):
            guard text.utf8.count <= maxTextBytes else { return .skipped }
            entry.kind = HistoryKind.text
            entry.preview = previewText(text)
            entry.text = text
        case .image(let width, let height, let rgba):
            // Encoding and the write both run in a blocking context;
            // content-addressed, so an existing blob is not rewritten
            guard rgba.count <= maxRawImageBytes else { return .skipped }
            let blobsDir = dir.appendingPathComponent("blobs")
            let path = blobPath(contentHash)
            // Encoding is an actor suspension point (tens to hundreds of
            // milliseconds for a large image), during which clear or another
            // record can slip in — the state must be revalidated on resume
            let generation = clearGeneration
            let written = await runBlocking { () -> UInt64? in
                guard let png = encodePNG(width: width, height: height, rgba: rgba),
                    png.count <= maxImagePNGBytes
                else { return nil }
                if FileManager.default.fileExists(atPath: path.path) {
                    return 0
                }
                do {
                    try FileManager.default.createDirectory(
                        at: blobsDir, withIntermediateDirectories: true)
                    try png.write(to: path)
                    return UInt64(png.count)
                } catch {
                    return nil
                }
            }
            guard let addedBytes = written else { return .skipped }
            // The user cleared the history while we were encoding: discard
            // this record and delete the blob just written — otherwise a
            // "clear" leaves a broken-image entry pointing into a deleted
            // directory, with the usage counter inflated on top
            guard generation == clearGeneration else {
                if addedBytes > 0 {
                    await runBlocking { try? FileManager.default.removeItem(at: path) }
                }
                return .skipped
            }
            // Another path recorded the same content while we were encoding:
            // fall back to bump semantics instead of creating a duplicate
            // entry (blobs are content-addressed 1:1, so the one just written
            // is exactly the one that entry references — no deletion needed)
            if let index = entries.firstIndex(where: { $0.contentHash == contentHash }) {
                blobBytes += addedBytes
                bump(index: index, at: at, origin: origin, sourceApp: sourceApp)
                return .bumped
            }
            blobBytes += addedBytes
            entry.kind = HistoryKind.image
            entry.preview = "\(width)×\(height)"
            entry.blobHash = contentHash
        case .files(let paths):
            entry.kind = HistoryKind.files
            entry.preview = previewFiles(paths)
            entry.files = paths
        case .imageUnread:
            // Already filtered out by the type toggles, so unreachable; fall
            // back to skipping rather than trapping
            return .skipped
        }

        // Insert and evict: the oldest unpinned entries; nothing can be
        // evicted when every entry is pinned
        //
        // Work out how many to evict once and remove them in one batch instead
        // of running filter+min per entry — lowering the cap from 200 to 10
        // would otherwise mean 190 full-table scans
        entries.append(entry)
        var evicted: [HistoryEntry] = []
        let overflow = entries.count - max(config.maxEntries, 1)
        if overflow > 0 {
            let victims =
                entries.indices
                .filter { !entries[$0].pinned }
                .sorted { entries[$0].lastCopiedAt < entries[$1].lastCopiedAt }
                .prefix(overflow)
            // Remove in descending order so earlier removals do not shift the
            // indices still to come
            for index in victims.sorted(by: >) {
                evicted.append(entries.remove(at: index))
            }
        }
        for old in evicted {
            await removeBlob(of: old)
        }
        save()
        return .added
    }

    // MARK: - Queries

    /// Entry list (with inlined full text; ordering per [`sortedEntries`])
    public func list(sort: String) -> [HistoryEntry] {
        sortedEntries(entries, sort: sort)
    }

    /// List projection (carries no full text; used by the panel list)
    public func listMeta(sort: String) -> [HistoryEntryMeta] {
        sortedEntries(entries, sort: sort).map(HistoryEntryMeta.of)
    }

    /// ID of the nth entry after sorting (for slot hotkeys; same comparator as
    /// the list order)
    public func entryIdAt(sort: String, n: Int) -> String? {
        let sorted = sortedEntries(entries, sort: sort)
        guard n < sorted.count else { return nil }
        return sorted[n].id
    }

    /// Search (lowercased containment; matches preview plus full text):
    /// returns the IDs that hit
    ///
    /// The semantics must line up exactly with the Rust side's
    /// `to_lowercase().contains()` (the two are clients of one product, so the
    /// same history has to search identically), which is why this
    /// **deliberately** avoids `range(of:options:.caseInsensitive)` — that one
    /// diverges from whole-string lowercasing on edge mappings such as Turkish
    /// İ and German ß. The preview is short, `||` short-circuits, and entry
    /// text normally stays far below the 5MB cap, so the cost is a non-issue.
    public func search(query: String) -> [String] {
        let needle = query.lowercased()
        if needle.isEmpty {
            return entries.map(\.id)
        }
        return
            entries
            .filter {
                $0.preview.lowercased().contains(needle)
                    || $0.text?.lowercased().contains(needle) == true
            }
            .map(\.id)
    }

    /// Detail text (truncated to maxChars scalars; returns the truncated text
    /// and the total scalar count)
    public func entryText(id: String, maxChars: Int) -> (text: String, totalChars: Int)? {
        guard let text = entries.first(where: { $0.id == id })?.text else { return nil }
        let scalars = text.unicodeScalars
        let total = scalars.count
        if total <= maxChars {
            return (text, total)
        }
        return (String(String.UnicodeScalarView(scalars.prefix(maxChars))), total)
    }

    /// Looks up an entry by ID
    public func entry(id: String) -> HistoryEntry? {
        entries.first(where: { $0.id == id })
    }

    /// Disk bytes the history occupies (index + blobs + sync-files; the blob
    /// figure comes from the runtime counter)
    ///
    /// sync-files is walked directly (only two levels deep): the number of
    /// batches is the same order of magnitude as the entry count, which does
    /// not justify maintaining another counter.
    public func diskUsage() async -> UInt64 {
        let indexPath = dir.appendingPathComponent("index.json")
        let syncRoot = self.syncRoot
        let diskBytes = await runBlocking { () -> UInt64 in
            var total =
                ((try? FileManager.default.attributesOfItem(atPath: indexPath.path))?[.size]
                    as? UInt64) ?? 0
            let batches =
                (try? FileManager.default.contentsOfDirectory(
                    at: syncRoot, includingPropertiesForKeys: [.fileSizeKey])) ?? []
            for batch in batches {
                let files =
                    (try? FileManager.default.contentsOfDirectory(
                        at: batch, includingPropertiesForKeys: [.fileSizeKey])) ?? []
                for file in files {
                    let size = (try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize
                    total += UInt64(max(0, size ?? 0))
                }
            }
            return total
        }
        return diskBytes + blobBytes
    }

    // MARK: - Mutation

    /// Deletes one entry (along with its blob)
    public func delete(id: String) async -> Bool {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return false }
        let entry = entries.remove(at: index)
        await removeBlob(of: entry)
        save()
        return true
    }

    /// Existing entry hit: refresh count, timestamps and origin
    ///
    /// Origin refresh rules are documented on [`record`]; the dedup path and
    /// the compensation path for "recorded concurrently while encoding" share
    /// this implementation.
    private func bump(index: Int, at: UInt64, origin: String?, sourceApp: String?) {
        entries[index].copyCount =
            entries[index].copyCount == .max ? .max : entries[index].copyCount + 1
        entries[index].lastCopiedAt = at
        if origin != nil {
            entries[index].origin = origin
            entries[index].sourceApp = nil
        } else {
            entries[index].origin = nil
            if sourceApp != nil {
                entries[index].sourceApp = sourceApp
            }
        }
        save()
    }

    /// Clears the whole history (pinned entries, every blob and every
    /// sync-files batch included)
    ///
    /// Deleting the entire sync-files root is safe: after a clear no entry
    /// references a batch directory (a batch still being received fails its
    /// write and is rejected — the same class of race as clear against the
    /// image branch; under extreme timing a failed sync beats a leftover
    /// orphan).
    public func clear() async {
        // Bump the generation to invalidate an in-flight record (see the
        // revalidation in its image branch)
        clearGeneration &+= 1
        entries.removeAll()
        let blobsDir = dir.appendingPathComponent("blobs")
        let syncRoot = self.syncRoot
        await runBlocking {
            try? FileManager.default.removeItem(at: blobsDir)
            try? FileManager.default.removeItem(at: syncRoot)
        }
        blobBytes = 0
        previewCache.removeAll()
        save()
    }

    /// Pins or unpins an entry
    public func setPinned(id: String, pinned: Bool) -> Bool {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return false }
        entries[index].pinned = pinned
        save()
        return true
    }

    // MARK: - Images

    /// Reads a blob and decodes it into full-resolution RGBA (restore path,
    /// **must not downsample**)
    public func loadImageRGBA(blobHash: String) async -> (
        width: Int, height: Int, rgba: [UInt8]
    )? {
        let path = blobPath(blobHash)
        return await runBlocking {
            (try? Data(contentsOf: path)).flatMap(decodePNG)
        }
    }

    /// Display PNG for the preview card (long edge ≤800, small images pass
    /// through; cached content-addressed by blob_hash)
    public func previewPNG(blobHash: String) async -> Data? {
        if let index = previewCache.firstIndex(where: { $0.hash == blobHash }) {
            let hit = previewCache.remove(at: index)
            previewCache.insert(hit, at: 0)
            return hit.png
        }
        let path = blobPath(blobHash)
        let png = await runBlocking { () -> Data? in
            (try? Data(contentsOf: path)).flatMap { thumbnailPNG($0, maxPixel: previewMaxPixel) }
        }
        guard let png else { return nil }
        previewCache.insert((blobHash, png), at: 0)
        if previewCache.count > previewCacheCap {
            previewCache.removeLast()
        }
        return png
    }

    // MARK: - Source application icons

    /// Caches a source application icon (skipped when it already exists;
    /// failures are silent)
    public func saveAppIcon(appName: String, png: Data) async {
        let path = appIconPath(appName)
        let iconsDir = dir.appendingPathComponent("appicons")
        await runBlocking {
            guard !FileManager.default.fileExists(atPath: path.path) else { return }
            try? FileManager.default.createDirectory(
                at: iconsDir, withIntermediateDirectories: true)
            try? png.write(to: path)
        }
    }

    /// Reads a source application icon
    public func appIconPNG(appName: String) async -> Data? {
        let path = appIconPath(appName)
        return await runBlocking { try? Data(contentsOf: path) }
    }

    /// Whether this application's icon is already cached
    public func hasAppIcon(appName: String) async -> Bool {
        let path = appIconPath(appName)
        return await runBlocking { FileManager.default.fileExists(atPath: path.path) }
    }

    // MARK: - Persistence

    /// Asynchronous persistence (coalesced by the dirty flag; on failure only
    /// the in-memory state applies)
    private func save() {
        pendingSnapshot.put(entries)
        savePending = true
        if !saving {
            saving = true
            Task { await self.flushLoop() }
        }
    }

    /// Single-writer loop: reset the dirty flag inside the loop before
    /// persisting — changes arriving after the reset queue up for the next
    /// round, while this round already covers everything before it, so there
    /// is no window for loss
    private func flushLoop() async {
        while savePending {
            savePending = false
            await writeLatest()
        }
        saving = false
    }

    /// Persists once synchronously (used by the shutdown flush and by tests;
    /// shares the serialization lock with the async path)
    public func flush() async {
        pendingSnapshot.put(entries)
        savePending = false
        await writeLatest()
    }

    /// Persists once, taking the mailbox's latest snapshot **inside the lock**
    ///
    /// The snapshot must not be taken outside the lock — NSLock is not fair,
    /// so a snapshot taken later can win the lock first, and an older snapshot
    /// landing afterwards erases the newer entries. flushLoop has meanwhile
    /// exited because flush reset the dirty flag, so nothing recovers, and the
    /// last copy before shutdown is lost for good.
    private func writeLatest() async {
        let box = pendingSnapshot
        let dir = self.dir
        let lock = ioLock
        await runBlocking {
            lock.withLock {
                guard let snapshot = box.take() else { return }
                writeIndexSnapshot(snapshot, to: dir)
            }
        }
    }

    // MARK: - Internals

    /// Path of a blob file
    private func blobPath(_ blobHash: String) -> URL {
        dir.appendingPathComponent("blobs").appendingPathComponent("\(blobHash).png")
    }

    /// Path of a cached application icon (key = hashText(app name), derived
    /// the same way as in the Tauri client)
    private func appIconPath(_ appName: String) -> URL {
        dir.appendingPathComponent("appicons").appendingPathComponent(
            "\(hashText(appName)).png")
    }

    /// Removes the on-disk artifacts of an entry and reclaims the usage
    /// counter and cache: images = the blob file (content-addressed 1:1);
    /// files that arrived from a remote = the sync-files batch directory
    /// (**only paths under the sync-files prefix** — a locally copied file
    /// entry references the user's own files and must never be touched)
    private func removeBlob(of entry: HistoryEntry) async {
        if let files = entry.files {
            let rootPrefix = syncRoot.path + "/"
            let batchDirs = Set(
                files.filter { $0.hasPrefix(rootPrefix) }
                    .map { URL(fileURLWithPath: $0).deletingLastPathComponent() })
            if !batchDirs.isEmpty {
                await runBlocking {
                    for dir in batchDirs {
                        try? FileManager.default.removeItem(at: dir)
                    }
                }
            }
        }
        guard let hash = entry.blobHash else { return }
        let path = blobPath(hash)
        let removed = await runBlocking { () -> UInt64 in
            let size = (try? FileManager.default.attributesOfItem(atPath: path.path))?[
                .size] as? UInt64
            guard (try? FileManager.default.removeItem(at: path)) != nil else { return 0 }
            return size ?? 0
        }
        // Saturating: the counter and the disk can drift apart when files are
        // deleted from outside, and it must not underflow
        blobBytes = blobBytes >= removed ? blobBytes - removed : 0
        previewCache.removeAll { $0.hash == hash }
    }
}

extension HistoryEntryMeta {
    /// Projection factory (for the inline map in listMeta)
    fileprivate static func of(_ entry: HistoryEntry) -> HistoryEntryMeta {
        HistoryEntryMeta(of: entry)
    }
}

/// Atomically writes index.json (the caller must already hold the io
/// serialization lock)
///
/// A serialization failure **never writes to disk**: swallowing the error into
/// an atomic overwrite with empty bytes silently wipes the entire history,
/// pinned entries included. Compact encoding: the file is for the program's
/// own use, expand it with jq when investigating.
private func writeIndexSnapshot(_ snapshot: [HistoryEntry], to dir: URL) {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.withoutEscapingSlashes]
    guard let bytes = try? encoder.encode(snapshot) else { return }
    do {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try bytes.write(to: dir.appendingPathComponent("index.json"), options: .atomic)
    } catch {
        // A failed write leaves only the in-memory state (retried on the next
        // change)
    }
}
