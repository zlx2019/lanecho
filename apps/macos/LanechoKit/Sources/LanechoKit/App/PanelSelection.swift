// Where the list highlight goes (pure functions, easy to unit test)
//
// They live on their own because the edge cases are easy to get wrong: "nothing
// was selected" and "the selected entry is gone" must be handled differently —
// the old row number can be invalid in either case, and conflating them makes
// a refresh conjure up a highlight out of nowhere (very visible when a remote
// sync lands, since the preview card pops up with it).

import CoreGraphics
import Foundation

/// Row to select after the refresh; `nil` = keep nothing selected
///
/// - Parameters:
///   - resetSelection: means "go back to the first entry" (panel opened,
///     search text changed)
///   - hadSelection: whether anything was selected before the refresh (nothing
///     is, while the pointer rests on the footer or the search field)
///   - restoredIndex: position of the previously selected entry in the new
///     list (follow it if still present, even when the order changed)
///   - previousRow: row number before the refresh (when the old entry is gone,
///     stay nearby instead of jumping back to the first row)
///   - entryCount: number of entries in the new list
public func rowToSelectAfterRefresh(
    resetSelection: Bool,
    hadSelection: Bool,
    restoredIndex: Int?,
    previousRow: Int,
    entryCount: Int
) -> Int? {
    guard entryCount > 0 else { return nil }
    if resetSelection { return 0 }
    if let restoredIndex, restoredIndex >= 0, restoredIndex < entryCount {
        return restoredIndex
    }
    // Nothing was selected: keep it that way — an invalid row number must not
    // be clamped to 0
    guard hadSelection else { return nil }
    return min(max(previousRow, 0), entryCount - 1)
}

/// Row the pointer sits on after a mouse move; `nil` = the list gives up the
/// highlight
///
/// **Clip to the visible list area before trusting the row number.** The table
/// is a scroll view's document view, so as soon as the content is taller than
/// the visible area its bounds reach down past the footer, and a point on a
/// footer menu row still maps onto a row. Without the clip a row stays lit
/// while the pointer rests on "Settings", which reads as two selections at
/// once.
///
/// - Parameters:
///   - point: pointer position in the panel's coordinate space
///   - listRect: the list area in that same space
///   - rowAtPoint: the row the table reports for this point (-1 = no row,
///     which is what the blank space under a short list gives)
public func hoverRow(point: CGPoint, listRect: CGRect, rowAtPoint: Int) -> Int? {
    guard listRect.contains(point), rowAtPoint >= 0 else { return nil }
    return rowAtPoint
}
