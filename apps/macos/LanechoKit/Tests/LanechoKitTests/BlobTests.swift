// Unit tests for blob name sanitization and collision resolution, mirroring
// the Rust sync/blob.rs tests case for case — both ends must produce the same
// safe file names from the same hostile manifest.

import Foundation
import Testing

@testable import LanechoKit

/// Every path-traversal payload collapses to a safe basename
@Test func sanitizeBlocksTraversal() {
    #expect(sanitizeFileName("../../etc/passwd", fallbackIndex: 0) == "passwd")
    #expect(sanitizeFileName("/etc/shadow", fallbackIndex: 0) == "shadow")
    // A backslash is not a path separator on Unix, so basename does not split
    // on it — but once it is replaced with _, the whole string is just a
    // harmless ordinary file name: no separator, and not ".."
    #expect(sanitizeFileName("..\\..\\win.ini", fallbackIndex: 0) == ".._.._win.ini")
    #expect(sanitizeFileName("..", fallbackIndex: 0) == "file-0")
    #expect(sanitizeFileName("", fallbackIndex: 3) == "file-3")
    #expect(sanitizeFileName("...", fallbackIndex: 5) == "file-5")
}

/// Windows-illegal characters are replaced, trailing dots and spaces are
/// stripped, and reserved names get a prefix
@Test func sanitizeWindowsRules() {
    #expect(sanitizeFileName("a<b>c:d.txt", fallbackIndex: 0) == "a_b_c_d.txt")
    #expect(sanitizeFileName("name. ", fallbackIndex: 0) == "name")
    #expect(sanitizeFileName("CON.txt", fallbackIndex: 0) == "_CON.txt")
    #expect(sanitizeFileName("COM1", fallbackIndex: 0) == "_COM1")
    #expect(sanitizeFileName("COMMON.txt", fallbackIndex: 0) == "COMMON.txt")
    #expect(sanitizeFileName("Résumé file name.tar.gz", fallbackIndex: 0) == "Résumé file name.tar.gz")
    // Control characters are always replaced, including the non-ASCII C1 range
    #expect(sanitizeFileName("a\u{07}b\u{85}c", fallbackIndex: 0) == "a_b_c")
}

/// Collisions inside one batch get an incrementing suffix; a dotfile does not
/// split into an empty stem
@Test func uniqueNamesIncrement() {
    var used = Set<String>()
    #expect(uniqueName(used: &used, name: "a.txt") == "a.txt")
    #expect(uniqueName(used: &used, name: "a.txt") == "a (1).txt")
    #expect(uniqueName(used: &used, name: "a.txt") == "a (2).txt")
    #expect(uniqueName(used: &used, name: ".bashrc") == ".bashrc")
    #expect(uniqueName(used: &used, name: ".bashrc") == ".bashrc (1)")
}

/// PNG encode/decode round-trip: opaque RGBA survives byte-for-byte, which is
/// what the echo hash baseline rests on. ImageIO's premultiplied alpha rewrites
/// translucent pixels, so synced images are agreed to be fully opaque.
@Test func pngRoundtripPreservesOpaqueRGBA() throws {
    let (width, height) = (3, 2)
    var rgba = [UInt8]()
    for pixel in 0..<width * height {
        rgba.append(contentsOf: [
            UInt8(pixel * 7 % 251), UInt8(pixel * 11 % 241), UInt8(pixel * 13 % 239), 0xFF,
        ])
    }
    let png = try #require(encodePNG(width: width, height: height, rgba: rgba))
    let decoded = try #require(decodePNG(png))
    #expect(decoded.width == width)
    #expect(decoded.height == height)
    #expect(decoded.rgba == rgba)
}
