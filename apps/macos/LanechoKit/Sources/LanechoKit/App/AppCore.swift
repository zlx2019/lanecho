// Application shell core (mirrors the event pump in the Tauri client's
// bridge.rs plus the core command surface of its commands)
//
// Wiring: watcher → engine (clipboardChanged); engine event pump → serialized
// history worker (record includes PNG encoding of large images and is kept off
// the pump's critical path) + clipboard write + UI events.
//
// Three hard contracts, inherited unchanged from the Tauri client:
// - ApplyRemote: the engine registers the echo hash before emitting the event;
//   a failed clipboard write **must** cancelEcho, or the orphan hash swallows
//   the next genuine local copy of that same content
// - Restore write registration (restoreHash): when the pump sees a local copy
//   carrying that hash it does not capture the source application (focus is
//   already back in the target app, and capturing would overwrite the original
//   source); a mismatch voids the registration
// - Settings changes persist first and apply side effects only after the write
//   succeeds, so state cannot end up half-applied

import Foundation

/// Clipboard write port (tests inject a fake and never touch the system
/// clipboard)
public protocol ClipboardPort: Sendable {
    /// Writes text
    func writeText(_ text: String) async throws
    /// Writes an image (full resolution)
    func writeImage(width: Int, height: Int, rgba: [UInt8]) async throws
    /// Writes file references
    func writeFiles(_ paths: [String]) async throws
}

/// System clipboard implementation
public struct SystemClipboard: ClipboardPort {
    /// Creates an instance
    public init() {}

    public func writeText(_ text: String) async throws {
        try await writeClipboardText(text)
    }

    public func writeImage(width: Int, height: Int, rgba: [UInt8]) async throws {
        try await writeClipboardImage(width: width, height: height, rgba: rgba)
    }

    public func writeFiles(_ paths: [String]) async throws {
        try await writeClipboardFiles(paths)
    }
}

/// Shell layer errors (case names match the Tauri client's error codes so the
/// message table can be shared)
public enum CoreError: Error, Sendable, Equatable {
    /// The entry does not exist or its content is missing
    case historyMissing
    /// The file references are stale (source files deleted or moved)
    case filesMissing
    /// Writing the system clipboard failed
    case clipboardWriteFailed
    /// Persisting settings failed (no side effect was applied)
    case settingsSaveFailed
}

/// Events for the UI layer (filtered translation of engine events plus history
/// change notifications)
public enum CoreEvent: Sendable {
    /// A peer came online or was updated
    case peerUp(Peer)
    /// A peer went offline (fingerprint)
    case peerDown(String)
    /// A pairing request arrived (awaiting the user's decision)
    case pairRequested(PeerInfo)
    /// Pairing succeeded
    case paired(PeerInfo)
    /// Pairing was removed (fingerprint)
    case unpaired(String)
    /// The history list changed (the UI should refetch)
    case historyChanged
    /// Remote content landed on the local clipboard (used for notifications)
    case appliedRemote(from: String, preview: String)
    /// Result of one outbound sync
    case syncSent(to: PeerInfo, error: String?)
}

/// History recording job (pump → serialized worker)
private struct HistoryJob: Sendable {
    var content: ClipboardContent
    var hash: String
    var at: UInt64
    var origin: String?
    var sourceApp: String?
}

