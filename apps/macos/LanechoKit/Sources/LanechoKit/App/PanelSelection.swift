// Selection decision after a list refresh (pure function, easy to unit test)
//
// It lives on its own because the edge cases are easy to get wrong: "nothing
// was selected" and "the selected entry is gone" must be handled differently —
// the old row number can be invalid in either case, and conflating them makes
// a refresh conjure up a highlight out of nowhere (very visible when a remote
// sync lands, since the preview card pops up with it).

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
