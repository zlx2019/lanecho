// History entry model (mirrors HistoryEntry in the Tauri client's history.rs;
// index.json must stay compatible in both directions — the two clients read
// and write the same file)
//
// serde contract: camelCase keys, Option fields omitted when None, and missing
// fields take their default on decode (backward compatible). On the Swift
// side: property names are the keys (camelCase already), the synthesized
// encode uses encodeIfPresent for Optionals (matching omission semantics), and
// decoding goes through a custom init with decodeIfPresent plus a default for
// every field.

import Foundation

/// Content kind constants (same values as the Rust kind module)
public enum HistoryKind {
    public static let text = "text"
    public static let image = "image"
    public static let files = "files"
}

/// One history entry (serves as both the stored record and the DTO)
public struct HistoryEntry: Codable, Sendable, Equatable {
    /// Stable ID (UUID, lowercased to match the Rust uuid format)
    public var id: String
    /// Content kind (see [`HistoryKind`])
    public var kind: String
    /// Text content (inlined when kind=text, byte-for-byte as copied)
    public var text: String?
    /// Image blob hash (points at blobs/<hash>.png when kind=image)
    public var blobHash: String?
    /// File path references (kind=files)
    public var files: [String]?
    /// Summary shown in the list (truncated first line of text / image
    /// dimensions / file names)
    public var preview: String
    /// Content hash (dedup key, from the same source as ClipboardContent.hash)
    public var contentHash: String
    /// First copied at (Unix milliseconds)
    public var firstCopiedAt: UInt64
    /// Last copied at (Unix milliseconds)
    public var lastCopiedAt: UInt64
    /// Copy count (bumped when the same content is copied again)
    public var copyCount: UInt32
    /// Origin: nil = copied locally, a value = written by a remote sync
    /// (device name)
    public var origin: String?
    /// Source application (frontmost application name at local copy time; nil
    /// for remote entries and when the lookup fails)
    public var sourceApp: String?
    /// Pinned to the top (never evicted)
    public var pinned: Bool

    /// Field-by-field init (same defaults as the Rust Default)
    public init(
        id: String = "", kind: String = HistoryKind.text,
        text: String? = nil, blobHash: String? = nil, files: [String]? = nil,
        preview: String = "", contentHash: String = "",
        firstCopiedAt: UInt64 = 0, lastCopiedAt: UInt64 = 0, copyCount: UInt32 = 1,
        origin: String? = nil, sourceApp: String? = nil, pinned: Bool = false
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.blobHash = blobHash
        self.files = files
        self.preview = preview
        self.contentHash = contentHash
        self.firstCopiedAt = firstCopiedAt
        self.lastCopiedAt = lastCopiedAt
        self.copyCount = copyCount
        self.origin = origin
        self.sourceApp = sourceApp
        self.pinned = pinned
    }

    /// Lenient decoding: any missing field takes its default (serde default
    /// semantics)
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? HistoryKind.text
        text = try container.decodeIfPresent(String.self, forKey: .text)
        blobHash = try container.decodeIfPresent(String.self, forKey: .blobHash)
        files = try container.decodeIfPresent([String].self, forKey: .files)
        preview = try container.decodeIfPresent(String.self, forKey: .preview) ?? ""
        contentHash = try container.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
        firstCopiedAt = try container.decodeIfPresent(UInt64.self, forKey: .firstCopiedAt) ?? 0
        lastCopiedAt = try container.decodeIfPresent(UInt64.self, forKey: .lastCopiedAt) ?? 0
        copyCount = try container.decodeIfPresent(UInt32.self, forKey: .copyCount) ?? 1
        origin = try container.decodeIfPresent(String.self, forKey: .origin)
        sourceApp = try container.decodeIfPresent(String.self, forKey: .sourceApp)
        pinned = try container.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
    }
}

/// List projection DTO (carries no inlined full text; used by the panel list,
/// mirrors the Rust HistoryEntryMeta)
public struct HistoryEntryMeta: Sendable, Equatable {
    public var id: String
    public var kind: String
    public var files: [String]?
    public var preview: String
    public var firstCopiedAt: UInt64
    public var lastCopiedAt: UInt64
    public var copyCount: UInt32
    public var origin: String?
    public var sourceApp: String?
    public var pinned: Bool
    /// Text length in bytes (kind=text; the full text is not shipped with the
    /// list, this is what conveys its size)
    public var textLen: Int

    /// Projects a stored entry (the full text is reduced to a byte count)
    init(of entry: HistoryEntry) {
        id = entry.id
        kind = entry.kind
        files = entry.files
        preview = entry.preview
        firstCopiedAt = entry.firstCopiedAt
        lastCopiedAt = entry.lastCopiedAt
        copyCount = entry.copyCount
        origin = entry.origin
        sourceApp = entry.sourceApp
        pinned = entry.pinned
        textLen = entry.text?.utf8.count ?? 0
    }
}

/// List ordering (pinned entries always on top; sort = "frequent" orders by
/// copy count, anything else by recency). Ties fall back to the original index
/// for a stable order — the slot order and the list order must agree
func sortedEntries(_ entries: [HistoryEntry], sort: String) -> [HistoryEntry] {
    entries.enumerated()
        .sorted { lhs, rhs in
            let a = lhs.element
            let b = rhs.element
            if a.pinned != b.pinned {
                return a.pinned
            }
            if sort == "frequent" {
                if a.copyCount != b.copyCount {
                    return a.copyCount > b.copyCount
                }
            }
            if a.lastCopiedAt != b.lastCopiedAt {
                return a.lastCopiedAt > b.lastCopiedAt
            }
            return lhs.offset < rhs.offset
        }
        .map(\.element)
}

/// Text preview: the first line truncated to 80 scalars (display only; the
/// storage layer keeps the text byte-for-byte). The ellipsis is decided by
/// comparing byte lengths, matching Rust's preview.len() < text.len()
///
/// The line break scan MUST run on unicode scalars, not Characters: "\r\n" is
/// a single grapheme cluster, so a Character-level firstIndex(of: "\n") never
/// matches in CRLF text (exactly what Windows peers sync over) and the whole
/// multi-line prefix would leak into the preview
public func previewText(_ text: String) -> String {
    let scalars = text.unicodeScalars
    let lineEnd = scalars.firstIndex(where: CharacterSet.newlines.contains) ?? scalars.endIndex
    let preview = String(String.UnicodeScalarView(scalars[..<lineEnd].prefix(80)))
    return preview.utf8.count < text.utf8.count ? preview + "…" : preview
}

/// File preview: the file name list truncated to 80 scalars, with the file
/// count appended when truncated
func previewFiles(_ paths: [String]) -> String {
    let names = paths.compactMap { path -> String? in
        let name = URL(fileURLWithPath: path).lastPathComponent
        return name == "/" ? nil : name
    }
    let joined = names.joined(separator: ", ")
    let preview = String(String.UnicodeScalarView(joined.unicodeScalars.prefix(80)))
    if preview.unicodeScalars.count < joined.unicodeScalars.count {
        return "\(preview)… (\(paths.count))"
    }
    return preview
}
