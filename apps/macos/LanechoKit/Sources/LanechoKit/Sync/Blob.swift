// Sanitizing and landing helpers for blob sync (mirroring the Rust
// sync/blob.rs)
//
// Stream shape: the offer (a JSON frame) is followed by **continuous raw
// bytes** (1 MiB read/write buffer, no per-chunk frame header), terminated by
// a BlobFooter frame carrying the BLAKE3 of the whole stream; the integrity
// check is computed as the bytes flow. The stream IO itself lives in
// Session.swift (it needs a NIO channel); this file holds only pure logic and
// the landing on disk, so unit tests need no networking.

import Foundation

/// Name of the receive landing directory, under the data directory; history
/// entry deletion and the startup sweep recognize it by this prefix.
/// **The user's own original files never live here — cleanup tells the two
/// apart by the prefix**
public let syncFilesDirName = "sync-files"

/// File name sanitizing (security critical: the name comes from the peer and is
/// untrusted)
///
/// Only flat file names are transferred, never a directory tree: take the
/// basename first to strip every path component (a traversal payload such as
/// `../../x` is left as `x`), then sanitize characters that are illegal on
/// other platforms and Windows reserved names.
/// **Never rejects**: an empty or all-dots result falls back to `file-<i>` —
/// one malicious name should not fail the whole batch. Matches the Rust
/// `sanitize_file_name` case for case.
func sanitizeFileName(_ raw: String, fallbackIndex: Int) -> String {
    // basename: split on '/' only (matching the Unix semantics of the Rust
    // Path::file_name; a backslash is not a separator and the character filter
    // below turns it into '_')
    let base = raw.split(separator: "/").last.map(String.init) ?? ""
    // Filter scalar by scalar (the same granularity as the Rust char
    // iteration): control characters and characters illegal on Windows become
    // '_'
    let badScalars = Set("<>:\"|?*/\\".unicodeScalars)
    var cleaned = ""
    for scalar in base.unicodeScalars {
        if scalar.properties.generalCategory == .control || badScalars.contains(scalar) {
            cleaned.append("_")
        } else {
            cleaned.unicodeScalars.append(scalar)
        }
    }
    while let last = cleaned.last, last == " " || last == "." {
        cleaned.removeLast()
    }
    if cleaned.isEmpty || cleaned.allSatisfy({ $0 == "." }) {
        return "file-\(fallbackIndex)"
    }
    // Windows reserved names (CON/PRN/…): sanitized even though this machine is
    // macOS — a network or sync drive can carry the data directory onto a
    // Windows machine
    let stem = cleaned.split(separator: ".").first.map { $0.uppercased() } ?? ""
    let reserved =
        ["CON", "PRN", "AUX", "NUL"].contains(stem)
        || ((stem.hasPrefix("COM") || stem.hasPrefix("LPT"))
            && stem.count == 4
            && ("0"..."9").contains(String(stem.suffix(1))))
    if reserved {
        return "_\(cleaned)"
    }
    return cleaned
}

/// Resolve name collisions within a batch: `a.txt` → `a (1).txt`, counting up
/// (a dotfile is not split into an empty stem)
func uniqueName(used: inout Set<String>, name: String) -> String {
    if used.insert(name).inserted {
        return name
    }
    let stem: String
    let ext: String
    // Find the extension dot from the right; a dotfile like ".bashrc" is all
    // stem
    if let dot = name.lastIndex(of: "."), dot != name.startIndex {
        stem = String(name[..<dot])
        ext = String(name[dot...])
    } else {
        stem = name
        ext = ""
    }
    var counter = 1
    while true {
        let candidate = "\(stem) (\(counter))\(ext)"
        if used.insert(candidate).inserted {
            return candidate
        }
        counter += 1
    }
}

/// Give the files their final names once the checksum passes: `.part` → the
/// sanitized name (collisions within the batch count up)
///
/// Returns the final paths in manifest order, for writing to the clipboard and
/// recording in history.
func finalizeParts(parts: [URL], metas: [FileMeta], batchDir: URL) async -> [URL]? {
    await runBlocking {
        var used = Set<String>()
        var finals: [URL] = []
        finals.reserveCapacity(parts.count)
        for (index, (part, meta)) in zip(parts, metas).enumerated() {
            let name = uniqueName(
                used: &used, name: sanitizeFileName(meta.name, fallbackIndex: index))
            let target = batchDir.appendingPathComponent(name)
            do {
                try FileManager.default.moveItem(at: part, to: target)
            } catch {
                return nil
            }
            finals.append(target)
        }
        return finals
    }
}