/// Application shell core
public actor AppCore {
    /// Data directory
    public let dataDir: URL
    /// Sync engine (internal so tests can drive it directly)
    let engine: SyncEngine
    /// Discovery service
    private let discovery: DiscoveryService
    /// History storage
    private let history: HistoryStore
    /// Clipboard watcher (nil when disabled)
    private let watcher: PasteboardWatcher?
    /// Clipboard write port
    private let clipboard: any ClipboardPort
    /// The "record images" toggle (drives whether the watcher skips the read)
    private let readImages: AtomicFlag
    /// Reset channel for the watcher's dedup baseline (set when recording
    /// resumes; see PasteboardWatcher)
    private let resetBaseline: AtomicFlag
    /// Current settings
    private var settings: Settings
    /// Incognito (recording paused; session state, never persisted, and it
    /// covers history only, never sync)
    private var incognito = false
    /// Restore write registration (a local copy matching this hash does not
    /// capture the source application)
    private var restoreHash: String?
    /// Applications whose icon this session already tried to capture
    private var knownIcons: Set<String> = []
    /// History job queue (serialized worker; on overflow the newest job is
    /// dropped, same semantics as Rust's try_send)
    private let historyJobs: AsyncStream<HistoryJob>.Continuation
    /// Outlet for UI events
    private let events: AsyncStream<CoreEvent>.Continuation
    /// Background tasks (discovery bridge and watcher forwarding; simply
    /// cancelled on shutdown)
    private var tasks: [Task<Void, Never>] = []
    /// Event pump task (on shutdown it must be awaited to drain before the
    /// history queue may finish — while draining buffered events the pump
    /// still posts into that queue)
    private var pumpTask: Task<Void, Never>?
    /// History worker task (on shutdown, finish the queue then **await the
    /// drain, never cancel**)
    private var historyWorker: Task<Void, Never>?

    private init(
        dataDir: URL, engine: SyncEngine, discovery: DiscoveryService,
        history: HistoryStore, watcher: PasteboardWatcher?,
        clipboard: any ClipboardPort, readImages: AtomicFlag, resetBaseline: AtomicFlag,
        settings: Settings,
        historyJobs: AsyncStream<HistoryJob>.Continuation,
        events: AsyncStream<CoreEvent>.Continuation
    ) {
        self.dataDir = dataDir
        self.engine = engine
        self.discovery = discovery
        self.history = history
        self.watcher = watcher
        self.clipboard = clipboard
        self.readImages = readImages
        self.resetBaseline = resetBaseline
        self.settings = settings
        self.historyJobs = historyJobs
        self.events = events
    }

    /// Starts the shell: settings, history, engine, discovery and the watcher
    /// come up in order, then the pump and the worker are wired
    ///
    /// - `discoveryPort`/`bonjour`/`watchClipboard`: for test isolation;
    ///   production always uses the defaults
    public static func start(
        dataDir: URL,
        clipboard: any ClipboardPort = SystemClipboard(),
        discoveryPort: UInt16 = Config.discoveryPort,
        bonjour: Bool = true,
        watchClipboard: Bool = true
    ) async throws -> (AppCore, AsyncStream<CoreEvent>) {
        let settings = Settings.load(dataDir: dataDir)
        let history = await HistoryStore.load(dataDir: dataDir)
        // Sweep orphan sync-files batches at startup, without blocking the
        // startup chain
        Task { await history.sweepOrphanSyncFiles() }
        let (engine, engineEvents) = try await SyncEngine.start(
            config: EngineConfig(
                dataDir: dataDir, tcpPort: settings.tcpPort,
                syncMode: settings.engineSyncMode,
                syncTypes: settings.engineSyncTypes,
                maxSyncFileBytes: settings.maxSyncFileBytes))
        // The engine already holds the TCP port — every later failure must
        // shut it down before throwing, otherwise the listener task strongly
        // captures the engine and forms a reference cycle: the actor is never
        // released and the port stays occupied (the next launch then fails to
        // bind, which looks like "port conflict with nothing running")
        let identity: DeviceIdentity
        let discovery: DiscoveryService
        let discoveryStream: AsyncStream<RegistryChange>
        do {
            identity = try DeviceIdentity.loadOrCreate(dir: dataDir)
            (discovery, discoveryStream) = try await DiscoveryService.start(
                identity: identity, tcpPort: await engine.port(),
                discoveryPort: discoveryPort, bonjour: bonjour)
        } catch {
            await engine.shutdown()
            throw error
        }

        // Skip-the-read condition: pixels must be read when **either** "record
        // images" or "sync images" is on; the two collapse into a single flag
        // fed to the watcher, so core stays unaware of the settings structure
        let readImages = AtomicFlag(settings.historyRecordImages || settings.syncImages)
        let (eventStream, eventCont) = AsyncStream.makeStream(
            of: CoreEvent.self, bufferingPolicy: .bufferingNewest(Config.eventChannelCap))
        let (jobStream, jobCont) = AsyncStream.makeStream(
            of: HistoryJob.self, bufferingPolicy: .bufferingOldest(Config.eventChannelCap))

        var watcher: PasteboardWatcher?
        var watchStream: AsyncStream<ClipboardEvent>?
        let resetBaseline = AtomicFlag(false)
        if watchClipboard {
            let (w, s) = PasteboardWatcher.start(
                readImages: readImages, resetBaseline: resetBaseline)
            watcher = w
            watchStream = s
        }

        let core = AppCore(
            dataDir: dataDir, engine: engine, discovery: discovery, history: history,
            watcher: watcher, clipboard: clipboard, readImages: readImages,
            resetBaseline: resetBaseline,
            settings: settings, historyJobs: jobCont, events: eventCont)
        await core.startTasks(
            engineEvents: engineEvents, discoveryStream: discoveryStream,
            watchStream: watchStream, jobStream: jobStream)
        return (core, eventStream)
    }

    /// Wires up the background tasks
    private func startTasks(
        engineEvents: AsyncStream<EngineEvent>,
        discoveryStream: AsyncStream<RegistryChange>,
        watchStream: AsyncStream<ClipboardEvent>?,
        jobStream: AsyncStream<HistoryJob>
    ) {
        tasks.append(engine.attachDiscovery(discoveryStream))
        if let watchStream {
            let engine = engine
            tasks.append(
                Task {
                    for await event in watchStream {
                        await engine.clipboardChanged(event)
                    }
                })
        }
        pumpTask = Task { [weak self] in
            for await event in engineEvents {
                await self?.pump(event)
            }
        }
        // Serialized history worker: record (PNG encoding included) stays off
        // the pump's critical path
        historyWorker = Task { [weak self] in
            for await job in jobStream {
                await self?.runHistoryJob(job)
            }
        }
    }

    /// Engine event pump
    private func pump(_ event: EngineEvent) async {
        switch event {
        case .peerUp(let peer):
            events.yield(.peerUp(peer))
        case .peerDown(let fingerprint):
            events.yield(.peerDown(fingerprint))
        case .pairRequested(let info):
            events.yield(.pairRequested(info))
        case .paired(let info):
            events.yield(.paired(info))
        case .unpaired(let fingerprint):
            events.yield(.unpaired(fingerprint))
        case .localCopied(let content, let hash, let timestampMs):
            guard !incognito else { return }
            // A skip-the-read event only advances the LWW baseline; there is
            // no content to record
            if case .imageUnread = content { return }
            // Restore write: on a hash match do not capture the source
            // application (a registration that does not match is voided too)
            let restored = restoreHash == hash
            restoreHash = nil
            let sourceApp = restored ? nil : await frontmostAppName()
            // Icon captured alongside: at most once per application per
            // session, and when it is already on disk even the extraction is
            // skipped
            if let name = sourceApp, !knownIcons.contains(name) {
                knownIcons.insert(name)
                if await !history.hasAppIcon(appName: name),
                    let png = await frontmostAppIconPNG()
                {
                    await history.saveAppIcon(appName: name, png: png)
                }
            }
            historyJobs.yield(
                HistoryJob(
                    content: content, hash: hash, at: timestampMs,
                    origin: nil, sourceApp: sourceApp))
        case .applyRemote(let content, let from, _, let hash):
            // Notification preview is built per content kind (1.1: first line
            // of text / image dimensions / file names)
            let preview: String
            switch content {
            case .text(let text): preview = previewText(text)
            case .image(let width, let height, _):
                preview = "[\(localeImageWord()) \(width)×\(height)]"
            case .files(let paths): preview = filesPreview(paths)
            case .imageUnread: preview = content.kind
            }
            // Shell layer write: a failed write must cancel the echo
            // registration (contract at the top of this file)
            do {
                switch content {
                case .text(let text):
                    try await clipboard.writeText(text)
                case .image(let width, let height, let rgba):
                    try await clipboard.writeImage(width: width, height: height, rgba: rgba)
                case .files(let paths):
                    try await clipboard.writeFiles(paths)
                case .imageUnread:
                    // The engine never sends this payload; cancel the echo
                    // defensively
                    await engine.cancelEcho(hash: hash)
                    return
                }
            } catch {
                await engine.cancelEcho(hash: hash)
                return
            }
            // Remote writes enter the history too (the engine swallows the
            // echo event, so this is the only entry point)
            if !incognito {
                historyJobs.yield(
                    HistoryJob(
                        content: content, hash: hash, at: nowMs(),
                        origin: from.name, sourceApp: nil))
            }
            events.yield(.appliedRemote(from: from.name, preview: preview))
        case .syncSent(let to, let error):
            events.yield(.syncSent(to: to, error: error))
        }
    }

    /// Body of the history worker
    private func runHistoryJob(_ job: HistoryJob) async {
        let outcome = await history.record(
            content: job.content, contentHash: job.hash, at: job.at,
            origin: job.origin, sourceApp: job.sourceApp,
            config: settings.historyConfig)
        if outcome != .skipped {
            events.yield(.historyChanged)
        }
    }

    /// The word for "image" in a notification preview (same duty as the Tauri
    /// client's locale_image_word; the shell layer owns the language setting,
    /// the engine knows nothing about localization)
    private func localeImageWord() -> String {
        let isZh =
            switch settings.language {
            case "zh": true
            case "en": false
            default: Locale.preferredLanguages.first?.hasPrefix("zh") == true
            }
        return isZh ? "图像" : "Image"
    }
}

