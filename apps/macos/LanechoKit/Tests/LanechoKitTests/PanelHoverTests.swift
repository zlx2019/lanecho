// Regression tests for the history panel's hover row band.
//
// With a mouse plugged in, the system's automatic scroll bar setting draws
// legacy scrollers, which take a column off the right of the list's table. A
// point over that column answers no row, so a pointer crossing it on its way
// out to a preview card on the right gave the highlight up and the card went
// down before the pointer got there. A trackpad-only machine never shows it:
// overlay scrollers float over the table. The scroll view here is pinned to
// the legacy layout so these tests behave the same on any machine, CI
// included.
import AppKit
import XCTest

@testable import Lanecho

/// Scroll view pinned to one scroller style. Setting scrollerStyle is not
/// enough: AppKit writes the system preference back once the content scrolls
private final class PinnedStyleScrollView: NSScrollView {
    var pinnedStyle: NSScroller.Style = .legacy

    override var scrollerStyle: NSScroller.Style {
        get { pinnedStyle }
        set {}
    }
}

@MainActor
private final class Rows: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let count: Int

    init(count: Int) {
        self.count = count
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        count
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        NSTextField(labelWithString: "row \(row)")
    }
}

@MainActor
final class PanelHoverTests: XCTestCase {
    /// A panel-shaped window: 40pt of search area above the list and 100pt of
    /// footer below it, the list set up like the history panel's
    @MainActor
    private struct Fixture {
        let window: NSPanel
        let scrollView: NSScrollView
        let tableView: NSTableView
        /// The table only holds its data source weakly
        let rows: Rows

        /// Window point at the given x, at the vertical middle of a row
        func point(x: CGFloat, row: Int) -> NSPoint {
            let rect = tableView.rect(ofRow: row)
            return NSPoint(
                x: x, y: tableView.convert(NSPoint(x: rect.midX, y: rect.midY), to: nil).y)
        }

        func pointerRow(_ point: NSPoint) -> Int? {
            HistoryPanelViewController.pointerRow(
                atWindowPoint: point, tableView: tableView, scrollView: scrollView)
        }
    }

    private func makeFixture(rowCount: Int) -> Fixture {
        let window = NSPanel(
            contentRect: NSRect(x: 200, y: 200, width: 360, height: 400),
            styleMask: [.nonactivatingPanel, .borderless], backing: .buffered, defer: false)
        let root = NSView(frame: NSRect(x: 0, y: 0, width: 360, height: 400))
        window.contentView = root

        let tableView = NSTableView()
        tableView.addTableColumn(NSTableColumn(identifier: HistoryCellView.identifier))
        tableView.headerView = nil
        tableView.style = .plain
        tableView.rowHeight = 24
        tableView.intercellSpacing = NSSize(width: 0, height: 1)
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        let rows = Rows(count: rowCount)
        tableView.dataSource = rows
        tableView.delegate = rows

        let scrollView = PinnedStyleScrollView()
        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.contentInsets = NSEdgeInsets(top: 5, left: 0, bottom: 5, right: 0)
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(scrollView)
        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: root.topAnchor, constant: 40),
            scrollView.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -100),
            scrollView.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: root.trailingAnchor),
        ])
        tableView.reloadData()
        root.layoutSubtreeIfNeeded()
        return Fixture(window: window, scrollView: scrollView, tableView: tableView, rows: rows)
    }

    /// **Regression guard**: a pointer over the legacy scroller column, at a
    /// row's height, is still on that row. Probe the table at the pointer's x
    /// again and this goes red: the column answers no row, and the highlight
    /// and its card go before the pointer can leave for a card on the right
    func testScrollerColumnStillCountsAsTheRow() {
        let fixture = makeFixture(rowCount: 100)
        let scrollerColumn = fixture.scrollView.contentView.frame.maxX
        XCTAssertLessThan(
            scrollerColumn, fixture.scrollView.frame.width,
            "precondition: the legacy scroller takes a column off the table's right side")

        let overScroller = fixture.point(x: 355, row: 2)
        XCTAssertGreaterThan(overScroller.x, scrollerColumn)
        XCTAssertEqual(
            fixture.tableView.row(at: fixture.tableView.convert(overScroller, from: nil)), -1,
            "precondition: asked at the pointer itself, the table answers no row there")
        XCTAssertEqual(fixture.pointerRow(overScroller), 2)
        XCTAssertEqual(fixture.pointerRow(fixture.point(x: 20, row: 2)), 2)
    }

    /// Off the rows still gives the highlight up: the search area, the footer
    /// (where a long table, running on below the visible area, still answers
    /// with a row) and the blank space under a short list
    func testOffTheRowsGivesUpTheHighlight() {
        let long = makeFixture(rowCount: 100)
        XCTAssertNil(long.pointerRow(NSPoint(x: 180, y: 380)), "search area")
        XCTAssertNil(long.pointerRow(NSPoint(x: 180, y: 50)), "footer")

        let short = makeFixture(rowCount: 3)
        let belowLastRow = short.point(x: 180, row: 2).y - 60
        XCTAssertGreaterThan(belowLastRow, 100, "still inside the list area")
        XCTAssertNil(short.pointerRow(NSPoint(x: 180, y: belowLastRow)), "blank space")
    }
}
