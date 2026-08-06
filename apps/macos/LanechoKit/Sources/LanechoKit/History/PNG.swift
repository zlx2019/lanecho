// PNG encode/decode (for history blobs; mirrors encode_png/decode_png in the
// Rust history.rs)
//
// Goes through the ImageIO system framework: the system tunes encoding quality
// and speed, and the output interoperates with what the Rust png crate writes
// (a PNG is a PNG). Pixels are uniformly RGBA8 premultipliedLast on deviceRGB,
// the same convention as the pasteboard read side.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// CGImage → RGBA8 pixels (always redrawn to obtain the bytes; shared by the
/// clipboard read path and PNG decoding)
func rgbaPixels(of image: CGImage) -> (width: Int, height: Int, rgba: [UInt8])? {
    let width = image.width
    let height = image.height
    guard width > 0, height > 0 else { return nil }
    var rgba = [UInt8](repeating: 0, count: width * height * 4)
    let drawn = rgba.withUnsafeMutableBytes { buffer -> Bool in
        guard
            let context = CGContext(
                data: buffer.baseAddress, width: width, height: height,
                bitsPerComponent: 8, bytesPerRow: width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return false }
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return true
    }
    guard drawn else { return nil }
    return (width, height, rgba)
}

/// RGBA8 pixels → CGImage
private func cgImage(width: Int, height: Int, rgba: [UInt8]) -> CGImage? {
    guard rgba.count >= width * height * 4,
        let provider = CGDataProvider(data: Data(rgba) as CFData)
    else { return nil }
    return CGImage(
        width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
        bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
        provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
}

/// Encodes a PNG (nil on failure; callers treat that as skip)
func encodePNG(width: Int, height: Int, rgba: [UInt8]) -> Data? {
    guard let image = cgImage(width: width, height: height, rgba: rgba) else { return nil }
    return encodePNG(image)
}

/// CGImage → PNG bytes
private func encodePNG(_ image: CGImage) -> Data? {
    let data = NSMutableData()
    guard
        let destination = CGImageDestinationCreateWithData(
            data, UTType.png.identifier as CFString, 1, nil)
    else { return nil }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { return nil }
    return data as Data
}

/// Decodes a PNG into full-resolution RGBA (history restore path, **must not
/// downsample**)
func decodePNG(_ bytes: Data) -> (width: Int, height: Int, rgba: [UInt8])? {
    guard
        let source = CGImageSourceCreateWithData(bytes as CFData, nil),
        let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { return nil }
    return rgbaPixels(of: image)
}

/// PNG → display PNG whose long edge is at most maxPixel (preview card
/// downsampling; small images pass through unchanged)
func thumbnailPNG(_ bytes: Data, maxPixel: Int) -> Data? {
    guard let source = CGImageSourceCreateWithData(bytes as CFData, nil) else { return nil }
    // Read the size metadata first (no pixel decoding) and pass small images
    // straight through
    if let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
        let width = props[kCGImagePropertyPixelWidth] as? Int,
        let height = props[kCGImagePropertyPixelHeight] as? Int,
        max(width, height) <= maxPixel
    {
        return bytes
    }
    let options: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceThumbnailMaxPixelSize: maxPixel,
        kCGImageSourceCreateThumbnailWithTransform: true,
    ]
    guard
        let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    else { return nil }
    return encodePNG(thumbnail)
}