/// Notification preview for a file batch: first file name plus a count (same
/// shape as the Tauri client's files_preview)
private func filesPreview(_ paths: [String]) -> String {
    let first = paths.first.map { URL(fileURLWithPath: $0).lastPathComponent } ?? ""
    return paths.count > 1 ? "\(first) +\(paths.count - 1)" : first
}

extension AppCore {

    // MARK: - Settings

    /// Snapshot of the current settings
    public func currentSettings() -> Settings {
        settings
    }

    /// Updates settings: persist first, apply side effects only after the
    /// write succeeds (sync direction policy, type toggles, caps, and the
    /// skip-the-read flag)
    public func updateSettings(_ new: Settings) async throws {
        var new = new
        new.normalize()
        do {
            try new.save(dataDir: dataDir)
        } catch {
            throw CoreError.settingsSaveFailed
        }
        let old = settings
        settings = new
        if old.syncMode != new.syncMode {
            await engine.setSyncMode(new.engineSyncMode)
        }
        if old.engineSyncTypes != new.engineSyncTypes {
            await engine.setSyncTypes(new.engineSyncTypes)
        }
        if old.maxSyncFileMb != new.maxSyncFileMb {
            await engine.setMaxSyncFileBytes(new.maxSyncFileBytes)
        }
        if old.historyRecordImages != new.historyRecordImages
            || old.syncImages != new.syncImages
        {
            readImages.set(new.historyRecordImages || new.syncImages)
        }
        // Recording or the sync pipe resumed (either gate went from off to
        // on): reset the watcher's dedup baseline, so content copied while a
        // gate was closed is recorded and sent normally when copied again
        if recordResumed(old: old, new: new) || syncResumed(old: old, new: new) {
            resetBaseline.set(true)
        }
    }

