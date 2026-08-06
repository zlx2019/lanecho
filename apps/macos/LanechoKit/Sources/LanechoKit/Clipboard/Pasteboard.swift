// System clipboard read/write, mirroring the arboard wrapper in the Rust
// clipboard/mod.rs plus the sensitive.rs / avail.rs / stamp.rs platform probes.
//
// Threading: NSPasteboard is touched from any thread, the same access model
// arboard uses. Every call goes through runBlocking to leave the cooperative
// thread pool and is wrapped in an autoreleasepool — AppKit on a long-lived
// thread without a pool leaks.

import AppKit
import Foundation

/// Clipboard operation error
public enum ClipboardError: Error, Sendable {
    /// The system refused the write
    case writeFailed
}

/// Concealed marker type (the de facto password-manager convention): on a hit
/// nothing is read at all
private let concealedType = NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType")

/// System clipboard: the synchronous primitives, called from above via
/// runBlocking
enum Pasteboard {
    /// Change stamp (NSPasteboard.changeCount); if it has not moved, the
    /// content cannot have changed
    static func changeCount() -> Int {
        NSPasteboard.general.changeCount
    }

    /// Whether the current content carries the concealed marker
    static func isConcealed() -> Bool {
        NSPasteboard.general.types?.contains(concealedType) ?? false
    }

    /// Whether an image representation exists (cheap probe, reads no pixels)
    static func hasImage() -> Bool {
        let types = NSPasteboard.general.types ?? []
        return types.contains(.tiff) || types.contains(.png)
    }

    /// Read and classify the current content: file lists first as the most
    /// specific, then image, then text
    static func readContent() -> ClipboardContent? {
        let pasteboard = NSPasteboard.general
        if let urls = pasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL],
            !urls.isEmpty
        {
            return .files(urls.map(\.path))
        }
        if let image = readImage(pasteboard) {
            return image
        }
        if let text = pasteboard.string(forType: .string) {
            return .text(text)
        }
        return nil
    }

    /// Read the content and return it together with its hash. BLAKE3 over a
    /// whole image costs real time, so it is computed in the same blocking
    /// context before returning to async instead of on a cooperative thread.
    static func readContentHashed() -> (content: ClipboardContent, hash: String)? {
        readContent().map { ($0, $0.hash()) }
    }

    /// Image representation → RGBA8 pixels
    ///
    /// Bytes always come from a premultipliedLast redraw (rgbaPixels): the hash
    /// only has to be self-consistent. Byte-for-byte agreement with the Rust
    /// build is not a goal here; a divergence only affects dedup counts when
    /// the two clients are used alternately, never data correctness.
    private static func readImage(_ pasteboard: NSPasteboard) -> ClipboardContent? {
        guard
            let data = pasteboard.data(forType: .tiff) ?? pasteboard.data(forType: .png),
            let rep = NSBitmapImageRep(data: data),
            let cgImage = rep.cgImage,
            let (width, height, rgba) = rgbaPixels(of: cgImage)
        else { return nil }
        return .image(width: width, height: height, rgba: rgba)
    }

    /// Write text (remote-sync landing and history-restore paths; the shell
    /// layer performs echo registration first)
    static func writeText(_ text: String) throws {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.setString(text, forType: .string) else {
            throw ClipboardError.writeFailed
        }
    }

    /// Write an image (history-restore path; must keep the original resolution)
    static func writeImage(width: Int, height: Int, rgba: [UInt8]) throws {
        guard
            let rep = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                colorSpaceName: .deviceRGB, bytesPerRow: width * 4, bitsPerPixel: 32),
            let target = rep.bitmapData
        else { throw ClipboardError.writeFailed }
        rgba.withUnsafeBufferPointer { source in
            guard let base = source.baseAddress else { return }
            target.update(from: base, count: min(source.count, width * height * 4))
        }
        guard let tiff = rep.tiffRepresentation else { throw ClipboardError.writeFailed }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.setData(tiff, forType: .tiff) else {
            throw ClipboardError.writeFailed
        }
    }

    /// Write a list of file references (history-restore path)
    static func writeFiles(_ paths: [String]) throws {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        let urls = paths.map { NSURL(fileURLWithPath: $0) }
        guard pasteboard.writeObjects(urls) else {
            throw ClipboardError.writeFailed
        }
    }
}

/// Move blocking AppKit/disk calls off the cooperative thread pool (the
/// spawn_blocking equivalent) and wrap them in an autoreleasepool
func runBlocking<T: Sendable>(_ work: @escaping @Sendable () -> T) async -> T {
    await withCheckedContinuation { continuation in
        DispatchQueue.global(qos: .utility).async {
            continuation.resume(returning: autoreleasepool(invoking: work))
        }
    }
}

// MARK: - Public async API (mirrors the Rust read_content / write_*)

/// Read and classify the current clipboard content
public func readClipboard() async -> ClipboardContent? {
    await runBlocking { Pasteboard.readContent() }
}

/// Write text to the clipboard
public func writeClipboardText(_ text: String) async throws {
    let result = await runBlocking { Result { try Pasteboard.writeText(text) } }
    try result.get()
}

/// Write an image to the clipboard (original resolution)
public func writeClipboardImage(width: Int, height: Int, rgba: [UInt8]) async throws {
    let result = await runBlocking {
        Result { try Pasteboard.writeImage(width: width, height: height, rgba: rgba) }
    }
    try result.get()
}

/// Write file references to the clipboard
public func writeClipboardFiles(_ paths: [String]) async throws {
    let result = await runBlocking { Result { try Pasteboard.writeFiles(paths) } }
    try result.get()
}

/// Frontmost application name, collected as a history entry's source
/// application; mirrors the Rust frontapp.rs.
///
/// Returns nil for our own process: a restore write must not rewrite the
/// source to ourselves.
public func frontmostAppName() async -> String? {
    await runBlocking {
        guard let app = NSWorkspace.shared.frontmostApplication,
            app.processIdentifier != ProcessInfo.processInfo.processIdentifier
        else { return nil }
        return app.localizedName
    }
}

/// Frontmost application icon → 32px PNG (the preview card display size;
/// collected at most once per application per session)
public func frontmostAppIconPNG() async -> Data? {
    await runBlocking {
        guard let app = NSWorkspace.shared.frontmostApplication,
            app.processIdentifier != ProcessInfo.processInfo.processIdentifier,
            let icon = app.icon,
            let rep = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: 32, pixelsHigh: 32,
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
            let context = NSGraphicsContext(bitmapImageRep: rep)
        else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        icon.draw(in: NSRect(x: 0, y: 0, width: 32, height: 32))
        NSGraphicsContext.restoreGraphicsState()
        return rep.representation(using: .png, properties: [:])
    }
}
