// Floating-window placement (pure functions; mirrors the edge clamping in the
// Tauri client's show_panel: with no clamp, an anchor in a screen corner
// pushes the panel off-screen)
//
// Coordinates are AppKit screen coordinates (origin bottom-left, y up).

import CoreGraphics
import Foundation

/// Panel placement: the anchor (usually the mouse position) is the panel's
/// **top-left** reference and the panel expands downward, then the whole frame
/// is clamped into the screen's visible area on both axes
public func placePanel(anchor: CGPoint, size: CGSize, visibleFrame: CGRect) -> CGPoint {
    var x = anchor.x
    var y = anchor.y - size.height
    // Clamp x and y on both sides: clamping only one edge lets the panel fly
    // off the opposite one
    x = min(max(x, visibleFrame.minX), max(visibleFrame.minX, visibleFrame.maxX - size.width))
    y = min(max(y, visibleFrame.minY), max(visibleFrame.minY, visibleFrame.maxY - size.height))
    return CGPoint(x: x, y: y)
}

/// Preview card placement (same semantics as the Tauri client's
/// place_preview): prefer the right of the panel and flip to the left when it
/// does not fit; align the card's top with the anchor row's top; clamp x and y
/// into the visible area on both sides
public func placePreview(
    panelFrame: CGRect, cardSize: CGSize, anchorTopY: CGFloat,
    visibleFrame: CGRect, gap: CGFloat = 8
) -> CGPoint {
    var x = panelFrame.maxX + gap
    if x + cardSize.width > visibleFrame.maxX {
        x = panelFrame.minX - gap - cardSize.width
    }
    // Fits on neither side (narrow screen): fall back to the left edge of the
    // visible area and allow overlap with the panel
    x = min(max(x, visibleFrame.minX), max(visibleFrame.minX, visibleFrame.maxX - cardSize.width))
    var y = anchorTopY - cardSize.height
    y = min(max(y, visibleFrame.minY), max(visibleFrame.minY, visibleFrame.maxY - cardSize.height))
    return CGPoint(x: x, y: y)
}