    /// Incognito (pause recording) toggle; covers the local history only and
    /// does not affect sync
    public func setIncognito(_ on: Bool) {
        incognito = on
        // Leaving incognito means recording resumed: same cause and same
        // treatment as a record type toggle being turned back on
        if !on {
            resetBaseline.set(true)
        }
    }

    /// Incognito state
    public func isIncognito() -> Bool {
        incognito
    }

    // MARK: - History commands

    /// History list projection (ordered per the settings)
    public func historyList() async -> [HistoryEntryMeta] {
        await history.listMeta(sort: settings.historySort)
    }

    /// Searches the history (returns the IDs that hit)
    public func searchHistory(query: String) async -> [String] {
        await history.search(query: query)
    }

    /// Detail text (truncated, plus the total scalar count)
    public func historyEntryText(id: String, maxChars: Int) async -> (
        text: String, totalChars: Int
    )? {
        await history.entryText(id: id, maxChars: maxChars)
    }

    /// ID of the nth entry after sorting (slot hotkeys)
    public func historyEntryIdAt(_ n: Int) async -> String? {
        await history.entryIdAt(sort: settings.historySort, n: n)
    }

    /// Display PNG for an image entry (downsampled; the restore path is
    /// unaffected)
    public func historyImagePNG(id: String) async -> Data? {
        guard let hash = await history.entry(id: id)?.blobHash else { return nil }
        return await history.previewPNG(blobHash: hash)
    }

    /// Source application icon PNG
    public func appIconPNG(appName: String) async -> Data? {
        await history.appIconPNG(appName: appName)
    }

