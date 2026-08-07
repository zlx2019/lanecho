// Clipboard content model, mirroring ClipboardContent in the Rust
// clipboard/mod.rs.
//
// The hash is the single key for both echo comparison and history dedup:
// BLAKE3 over a type prefix (t:/i:/f:) plus the content, so the text "a.txt"
// cannot collide with the file list ["a.txt"]. Hashes stay on this machine and
// never go on the wire, but the semantics match the Rust build so design and
// tests port over directly.

import Foundation

/// Clipboard content: the classified read result, most specific first —
/// files > image > text
public enum ClipboardContent: Sendable, Equatable {
    /// Plain text, byte-for-byte as-is (hard rule: never trim, never escape)
    case text(String)
    /// Bitmap (raw RGBA pixels)
    case image(width: Int, height: Int, rgba: [UInt8])
    /// List of file references (path strings; the clipboard itself is the
    /// reference)
    case files([String])
    /// An image is present but its pixels were not read (the skip-the-read
    /// result with "record images" off): an event still has to be emitted to
    /// advance the LWW baseline, and consumers always treat it as no content
    case imageUnread

    /// Content BLAKE3 (hex); the type prefix rules out cross-type collisions
    public func hash() -> String {
        var bytes: [UInt8] = []
        switch self {
        case .text(let text):
            return hashText(text)
        case .image(let width, let height, let rgba):
            bytes.append(contentsOf: Array("i:".utf8))
            withUnsafeBytes(of: UInt64(width).littleEndian) { bytes.append(contentsOf: $0) }
            withUnsafeBytes(of: UInt64(height).littleEndian) { bytes.append(contentsOf: $0) }
            bytes.append(contentsOf: rgba)
        case .files(let paths):
            bytes.append(contentsOf: Array("f:".utf8))
            for path in paths {
                bytes.append(contentsOf: Array(path.utf8))
                bytes.append(0)
            }
        case .imageUnread:
            // Skip-the-read sentinel: it only has to avoid colliding with
            // real content
            bytes.append(contentsOf: Array("i:unread".utf8))
        }
        return Blake3.hex(bytes)
    }

    /// Short type name (for logging)
    public var kind: String {
        switch self {
        case .text: "text"
        case .image, .imageUnread: "image"
        case .files: "files"
        }
    }
}

/// Hash of text content, same format as the text branch of
/// [`ClipboardContent.hash`]: lets echo registration hash without cloning the
/// whole string
public func hashText(_ text: String) -> String {
    var bytes = Array("t:".utf8)
    bytes.append(contentsOf: Array(text.utf8))
    return Blake3.hex(bytes)
}

/// Clipboard change event, produced by the watcher task
public struct ClipboardEvent: Sendable {
    /// The content after the change
    public var content: ClipboardContent
    /// Content hash, precomputed so consumers do not rehash large images
    public var hash: String
    /// When the change was detected (Unix milliseconds); the LWW tiebreaker
    public var timestampMs: UInt64
    /// Pasteboard type snapshot taken at read time (the ignore-rule type
    /// check has to run against what was on the pasteboard then, not at
    /// whatever later moment the event is consumed)
    public var pasteboardTypes: [String]
    /// Ignore verdict: skip the broadcast (the LWW baseline still advances
    /// and localCopied still fires)
    public var suppressBroadcast: Bool
    /// Ignore verdict: skip the history recording (rides through the engine
    /// into localCopied)
    public var suppressRecord: Bool

    /// Field-wise initializer
    public init(
        content: ClipboardContent, hash: String, timestampMs: UInt64,
        pasteboardTypes: [String] = [],
        suppressBroadcast: Bool = false, suppressRecord: Bool = false
    ) {
        self.content = content
        self.hash = hash
        self.timestampMs = timestampMs
        self.pasteboardTypes = pasteboardTypes
        self.suppressBroadcast = suppressBroadcast
        self.suppressRecord = suppressRecord
    }
}

/// Current Unix timestamp in milliseconds
public func nowMs() -> UInt64 {
    UInt64(max(0, Date().timeIntervalSince1970 * 1000))
}
