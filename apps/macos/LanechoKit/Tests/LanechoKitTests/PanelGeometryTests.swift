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

/// Whether the pointer ends up over the panel (the defect: sliding the panel
/// back on screen put the pointer on its rows)
private func covers(_ origin: CGPoint, _ pointer: CGPoint) -> Bool {
    CGRect(origin: origin, size: size).insetBy(dx: 1, dy: 1).contains(pointer)
}

/// Bottom-right anchor: flips on both axes — the panel's bottom-right corner
/// sits on the pointer instead of sliding back under it
@Test func panelPlacementBottomRightFlips() {
    let pointer = CGPoint(x: 1900, y: 100)
    let origin = placePanel(anchor: pointer, size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 1900 - 360, y: 100))
    #expect(!covers(origin, pointer))
}

/// Top-right anchor: flips left only, still opening downwards
@Test func panelPlacementTopRightFlipsLeft() {
    let pointer = CGPoint(x: 1800, y: 1000)
    let origin = placePanel(anchor: pointer, size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 1800 - 360, y: 1000 - 440))
    #expect(!covers(origin, pointer))
}

/// Bottom-left anchor: flips up only
@Test func panelPlacementBottomLeftFlipsUp() {
    let pointer = CGPoint(x: 100, y: 50)
    let origin = placePanel(anchor: pointer, size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 100, y: 50))
    #expect(!covers(origin, pointer))
}

/// Pointer up in the menu bar (above the visible frame) at the right: flips
/// left, and the top is held to the visible frame
@Test func panelPlacementMenuBarPointer() {
    let origin = placePanel(anchor: CGPoint(x: 1800, y: 1080), size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 1800 - 360, y: 1055 - 440))
}

/// Resizing in place is a plain clamp: no flipping, the top edge only moves
/// when the grown frame would leave the screen
@Test func panelClampKeepsInPlaceResizes() {
    #expect(
        clampPanel(origin: CGPoint(x: 100, y: 300), size: size, visibleFrame: screen)
            == CGPoint(x: 100, y: 300))
    #expect(
        clampPanel(origin: CGPoint(x: 100, y: -50), size: size, visibleFrame: screen)
            == CGPoint(x: 100, y: 0))
}

/// Top anchor (a menu bar click): y never crosses the top of the visible frame
@Test func panelPlacementTopClamps() {
    let origin = placePanel(anchor: CGPoint(x: 10, y: 1055), size: size, visibleFrame: screen)
    #expect(origin == CGPoint(x: 10, y: 1055 - 440))
}

/// Negative coordinates on a secondary display, pointer near its bottom-left:
/// the panel flips up and stays inside that screen's visible frame
@Test func panelPlacementNegativeSecondaryDisplay() {
    let secondary = CGRect(x: -1440, y: -300, width: 1440, height: 875)
    let origin = placePanel(
        anchor: CGPoint(x: -1430, y: -290), size: size, visibleFrame: secondary)
    #expect(origin.x == -1430)
    #expect(origin.y == -290)
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