    /// Restores an entry to the system clipboard (equivalent to a copy: it
    /// broadcasts and bumps the history; on success it registers restoreHash
    /// to preserve the original source application)
    public func restoreEntry(id: String) async throws {
        guard let entry = await history.entry(id: id) else { throw CoreError.historyMissing }
        switch entry.kind {
        case HistoryKind.text:
            guard let text = entry.text else { throw CoreError.historyMissing }
            do {
                try await clipboard.writeText(text)
            } catch {
                throw CoreError.clipboardWriteFailed
            }
        case HistoryKind.image:
            guard let hash = entry.blobHash,
                let (width, height, rgba) = await history.loadImageRGBA(blobHash: hash)
            else { throw CoreError.historyMissing }
            do {
                try await clipboard.writeImage(width: width, height: height, rgba: rgba)
            } catch {
                throw CoreError.clipboardWriteFailed
            }
        case HistoryKind.files:
            guard let files = entry.files else { throw CoreError.historyMissing }
            // Lazy validation: an entry whose source files were deleted or
            // moved only reports as stale once it is selected
            guard files.allSatisfy({ FileManager.default.fileExists(atPath: $0) }) else {
                throw CoreError.filesMissing
            }
            do {
                try await clipboard.writeFiles(files)
            } catch {
                throw CoreError.clipboardWriteFailed
            }
        default:
            throw CoreError.historyMissing
        }
        restoreHash = entry.contentHash
    }

    /// Deletes one entry
    public func deleteEntry(id: String) async -> Bool {
        let removed = await history.delete(id: id)
        if removed {
            events.yield(.historyChanged)
        }
        return removed
    }

    /// Clears the history
    public func clearHistory() async {
        await history.clear()
        events.yield(.historyChanged)
    }

    /// Pins or unpins an entry
    public func setPinned(id: String, pinned: Bool) async -> Bool {
        let changed = await history.setPinned(id: id, pinned: pinned)
        if changed {
            events.yield(.historyChanged)
        }
        return changed
    }

    /// Bytes the history occupies
    public func historyDiskUsage() async -> UInt64 {
        await history.diskUsage()
    }

    // MARK: - Device commands

    /// Peers currently online
    public func peers() async -> [Peer] {
        await engine.peers()
    }

    /// List of paired peers
    public func pairedList() async -> [PairedPeer] {
        await engine.pairedList()
    }

    /// Initiates pairing
    public func pair(fingerprint: String) async throws {
        try await engine.pair(fingerprint: fingerprint)
    }

    /// Responds to an inbound pairing request
    public func respondPair(fingerprint: String, accept: Bool) async {
        await engine.respondPair(fingerprint: fingerprint, accept: accept)
    }

    /// Inbound pairing requests awaiting the user's decision (the fallback the
    /// prompt refetches from)
    ///
    /// The pairing prompt is modal and the event pump stops dead while it is
    /// up, so `pairRequested` events arriving in that window get pushed out of
    /// the event stream — the UI layer refetches after each decision, which
    /// recovers the dropped requests. Matches the Tauri client's
    /// `list_pending_pairs`.
    public func pendingPairRequests() async -> [PeerInfo] {
        await engine.pendingPairRequests()
    }

    /// Removes a pairing
    public func unpair(fingerprint: String) async {
        await engine.unpair(fingerprint: fingerprint)
    }

    /// Information about this device
    public func localInfo() async -> PeerInfo {
        await engine.localInfo()
    }

    /// Changes the display name (nil goes back to following the host name):
    /// the engine persists and hot-updates it, then discovery replays it
    public func setDisplayName(_ name: String?) async throws {
        let info = try await engine.setDisplayName(name)
        await discovery.updateInfo(info)
    }

    /// Shuts down (test and exit paths; finishes persisting the history)
    ///
    /// The shutdown order is deliberate, do not rearrange it casually:
    /// - Cancelling a task blocked on `for await` **does not drop buffered
    ///   elements**; before ending, the pump still processes them and posts
    ///   them into the history queue — so `await pumpTask` has to drain first,
    ///   and only then may the queue be `finish()`ed
    /// - The history worker can only be awaited to drain, it **must not be
    ///   cancelled**: cancelling drops the last batch of copies, whose blobs
    ///   are already on disk and get deleted as orphans on the next launch —
    ///   from the user's side, the screenshot just copied vanishes
    public func shutdown() async {
        watcher?.stop()
        // Send goodbye as early as possible so peers drop this device from
        // their lists immediately
        await discovery.shutdown()
        for task in tasks {
            task.cancel()
        }
        // Closing the engine event stream ends the pump's for-await naturally
        await engine.shutdown()
        await pumpTask?.value
        historyJobs.finish()
        await historyWorker?.value
        await history.flush()
        events.finish()
    }
}
