// Clipboard watcher task, mirroring spawn_watcher in the Rust
// clipboard/mod.rs.
//
// Main loop: poll changeCount every 250ms as a fast path (unchanged means the
// content is never touched) → a concealed marker skips the tick entirely (no
// read, dedup state cleared) → with "record images" off, skip the read for
// images (emit only ImageUnread to advance the LWW baseline; it must never
// fall through to the text branch, which would break the two-pipeline
// semantics) → read and classify + dedup by hash → emit the event.
//
// Content already on the clipboard at startup only seeds the baseline and
// emits no event, otherwise every launch would rebroadcast stale content.

import Foundation

/// Cross-task boolean flag: the channel that makes the "record images"
/// setting take effect the moment it changes
public final class AtomicFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Bool

    /// Construct with an initial value
    public init(_ initial: Bool) {
        value = initial
    }

    /// Current value
    public func get() -> Bool {
        lock.withLock { value }
    }

    /// Update the value
    public func set(_ newValue: Bool) {
        lock.withLock { value = newValue }
    }

    /// Take and reset (one-shot signal semantics; used by the watcher's
    /// dedup-baseline reset channel)
    public func take() -> Bool {
        lock.withLock {
            defer { value = false }
            return value
        }
    }
}

/// Handle to the clipboard watcher task
public final class PasteboardWatcher: Sendable {
    /// The watch loop task
    private let task: Task<Void, Never>

    private init(task: Task<Void, Never>) {
        self.task = task
    }

    /// Stop watching; the event stream finishes with it
    public func stop() {
        task.cancel()
    }

    /// Start watching: returns the handle and the change event stream. The
    /// task exits on its own once the consumer stops consuming.
    ///
    /// `resetBaseline`: the dedup-baseline reset channel. The shell layer sets
    /// it when recording resumes (a type toggle switched back on, or incognito
    /// left) and the next tick clears lastHash. While recording is paused the
    /// baseline still remembers what was copied even though the history layer
    /// skipped it; without the reset, copying that same content again after
    /// resuming would be swallowed by dedup forever — until some other content
    /// displaces the baseline.
    public static func start(
        readImages: AtomicFlag,
        resetBaseline: AtomicFlag
    ) -> (PasteboardWatcher, AsyncStream<ClipboardEvent>) {
        let (stream, continuation) = AsyncStream.makeStream(
            of: ClipboardEvent.self,
            bufferingPolicy: .bufferingNewest(Config.eventChannelCap))
        let task = Task {
            // Baseline: current stamp + current content hash, under the same
            // rule as the main loop — concealed and skip-the-read never read
            var lastStamp = await runBlocking { Pasteboard.changeCount() }
            var lastHash = await baselineHash(readImages: readImages.get())

            while !Task.isCancelled {
                try? await Task.sleep(for: Config.watchInterval)
                if Task.isCancelled { break }
                if resetBaseline.take() {
                    lastHash = nil
                }
                // Fast path: the change stamp has not moved, skip without
                // touching the clipboard content
                let stamp = await runBlocking { Pasteboard.changeCount() }
                if stamp == lastStamp { continue }
                lastStamp = stamp
                // Concealed marker: skip the whole tick, the content is never
                // read (not broadcast, never recorded in history)
                if await runBlocking({ Pasteboard.isConcealed() }) {
                    lastHash = nil
                    continue
                }
                // Skip the read: with "record images" off, read no pixels and
                // emit a contentless event to advance the LWW baseline. The
                // probe must come first — letting the image branch fall
                // through to text is not an option
                if !readImages.get(), await runBlocking({ Pasteboard.hasImage() }) {
                    // The sentinel hash stands for no real content; keeping
                    // it would swallow later changes
                    lastHash = nil
                    let content = ClipboardContent.imageUnread
                    continuation.yield(
                        ClipboardEvent(
                            content: content, hash: content.hash(), timestampMs: nowMs()))
                    continue
                }
                guard let (content, hash) = await runBlocking({ Pasteboard.readContentHashed() })
                else { continue }
                // Stamp moved but the content did not (e.g. only the format
                // representation changed): dedup
                if lastHash == hash { continue }
                lastHash = hash
                continuation.yield(
                    ClipboardEvent(content: content, hash: hash, timestampMs: nowMs()))
            }
            continuation.finish()
        }
        continuation.onTermination = { _ in task.cancel() }
        return (PasteboardWatcher(task: task), stream)
    }
}

/// Content hash for the startup baseline; a failed read counts as no baseline
private func baselineHash(readImages: Bool) async -> String? {
    await runBlocking {
        if Pasteboard.isConcealed() { return nil }
        if !readImages, Pasteboard.hasImage() { return nil }
        return Pasteboard.readContentHashed()?.hash
    }
}
