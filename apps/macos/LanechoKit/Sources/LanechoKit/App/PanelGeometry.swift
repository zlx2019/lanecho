// Floating-window placement (pure functions; mirrors the Tauri client's
// place_panel_at_cursor / place_preview: with no clamp, an anchor in a screen
// corner pushes the panel off-screen)
//
// Coordinates are AppKit screen coordinates (origin bottom-left, y up).

import CoreGraphics
import Foundation

/// Panel placement at the pointer, context-menu style, axis by axis: open
/// below and to the right of the anchor, and flip to the other side of the
/// anchor on an axis where that side does not fit. Clamping alone would
/// slide the panel back under the pointer near the right or bottom edge,
/// leaving the pointer over the rows. The clamp still applies afterwards (a
/// pointer up in the menu bar sits above the visible frame, and a screen too
/// small for either side falls back to it)
public func placePanel(anchor: CGPoint, size: CGSize, visibleFrame: CGRect) -> CGPoint {
    var x = anchor.x
    if x + size.width > visibleFrame.maxX, anchor.x - size.width >= visibleFrame.minX {
        x = anchor.x - size.width
    }
    // y is up: "below" puts the panel's top edge on the anchor, "above" its
    // bottom edge
    var y = anchor.y - size.height
    if y < visibleFrame.minY, anchor.y + size.height <= visibleFrame.maxY {
        y = anchor.y
    }
    return clampPanel(origin: CGPoint(x: x, y: y), size: size, visibleFrame: visibleFrame)
}

/// Clamp a panel origin into the visible area on both axes (clamping only one
/// edge lets the panel fly off the opposite one). Also used on its own when
/// the panel resizes in place
public func clampPanel(origin: CGPoint, size: CGSize, visibleFrame: CGRect) -> CGPoint {
    CGPoint(
        x: min(
            max(origin.x, visibleFrame.minX),
            max(visibleFrame.minX, visibleFrame.maxX - size.width)),
        y: min(
            max(origin.y, visibleFrame.minY),
            max(visibleFrame.minY, visibleFrame.maxY - size.height)))
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
