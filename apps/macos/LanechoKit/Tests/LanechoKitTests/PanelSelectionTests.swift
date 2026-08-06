// Boundaries of the selection decision made after the list refreshes, and of
// the hover decision made on every mouse move

import CoreGraphics
import Testing

@testable import LanechoKit

/// An empty list never selects anything
@Test func refreshOnEmptyListSelectsNothing() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: true, hadSelection: true, restoredIndex: nil, previousRow: 3,
            entryCount: 0) == nil)
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: true, restoredIndex: 2, previousRow: 2,
            entryCount: 0) == nil)
}

/// Reset semantics (opening the panel, changing the search term) go back to the
/// first row
@Test func refreshWithResetGoesToFirstRow() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: true, hadSelection: false, restoredIndex: nil, previousRow: -1,
            entryCount: 5) == 0)
}

/// The previous selection still exists: follow it by id to its new position,
/// even when the order changed
@Test func refreshFollowsRestoredEntry() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: true, restoredIndex: 4, previousRow: 1,
            entryCount: 9) == 4)
}

/// The previous selection was deleted: stay on the same row number, or step
/// back one when it was the last row
@Test func refreshFallsBackToPreviousRow() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: true, restoredIndex: nil, previousRow: 2,
            entryCount: 9) == 2)
    // Was on the last row; once the list shrinks it clamps to the new last row
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: true, restoredIndex: nil, previousRow: 8,
            entryCount: 3) == 2)
}

/// **Regression guard**: if nothing was selected before the refresh (the
/// pointer is resting on the footer or the search field), nothing must be
/// selected after it.
///
/// Writing the row number as `min(max(previousRow ?? 0, 0), count - 1)` clamps
/// the -1 that means "no selection" to 0 — a highlight then appears out of
/// nowhere the moment a remote sync arrives, and the preview card pops up with
/// it. Change this function back to that clamp and this assertion must go red.
@Test func refreshKeepsEmptySelectionEmpty() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: false, restoredIndex: nil, previousRow: -1,
            entryCount: 6) == nil)
}

/// The list area used by the hover tests: the panel is 360 wide, the search
/// area takes the top and the footer menu the bottom
private let listRect = CGRect(x: 0, y: 100, width: 360, height: 300)

/// A move inside the list follows the row the table reports
@Test func hoverInsideListTakesTheRow() {
    #expect(hoverRow(point: CGPoint(x: 180, y: 250), listRect: listRect, rowAtPoint: 7) == 7)
}

/// Blank space under a short list reports no row, and the highlight goes
@Test func hoverOnBlankSpaceGivesUpTheHighlight() {
    #expect(hoverRow(point: CGPoint(x: 180, y: 120), listRect: listRect, rowAtPoint: -1) == nil)
}

/// **Regression guard**: a point on a footer menu row gives up the highlight
/// even while the table still answers with a row number.
///
/// Once the list scrolls, the table view — the scroll view's document view —
/// is taller than the visible area and its bounds reach down past the footer,
/// so `row(at:)` happily maps a point on "Settings" onto a row. Drop the
/// listRect clip from hoverRow and this assertion must go red: the panel would
/// light a history row and a footer menu row at the same time, which is what
/// the user sees as two selections.
@Test func hoverOnFooterGivesUpTheHighlight() {
    // y = 40 is below the list; the table still claims row 15
    #expect(hoverRow(point: CGPoint(x: 180, y: 40), listRect: listRect, rowAtPoint: 15) == nil)
    // Same story above the list, where the search field is
    #expect(hoverRow(point: CGPoint(x: 180, y: 430), listRect: listRect, rowAtPoint: 0) == nil)
}

/// An out-of-range restoredIndex is not trusted; the row-number fallback takes
/// over
@Test func refreshIgnoresOutOfRangeRestoredIndex() {
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: true, restoredIndex: 12, previousRow: 1,
            entryCount: 4) == 1)
    #expect(
        rowToSelectAfterRefresh(
            resetSelection: false, hadSelection: false, restoredIndex: 12, previousRow: -1,
            entryCount: 4) == nil)
}
