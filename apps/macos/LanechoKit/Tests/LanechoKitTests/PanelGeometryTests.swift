// Boundary tests for panel placement, following the same style as the Tauri
// client's place_preview boundary unit tests.

import Foundation
import Testing

@testable import LanechoKit

private let size = CGSize(width: 360, height: 440)
private let screen = CGRect(x: 0, y: 0, width: 1920, height: 1055)

/// Mid-screen: the panel's top-left corner sits on the anchor and it opens
/// downwards
@Test func panelPlacementCenter() {
    let origin = placePanel(anchor: CGPoint(x: 800, y: 600), size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 800, y: 160))
}

/// Bottom-right anchor: both x and y are clamped, nothing overflows
@Test func panelPlacementBottomRightClamps() {
    let origin = placePanel(anchor: CGPoint(x: 1900, y: 100), size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 1920 - 360, y: 0))
}

/// Top anchor (a menu bar click): y never crosses the top of the visible frame
@Test func panelPlacementTopClamps() {
    let origin = placePanel(anchor: CGPoint(x: 10, y: 1055), size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 10, y: 1055 - 440))
}

/// Negative coordinates on a secondary display: the clamp keeps the panel
/// inside that screen's visible frame
@Test func panelPlacementNegativeSecondaryDisplay() {
    let secondary = CGRect(x: -1440, y: -300, width: 1440, height: 875)
    let origin = placePanel(
        anchor: CGPoint(x: -1430, y: -290), size: size, visibleFrame: secondary)
    #expect(origin.x == -1430)
    #expect(origin.y == -300)
    #expect(secondary.contains(CGRect(origin: origin, size: size)))
}

/// Degenerate case where the screen is smaller than the panel: fall back to the
/// visible frame's origin instead of overflowing negatively
@Test func panelPlacementTinyScreen() {
    let tiny = CGRect(x: 0, y: 0, width: 300, height: 400)
    let origin = placePanel(anchor: CGPoint(x: 200, y: 300), size: size, visibleFrame: tiny)
    #expect(origin == CGPoint(x: 0, y: 0))
}

// MARK: - Preview card placement

private let card = CGSize(width: 320, height: 240)

/// Room on the right: the card sits against the panel's right edge with its top
/// aligned to the top of the anchor row
@Test func previewPlacementPrefersRight() {
    let panel = CGRect(x: 400, y: 300, width: 360, height: 480)
    let origin = placePreview(
        panelFrame: panel, cardSize: card, anchorTopY: 700, visibleFrame: screen)
    #expect(origin == CGPoint(x: 768, y: 460))
}

/// No room on the right: flip to the left side
@Test func previewPlacementFlipsLeft() {
    let panel = CGRect(x: 1500, y: 300, width: 360, height: 480)
    let origin = placePreview(
        panelFrame: panel, cardSize: card, anchorTopY: 700, visibleFrame: screen)
    #expect(origin == CGPoint(x: 1500 - 8 - 320, y: 460))
}

/// Anchor near the bottom: y is clamped inside the visible frame
@Test func previewPlacementClampsBottom() {
    let panel = CGRect(x: 400, y: 0, width: 360, height: 480)
    let origin = placePreview(
        panelFrame: panel, cardSize: card, anchorTopY: 100, visibleFrame: screen)
    #expect(origin.y == 0)
}

/// Negative coordinates on a secondary display with no room on either side:
/// fall back to somewhere inside that screen's visible frame
@Test func previewPlacementNegativeNarrowScreen() {
    let narrow = CGRect(x: -500, y: -300, width: 460, height: 800)
    let panel = CGRect(x: -480, y: -200, width: 360, height: 480)
    let origin = placePreview(
        panelFrame: panel, cardSize: card, anchorTopY: 100, visibleFrame: narrow)
    #expect(narrow.contains(CGRect(origin: origin, size: card)))
}
