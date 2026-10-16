// History panel content, built from plain AppKit controls: NSVisualEffectView
// vibrancy plus NSSearchField, NSTableView, NSSwitch and NSMenu.
//
// The interaction rules, and why they have to be native:
// - The search field always holds focus and typing filters; ↑↓/Enter/Esc are
//   forwarded to the list through doCommandBy, AppKit's standard mechanism for
//   "focus is in a text field, the keyboard drives the list"
// - Decoupled highlighting: hover goes through NSTrackingArea's .mouseMoved,
//   which answers physical mouse movement only, so a row sliding under a
//   stationary cursor as keyboard navigation scrolls the list does **not**
//   trigger it. SwiftUI's onHover is enter/exit semantics and cannot manage
//   this; it fights keyboard navigation over the highlight. Scrolling with
//   the user's own wheel or trackpad is the exception and re-runs the hover
//   decision (listDidLiveScroll). The area covers the whole panel: on the
//   table alone the moves stop dead at its edge, and the list never gets to
//   give up the highlight on its way to the footer
// - A highlight the pointer put there leaves with the pointer, whether it
//   moves off the rows or out of the panel — except on its way to the preview
//   card, which gets a grace period to cross the gap (mouseExited)
// - The list refreshes through reloadData with the selection restored by entry
//   id, with no view-identity diff (SwiftUI's ForEach(id:) layered with
//   .id(index) is a dual-identity conflict that scrambles row content)
// - The clear confirmation is an in-panel overlay: a system alert takes focus,
//   and hide on blur would then close the panel

import AppKit
import LanechoKit

/// History panel view controller
@MainActor
final class HistoryPanelViewController: NSViewController {
    /// Panel width
    static let panelWidth: CGFloat = 360
    /// Initial content size; the real height hugs the entry count, see
    /// preferredHeight
    static let contentSize = NSSize(width: panelWidth, height: 420)

    /// Row height plus row spacing, used to compute the list height
    private static let rowStride: CGFloat = 25
    /// Search area height: the field plus the padding above and below
    private static let headerHeight: CGFloat = 37
    /// Vertical padding of the list area
    private static let listPadding: CGFloat = 10
    /// Minimum and maximum list height; an empty list still has to fit its
    /// placeholder, and anything over the maximum scrolls
    private static let listHeightRange: ClosedRange<CGFloat> = 90...420
    /// Content width of the in-panel alert; title and buttons share it
    private static let overlayContentWidth: CGFloat = 208
    /// How long the pointer may take from leaving the panel to landing on the
    /// card before the highlight it left with is given up: ample for the 8pt
    /// gap, and short enough not to linger after an exit that misses the card
    private static let cardReachGrace: Duration = .milliseconds(200)
    /// How long the pointer may be off the card, once it has been on it,
    /// before the highlight goes: just enough to cross back to the panel
    /// without the card flickering. 300ms read as the card lagging behind a
    /// pointer that had already left
    private static let cardLeaveGrace: Duration = .milliseconds(100)
    /// Pointer sampling interval while it is out of the panel on the card's
    /// side, which adds up to one interval on top of either grace
    private static let exitWatchInterval: Duration = .milliseconds(30)

    private let core: AppCore
    /// Hide-panel callback, for Esc and a completed restore
    var onHide: (() -> Void)?
    /// Open-preferences callback
    var onOpenSettings: (() -> Void)?
    /// Open-about-window callback
    var onShowAbout: (() -> Void)?
    /// Content height change callback; the panel hugs its content like a menu,
    /// so a change in entries or filter results changes the window height
    var onContentHeightChanged: ((CGFloat) -> Void)?
    /// Footer height, measured after assembly and used for the total height
    private var footerHeight: CGFloat = 100

    /// Search input: a hand-drawn magnifier plus a borderless field. Strip the
    /// bezel off an NSSearchField and its search button overlaps the text, and
    /// a raised input box has no business in a menu anyway
    private let searchField = NSTextField()
    private let searchIcon = NSImageView()
    private let tableView = NSTableView()
    private let scrollView = NSScrollView()
    private let emptyLabel = NSTextField(labelWithString: "")
    private let contextMenu = NSMenu()
    private let confirmOverlay = NSView()
    /// Alert title
    private let overlayTitle = NSTextField(labelWithString: "")
    /// Supporting alert text, used by the clear confirmation only and hidden
    /// for error messages
    private let overlayBody = NSTextField(wrappingLabelWithString: "")
    /// Primary button (Clear / OK)
    private let overlayPrimary = NSButton()
    /// Secondary button (Cancel), hidden for error messages
    private let overlaySecondary = NSButton()
    /// Current alert kind, which decides the button count and where Enter and
    /// Esc go
    private var overlayMode: OverlayMode = .clearConfirm
    /// The alert card, kept around to measure: when the panel is shorter than
    /// the card it has to stretch, or the rounded mask clips the card
    private let overlayCard = NSVisualEffectView()
    /// Footer menu rows, whose hover is suppressed together while an alert is
    /// up
    private var footerRows: [MenuItemRow] = []
    /// The search query at the moment the alert appeared, used to undo
    /// anything typed into the field while it is up
    private var queryBeforeOverlay = ""

    /// The two shapes of the in-panel alert
    private enum OverlayMode {
        /// Clear confirmation: a destructive primary action plus cancel
        case clearConfirm
        /// Error message: a single OK
        case alert
    }

    /// Full projection from the most recent fetch
    private var all: [HistoryEntryMeta] = []
    /// The filtered list on display
    private var entries: [HistoryEntryMeta] = []
    /// The clear confirmation layer is up
    private var confirming = false
    /// Refresh generation: concurrent refreshes can have their awaits return
    /// out of order, so stale results are discarded — old search results
    /// landing on top of newer ones render the list as scrambled data
    private var refreshGeneration = 0
    /// Floating preview card
    private let previewCard = PreviewCardController()
    /// Preview card delay task, reset whenever the selection changes, which
    /// doubles as the coalescing window while sweeping
    private var previewTimer: Task<Void, Never>?
    /// Preview card delay in milliseconds, re-read from settings on every
    /// panel open
    private var previewDelayMs: UInt32 = 150
    /// Whether the pointer put the current highlight there, as opposed to the
    /// keyboard, a fresh open or a search setting it as the keyboard's
    /// starting point; only a pointer-made highlight leaves with the pointer
    private var hoverOwnsHighlight = false
    /// Pointer watch while it is out of the panel on its way to the card, see
    /// watchPointerOnTheWayToCard
    private var exitWatch: Task<Void, Never>?
    /// Whether direct slot pastes are on, which decides if the slot hints
    /// appear on the right of a row
    private var slotHotkeysEnabled = true
    /// Modifier symbol shown in the slot hints (follows the slotModifier
    /// setting, re-read on every panel open)
    private var slotModSymbol = slotModifierSymbol("CmdOrCtrl")
    /// Auto-paste on selection, re-read from settings on every panel open
    private var autoPasteEnabled = false

    init(core: AppCore) {
        self.core = core
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    // MARK: - View assembly

    override func loadView() {
        // Root: the system menu material, the same look as Maccy and
        // Easydict, with corners rounded through maskImage — a layer
        // cornerRadius cannot clip behind-window sampling, and maskImage is
        // the supported way
        let effect = NSVisualEffectView()
        effect.material = .menu
        effect.blendingMode = .behindWindow
        effect.state = .active
        effect.maskImage = Self.roundedMask(radius: 10)
        effect.frame = NSRect(origin: .zero, size: Self.contentSize)
        view = effect
        // Hover tracking spans the **whole panel**, not just the list. On the
        // table view the events stop the instant the pointer leaves it, so
        // moving from a row down to a footer menu row never delivered the move
        // that gives up the highlight — a row stayed lit next to the
        // highlighted menu row, reading as two selections. Enter and exit
        // cover leaving the panel sideways, see mouseExited
        effect.addTrackingArea(
            NSTrackingArea(
                rect: .zero,
                options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                owner: self, userInfo: nil))
        // No preferredContentSize: the window height comes from
        // preferredHeight hugging the content, and setting it makes AppKit
        // keep resetting the window back to it

        searchField.placeholderString = L10n.t.searchPlaceholder
        searchField.focusRingType = .none
        searchField.font = .systemFont(ofSize: 13)
        searchField.isBezeled = false
        searchField.isBordered = false
        searchField.drawsBackground = false
        searchField.usesSingleLineMode = true
        searchField.lineBreakMode = .byTruncatingTail
        searchField.delegate = self
        searchIcon.image = NSImage(
            systemSymbolName: "magnifyingglass", accessibilityDescription: nil)
        searchIcon.contentTintColor = .secondaryLabelColor
        searchIcon.symbolConfiguration = .init(pointSize: 12, weight: .regular)

        buildTable()
        let footer = buildFooter()
        footerHeight = footer.fittingSize.height
        buildOverlay()

        // The hairline border a menu is known for; maskImage rounds the
        // corners without stroking them, so overlay a view that ignores events
        let border = BorderOverlayView(radius: 10)
        border.translatesAutoresizingMaskIntoConstraints = false

        emptyLabel.font = .systemFont(ofSize: 13)
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center

        let header = NSStackView(views: [searchIcon, searchField])
        header.orientation = .horizontal
        header.spacing = 6
        header.alignment = .centerY
        header.edgeInsets = NSEdgeInsets(top: 0, left: 11, bottom: 0, right: 11)

        let searchSeparator = Self.separator()
        let footerSeparator = Self.separator()
        for sub in [header, searchSeparator, scrollView, emptyLabel, footerSeparator, footer, confirmOverlay] {
            sub.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(sub)
        }
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 9),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: 19),

            searchSeparator.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 8),
            searchSeparator.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            searchSeparator.trailingAnchor.constraint(equalTo: view.trailingAnchor),

            scrollView.topAnchor.constraint(equalTo: searchSeparator.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: footerSeparator.topAnchor),

            emptyLabel.centerXAnchor.constraint(equalTo: scrollView.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: scrollView.centerYAnchor),

            footerSeparator.bottomAnchor.constraint(equalTo: footer.topAnchor),
            footerSeparator.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footerSeparator.trailingAnchor.constraint(equalTo: view.trailingAnchor),

            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),

            confirmOverlay.topAnchor.constraint(equalTo: view.topAnchor),
            confirmOverlay.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            confirmOverlay.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            confirmOverlay.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])

        view.addSubview(border)
        NSLayoutConstraint.activate([
            border.topAnchor.constraint(equalTo: view.topAnchor),
            border.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            border.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            border.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])
    }

    /// Total panel height that hugs the content, menu style: as tall as the
    /// entries need, scrolling only past the cap
    func preferredHeight() -> CGFloat {
        let rows = CGFloat(max(entries.count, 1))
        let list = min(
            max(rows * Self.rowStride + Self.listPadding, Self.listHeightRange.lowerBound),
            Self.listHeightRange.upperBound)
        // 1pt for each of the two separators
        let base = Self.headerHeight + 1 + list + 1 + footerHeight
        // With little history the panel can end up shorter than the alert,
        // which would let the rounded mask clip the card, so stretch to the
        // card while an alert is up. The card's height comes from its content
        // and it carries no height constraint of its own, so measuring is safe
        guard confirming else { return base }
        return max(base, overlayCard.fittingSize.height + 16)
    }

    /// The list: single column, view-based, transparent background, native
    /// selection
    private func buildTable() {
        let column = NSTableColumn(identifier: HistoryCellView.identifier)
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.style = .plain
        tableView.rowHeight = 24
        tableView.intercellSpacing = NSSize(width: 0, height: 1)
        tableView.backgroundColor = .clear
        tableView.focusRingType = .none
        tableView.allowsMultipleSelection = false
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.action = #selector(rowClicked)
        contextMenu.delegate = self
        tableView.menu = contextMenu

        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.drawsBackground = false
        scrollView.contentInsets = NSEdgeInsets(top: 5, left: 0, bottom: 5, right: 0)
        NotificationCenter.default.addObserver(
            self, selector: #selector(listDidLiveScroll(_:)),
            name: NSScrollView.didLiveScrollNotification, object: scrollView)
    }

    /// Footer menu area: the sync toggle plus three menu rows, with menu-style
    /// highlighting and shortcut hints
    private func buildFooter() -> NSView {
        let footer = NSView()

        // Every shortcut hinted here is really implemented in FloatingPanel;
        // never advertise one that is not
        let clearRow = MenuItemRow(title: L10n.t.clearHistory, shortcut: "⌥⌘⌫") {
            [weak self] in
            self?.presentClearConfirm()
        }
        let settingsRow = MenuItemRow(title: L10n.t.settings, shortcut: "⌘,") { [weak self] in
            self?.onOpenSettings?()
        }
        let aboutRow = MenuItemRow(title: L10n.t.about, shortcut: "") { [weak self] in
            self?.onShowAbout?()
        }
        let quitRow = MenuItemRow(title: L10n.t.quit, shortcut: "⌘Q") {
            NSApp.terminate(nil)
        }

        footerRows = [clearRow, settingsRow, aboutRow, quitRow]

        let stack = NSStackView(views: [clearRow, settingsRow, aboutRow, quitRow])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 1
        stack.edgeInsets = NSEdgeInsets(top: 5, left: 0, bottom: 6, right: 0)
        stack.translatesAutoresizingMaskIntoConstraints = false
        footer.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: footer.topAnchor),
            stack.bottomAnchor.constraint(equalTo: footer.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            clearRow.widthAnchor.constraint(equalTo: stack.widthAnchor),
            settingsRow.widthAnchor.constraint(equalTo: stack.widthAnchor),
            aboutRow.widthAnchor.constraint(equalTo: stack.widthAnchor),
            quitRow.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        return footer
    }

    /// Entry point for the clear shortcut (⌥⌘⌫), forwarded from the panel's
    /// key-equivalent chain
    func requestClearFromShortcut() {
        presentClearConfirm()
    }

    /// Entry point for the open-settings shortcut (⌘,)
    func requestSettingsFromShortcut() {
        onOpenSettings?()
    }

    /// Clear confirmation overlay: a dimming layer plus a card. The material
    /// blends within the window, so a layer corner radius can clip it
    private func buildOverlay() {
        confirmOverlay.wantsLayer = true
        confirmOverlay.layer?.backgroundColor =
            NSColor.black.withAlphaComponent(0.35).cgColor
        confirmOverlay.isHidden = true

        let card = overlayCard
        card.material = .popover
        card.blendingMode = .withinWindow
        card.state = .active
        card.wantsLayer = true
        card.layer?.cornerRadius = 12
        card.layer?.masksToBounds = true

        // The app icon, how a system alert always opens; when it cannot be
        // loaded, take up no space at all
        let icon = NSImageView()
        icon.image = Assets.appIcon
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.isHidden = Assets.appIcon == nil

        overlayTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        overlayTitle.alignment = .center
        overlayTitle.lineBreakMode = .byWordWrapping
        overlayTitle.usesSingleLineMode = false
        overlayTitle.maximumNumberOfLines = 3
        overlayTitle.preferredMaxLayoutWidth = Self.overlayContentWidth

        overlayBody.font = .systemFont(ofSize: 11)
        overlayBody.textColor = .secondaryLabelColor
        overlayBody.alignment = .center
        overlayBody.preferredMaxLayoutWidth = Self.overlayContentWidth

        Self.styleOverlayButton(
            overlayPrimary, target: self, action: #selector(overlayPrimaryClicked))
        Self.styleOverlayButton(
            overlaySecondary, target: self, action: #selector(overlaySecondaryClicked))

        let stack = NSStackView(views: [
            icon, overlayTitle, overlayBody, overlayPrimary, overlaySecondary,
        ])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 10
        stack.setCustomSpacing(14, after: icon)
        stack.setCustomSpacing(6, after: overlayBody)
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 18, bottom: 18, right: 18)
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        card.translatesAutoresizingMaskIntoConstraints = false
        confirmOverlay.addSubview(card)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            icon.widthAnchor.constraint(equalToConstant: 56),
            icon.heightAnchor.constraint(equalToConstant: 56),
            overlayTitle.widthAnchor.constraint(equalToConstant: Self.overlayContentWidth),
            overlayBody.widthAnchor.constraint(equalToConstant: Self.overlayContentWidth),
            // Buttons stack vertically and fill the card's content width
            // equally, the narrow layout of a system warning alert
            overlayPrimary.widthAnchor.constraint(equalToConstant: Self.overlayContentWidth),
            overlaySecondary.widthAnchor.constraint(equalToConstant: Self.overlayContentWidth),
            card.centerXAnchor.constraint(equalTo: confirmOverlay.centerXAnchor),
            card.centerYAnchor.constraint(equalTo: confirmOverlay.centerYAnchor),
        ])
    }

    /// Shared styling for alert buttons: large rounded buttons, stacked and
    /// equal width
    private static func styleOverlayButton(
        _ button: NSButton, target: AnyObject, action: Selector
    ) {
        button.bezelStyle = .rounded
        button.controlSize = .large
        button.target = target
        button.action = action
    }

    /// Set a button title; destructive actions go red — a .rounded button
    /// ignores contentTintColor, so only an attributed title can color it
    private static func setOverlayButtonTitle(
        _ button: NSButton, _ title: String, destructive: Bool
    ) {
        guard destructive else {
            button.attributedTitle = NSAttributedString(string: "")
            button.title = title
            return
        }
        let style = NSMutableParagraphStyle()
        style.alignment = .center
        button.attributedTitle = NSAttributedString(
            string: title,
            attributes: [
                .foregroundColor: NSColor.systemRed,
                .font: NSFont.systemFont(ofSize: 13),
                .paragraphStyle: style,
            ])
    }

    /// 1px separator
    private static func separator() -> NSView {
        let line = NSBox()
        line.boxType = .separator
        return line
    }

    /// Rounded mask image, stretched nine-part through capInsets
    private static func roundedMask(radius: CGFloat) -> NSImage {
        let side = radius * 2 + 1
        let image = NSImage(
            size: NSSize(width: side, height: side), flipped: false
        ) { rect in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
            return true
        }
        image.capInsets = NSEdgeInsets(top: radius, left: radius, bottom: radius, right: radius)
        image.resizingMode = .stretch
        return image
    }

    // MARK: - Lifecycle (the panel stays alive, just hidden)

    /// On every open: clear the search, scroll back to the top, re-fetch the
    /// list and re-read the settings it reflects
    func prepareForShow() {
        dismissOverlay()
        searchField.stringValue = ""
        handHighlightToKeyboard()
        cancelPreview()
        Task {
            let settings = await core.currentSettings()
            previewDelayMs = settings.previewDelayMs
            autoPasteEnabled = settings.autoPaste
            let symbol = slotModifierSymbol(settings.slotModifier)
            if slotHotkeysEnabled != settings.slotHotkeys || slotModSymbol != symbol {
                slotHotkeysEnabled = settings.slotHotkeys
                slotModSymbol = symbol
                tableView.reloadData()
            }
        }
        Task { await refresh(fetchAll: true, resetSelection: true) }
    }

    /// Runs with the panel hiding, on every path that hides it: stop the timer
    /// and the pointer watch, and take the card down
    func panelDidHide() {
        stopExitWatch()
        cancelPreview()
    }

    /// Focus the search field; the window controller calls this after opening
    func focusSearch() {
        view.window?.makeFirstResponder(searchField)
    }

    /// Refresh driven by a history change event, keeping the selected entry so
    /// the user is not interrupted
    func refreshFromEvent() {
        Task { await refresh(fetchAll: true, resetSelection: false) }
    }

    /// ⌥P: pin or unpin the highlighted entry
    func togglePinSelected() {
        guard !confirming else { return }
        let row = tableView.selectedRow
        guard entries.indices.contains(row) else { return }
        let entry = entries[row]
        Task { _ = await core.setPinned(id: entry.id, pinned: !entry.pinned) }
    }

    /// ⌘⌫: delete the highlighted entry; the list refresh comes back through
    /// the historyChanged event
    func deleteSelected() {
        guard !confirming else { return }
        let row = tableView.selectedRow
        guard entries.indices.contains(row) else { return }
        let id = entries[row].id
        Task { _ = await core.deleteEntry(id: id) }
    }

    // MARK: - Filtering and refresh

    /// The single refresh entry point, shared by the open, event and search
    /// paths
    ///
    /// - `fetchAll`: whether to re-fetch the full projection; the search path
    ///   only re-filters
    /// - Generation guard: check the generation after each await and discard
    ///   a stale result wholesale
    private func refresh(fetchAll: Bool, resetSelection: Bool) async {
        refreshGeneration += 1
        let generation = refreshGeneration
        if fetchAll {
            let fetched = await core.historyList()
            guard generation == refreshGeneration else { return }
            all = fetched
        }
        let query = searchField.stringValue
        let filtered: [HistoryEntryMeta]
        if query.isEmpty {
            filtered = all
        } else {
            let ids = Set(await core.searchHistory(query: query))
            guard generation == refreshGeneration else { return }
            filtered = all.filter { ids.contains($0.id) }
        }
        // Take both the old selected id and its row before the table changes:
        // restore by id first, which survives a reordering, and fall back to
        // the old row number when the id is gone because it was just deleted —
        // otherwise the selection snaps back to the first entry
        let previous = resetSelection ? nil : selectedEntryId()
        let previousRow = tableView.selectedRow
        let hadSelection = previousRow >= 0
        if resetSelection {
            // The first row is the keyboard's starting point, not a pointer
            // pick
            handHighlightToKeyboard()
        }
        entries = filtered
        tableView.reloadData()
        // Menu-style hugging: a change in entry count changes the window
        // height, so the panel shrinks as a search narrows the results
        onContentHeightChanged?(preferredHeight())
        emptyLabel.isHidden = !entries.isEmpty
        emptyLabel.stringValue =
            query.isEmpty ? L10n.t.emptyHistory : L10n.t.emptySearch
        // The old selection follows its id first, which survives a
        // reordering; the remaining edge cases live in rowToSelectAfterRefresh
        let restored = previous.flatMap { id in entries.firstIndex(where: { $0.id == id }) }
        if let row = rowToSelectAfterRefresh(
            resetSelection: resetSelection, hadSelection: hadSelection,
            restoredIndex: restored, previousRow: previousRow, entryCount: entries.count)
        {
            tableView.selectRowIndexes([row], byExtendingSelection: false)
            // Only scroll when the selection did not follow an id: id
            // following mostly happens on refreshes triggered by a remote
            // sync, and scrolling then drags the user away from what they
            // are looking at, which decoupled highlighting forbids
            if restored == nil {
                tableView.scrollRowToVisible(row)
            }
        } else {
            // Nothing selected now (no results, or nothing was selected
            // before). reloadData drops the selection without posting
            // selectionDidChange, so a card still up for the old highlight
            // would otherwise outlive it beside the emptied list
            cancelPreview()
        }
    }

    /// Id of the currently selected entry
    private func selectedEntryId() -> String? {
        let row = tableView.selectedRow
        guard entries.indices.contains(row) else { return nil }
        return entries[row].id
    }

    // MARK: - Actions

    /// A single click on a row restores it
    @objc private func rowClicked() {
        let row = tableView.clickedRow
        guard entries.indices.contains(row) else { return }
        restore(id: entries[row].id)
    }

    /// Restore the highlighted entry (Enter)
    private func restoreSelected() {
        let row = tableView.selectedRow
        guard entries.indices.contains(row) else { return }
        restore(id: entries[row].id)
    }

    /// Restore the entry and hide the panel; a failure raises the in-panel
    /// alert
    ///
    /// With auto-paste on, **hide the panel before sending the key**: the
    /// panel is a non-activating window, so the frontmost app never changed,
    /// but keyboard focus is on the panel and an early ⌘V gets eaten by it
    private func restore(id: String) {
        Task {
            do {
                try await core.restoreEntry(id: id)
                onHide?()
                if autoPasteEnabled {
                    AutoPaste.paste()
                }
            } catch {
                await presentFailure(error, entryId: id)
            }
        }
    }

    /// Move the highlight from the keyboard; only this path scrolls
    private func moveSelection(_ delta: Int) {
        guard !entries.isEmpty else { return }
        handHighlightToKeyboard()
        let current = tableView.selectedRow
        let next = min(max(0, current < 0 ? 0 : current + delta), entries.count - 1)
        tableView.selectRowIndexes([next], byExtendingSelection: false)
        tableView.scrollRowToVisible(next)
    }

    /// ⇧↑ / ⇧↓ and ⌘↑ / ⌘↓: jump the highlight straight to the first or last
    /// entry
    ///
    /// It cannot go through moveSelection with a large delta: with nothing
    /// selected — which is where the pointer resting on the footer leaves the
    /// list — that clamps to the first row either way, so ⇧↓ would jump the
    /// wrong way
    private func jumpSelection(toFirst: Bool) {
        guard !entries.isEmpty else { return }
        handHighlightToKeyboard()
        let row = toFirst ? 0 : entries.count - 1
        tableView.selectRowIndexes([row], byExtendingSelection: false)
        tableView.scrollRowToVisible(row)
    }

    /// Raise the clear confirmation layer
    private func presentClearConfirm() {
        overlayMode = .clearConfirm
        overlayTitle.stringValue = L10n.t.confirmClearTitle
        overlayBody.stringValue = L10n.t.confirmClearBody
        overlayBody.isHidden = false
        Self.setOverlayButtonTitle(overlayPrimary, L10n.t.clearConfirm, destructive: true)
        Self.setOverlayButtonTitle(overlaySecondary, L10n.t.cancel, destructive: false)
        overlaySecondary.isHidden = false
        setOverlayVisible(true)
    }

    /// Raise an error message with a single button
    ///
    /// **NSAlert is not an option**: a native modal takes focus and the
    /// panel's hide on blur closes it; on top of that, not one MainActor task
    /// runs while a modal is up, the same trap the pair request window hits
    private func presentAlert(_ title: String) {
        overlayMode = .alert
        overlayTitle.stringValue = title
        overlayBody.stringValue = ""
        overlayBody.isHidden = true
        Self.setOverlayButtonTitle(overlayPrimary, L10n.t.alertOK, destructive: false)
        overlaySecondary.isHidden = true
        setOverlayVisible(true)
    }

    /// Primary button: the clear confirmation clears, an error message only
    /// dismisses
    @objc private func overlayPrimaryClicked() {
        let mode = overlayMode
        dismissOverlay()
        if case .clearConfirm = mode {
            // Clearing is terminal and leaves nothing in the list to look at,
            // so hide the panel at once and let the write continue in the
            // background; clearHistory is an actor call and does not hold the
            // main thread
            onHide?()
            Task { await core.clearHistory() }
        }
    }

    /// Secondary button (Cancel), shared with Esc
    @objc private func overlaySecondaryClicked() {
        dismissOverlay()
    }

    private func dismissOverlay() {
        guard confirming else { return }
        setOverlayVisible(false)
    }

    /// The single entry point for showing and hiding the alert: the dimming
    /// layer, hover suppression underneath and the panel height must move
    /// together
    private func setOverlayVisible(_ visible: Bool) {
        confirming = visible
        confirmOverlay.isHidden = !visible
        // The alert only blocks clicks, not tracking areas, so the footer
        // menu rows have to suppress hover explicitly
        for row in footerRows {
            row.hoverSuppressed = visible
        }
        if visible {
            queryBeforeOverlay = searchField.stringValue
        }
        onContentHeightChanged?(preferredHeight())
    }

    /// An action failed: tell the user, and drop the record if it is already
    /// dead
    ///
    /// An entry whose source files moved away or whose content is gone would
    /// only be clicked and fail again and again; one that fails every time is
    /// not worth keeping
    private func presentFailure(_ error: Error, entryId: String?) async {
        let alert = L10n.t.panelAlert(error)
        if alert.discardsEntry, let entryId {
            _ = await core.deleteEntry(id: entryId)
            await refresh(fetchAll: true, resetSelection: false)
        }
        presentAlert(alert.title)
    }

    /// Context menu action: pin or unpin
    @objc private func togglePinClicked(_ sender: NSMenuItem) {
        guard let payload = sender.representedObject as? (String, Bool) else { return }
        Task { _ = await core.setPinned(id: payload.0, pinned: payload.1) }
    }

    /// Context menu action: delete
    @objc private func deleteClicked(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        Task { _ = await core.deleteEntry(id: id) }
    }

    // MARK: - Hover (decoupled highlighting: the selection follows the pointer
    // only when the pointer, or the user's own scrolling, moves it; hover
    // never scrolls)

    override func mouseMoved(with event: NSEvent) {
        guard !confirming else { return }
        updateHover(windowPoint: event.locationInWindow)
    }

    /// The hover decision, shared by pointer moves and user scrolling: the row
    /// the pointer is on takes the highlight, anywhere off the rows gives it
    /// up
    private func updateHover(windowPoint: NSPoint) {
        let row = Self.pointerRow(
            atWindowPoint: windowPoint, tableView: tableView, scrollView: scrollView)
        guard let row else {
            // The pointer is off the rows (the search area, the footer, the
            // blank space under a short list), so the list has to give up the
            // highlight; otherwise one row in the list and one footer menu row
            // light up at once, which reads as two selections
            releaseHighlight()
            return
        }
        hoverOwnsHighlight = true
        guard row != tableView.selectedRow else { return }
        tableView.selectRowIndexes([row], byExtendingSelection: false)
    }

    /// Row the pointer is on, from a point in window coordinates; nil = off
    /// the rows
    ///
    /// The row is decided by the pointer's height alone, asking the table at
    /// its horizontal midline. With a mouse plugged in, the system's automatic
    /// scroll bar setting draws legacy scrollers, which take a 15pt column off
    /// the table's right side, and a point over that column answers no row at
    /// all — the move crossing it on the way out to a card on the right gave
    /// the highlight up, and the card went down before the pointer could get
    /// there. The point also goes through hoverRow in list space, whose clip
    /// keeps a scrolled list from answering for points that are really on the
    /// footer
    static func pointerRow(
        atWindowPoint point: NSPoint, tableView: NSTableView, scrollView: NSScrollView
    ) -> Int? {
        let inTable = tableView.convert(point, from: nil)
        return hoverRow(
            point: scrollView.convert(point, from: nil),
            listRect: scrollView.bounds,
            rowAtPoint: tableView.row(at: NSPoint(x: tableView.bounds.midX, y: inTable.y)))
    }

    /// The pointer left the rows: give up the highlight, and the card goes
    /// with it through selectionDidChange
    private func releaseHighlight() {
        hoverOwnsHighlight = false
        stopExitWatch()
        if tableView.selectedRow >= 0 {
            tableView.deselectAll(nil)
        }
    }

    /// The keyboard, a fresh open or a search set the highlight: the pointer
    /// leaving no longer takes it away
    private func handHighlightToKeyboard() {
        hoverOwnsHighlight = false
        stopExitWatch()
    }

    /// The pointer left the panel. Leaving through the top or bottom already
    /// gave the highlight up on the way, over the search field or the footer,
    /// but a sideways exit leaves no move inside the panel to do that — so a
    /// highlight the pointer put there goes now, unless the pointer is headed
    /// for the card, which sits across a gap: giving up at the edge would take
    /// the card down before the pointer could ever reach it
    override func mouseExited(with event: NSEvent) {
        guard hoverOwnsHighlight, !confirming, let window = view.window, window.isVisible
        else { return }
        let pointer = window.convertPoint(toScreen: event.locationInWindow)
        // Still over the panel: another window, the row context menu, took
        // the pointer without it leaving
        guard !window.frame.insetBy(dx: 1, dy: 1).contains(pointer) else { return }
        if let card = previewCard.shownFrame,
            exitHeadsForCard(pointer: pointer, panelFrame: window.frame, cardFrame: card)
        {
            watchPointerOnTheWayToCard()
        } else {
            releaseHighlight()
        }
    }

    /// Back in the panel: the next move decides the highlight again
    override func mouseEntered(with event: NSEvent) {
        stopExitWatch()
    }

    /// Keep the highlight, and its card, while the pointer crosses over to the
    /// card or rests on it; give it up once the pointer has been on neither
    /// the panel nor the card for longer than the grace — cardReachGrace
    /// until it first lands on the card, the shorter cardLeaveGrace after that
    ///
    /// It samples the pointer instead of waiting for tracking events: a card
    /// whose content fits passes the mouse through, so it never sees the
    /// pointer arrive or leave
    private func watchPointerOnTheWayToCard() {
        stopExitWatch()
        exitWatch = Task { [weak self] in
            var lastOnTarget = ContinuousClock.now
            var reachedCard = false
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.exitWatchInterval)
                guard let self, !Task.isCancelled, let window = view.window else { return }
                let pointer = NSEvent.mouseLocation
                if window.frame.contains(pointer) {
                    // Back over the panel; mouseEntered normally gets here
                    // first
                    exitWatch = nil
                    return
                }
                let grace = reachedCard ? Self.cardLeaveGrace : Self.cardReachGrace
                if previewCard.shownFrame?.contains(pointer) == true {
                    lastOnTarget = .now
                    reachedCard = true
                } else if ContinuousClock.now - lastOnTarget >= grace {
                    releaseHighlight()
                    return
                }
            }
        }
    }

    private func stopExitWatch() {
        exitWatch?.cancel()
        exitWatch = nil
    }

    /// The user scrolled the list (wheel, trackpad, momentum or a scroller
    /// drag), sliding new rows under a pointer that did not move: the
    /// highlight follows the row under it now, as a menu's does, and a card
    /// placed against the old row position comes down to be rescheduled. Left
    /// alone, a card due mid-scroll would anchor to wherever its row had
    /// scrolled off to and get clamped to the screen edge, beside a list with
    /// no visible highlight
    ///
    /// Keyboard navigation's scrollRowToVisible is programmatic and posts no
    /// live-scroll notification (verified, as was a wheel scroll posting one
    /// per animation step), so this never fights the keyboard over the
    /// highlight
    @objc private func listDidLiveScroll(_ notification: Notification) {
        guard !confirming, let window = view.window else { return }
        let selected = tableView.selectedRow
        updateHover(windowPoint: window.convertPoint(fromScreen: NSEvent.mouseLocation))
        // A changed selection has already rescheduled through
        // selectionDidChange
        if tableView.selectedRow == selected {
            schedulePreview()
        }
    }

    // MARK: - Floating preview card

    /// Reschedule the preview card after a selection change; the delay is
    /// previewDelayMs and doubles as the coalescing window while sweeping, so
    /// a quick pass only pops the card where the pointer settles
    private func schedulePreview() {
        cancelPreview()
        let row = tableView.selectedRow
        guard !confirming, entries.indices.contains(row) else { return }
        let entry = entries[row]
        let delay = previewDelayMs
        previewTimer = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(UInt64(delay)))
            guard !Task.isCancelled else { return }
            await self?.presentPreview(for: entry, row: row)
        }
    }

    /// Stop the timer and take the card down
    private func cancelPreview() {
        previewTimer?.cancel()
        previewTimer = nil
        previewCard.hide()
    }

    /// Fetch the content and show the preview card; give up if the selection
    /// changed or the panel closed while fetching
    private func presentPreview(for meta: HistoryEntryMeta, row: Int) async {
        let content: PreviewContent
        switch meta.kind {
        case HistoryKind.image:
            guard let data = await core.historyImagePNG(id: meta.id),
                let image = NSImage(data: data)
            else { return }
            content = .image(image)
        case HistoryKind.files:
            content = .files(meta.files ?? [])
        default:
            guard
                let text = await core.historyEntryText(
                    id: meta.id, maxChars: PreviewCardController.textLimit)
            else { return }
            content = .text(text.text, totalChars: text.totalChars)
        }
        var appIcon: NSImage?
        if meta.origin == nil, let app = meta.sourceApp,
            let data = await core.appIconPNG(appName: app)
        {
            appIcon = NSImage(data: data)
        }
        // The fetch was async, so re-check the situation before committing.
        // **The entry id has to be part of that check**: a new copy or a
        // remote sync arriving mid-fetch refreshes the list, and the row
        // number stays the same while a different entry now sits there.
        // Comparing row numbers alone shows old content on the card while the
        // highlight points at the new entry. A cancelled presentation must not
        // land either: a newer selection, a user scroll or the panel closing
        // took over while it fetched. And the row has to be on screen, or the
        // anchor is wherever the row scrolled off to and the clamp drags the
        // card to the screen edge
        guard !Task.isCancelled, tableView.selectedRow == row, entries.indices.contains(row),
            entries[row].id == meta.id, rowIsVisible(row),
            let window = view.window, window.isVisible
        else { return }
        let rowRect = tableView.convert(tableView.rect(ofRow: row), to: nil)
        let anchorTopY = window.convertToScreen(rowRect).maxY
        previewCard.show(
            content: content, meta: meta, appIcon: appIcon,
            panelFrame: window.frame, anchorTopY: anchorTopY, screen: window.screen)
    }

    /// Whether any part of the row is inside the list's visible area
    private func rowIsVisible(_ row: Int) -> Bool {
        scrollView.documentVisibleRect.intersects(tableView.rect(ofRow: row))
    }
}

// MARK: - Table data source and delegate

extension HistoryPanelViewController: NSTableViewDataSource, NSTableViewDelegate {
    func numberOfRows(in tableView: NSTableView) -> Int {
        entries.count
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        let cell =
            tableView.makeView(withIdentifier: HistoryCellView.identifier, owner: self)
            as? HistoryCellView ?? HistoryCellView(frame: .zero)
        cell.identifier = HistoryCellView.identifier
        // Slot hints go on the first 6 rows only, matching the slot hotkeys
        // actually registered
        let hint = slotHotkeysEnabled && row < 6 ? "\(slotModSymbol)\(row + 1)" : ""
        cell.configure(with: entries[row], slotHint: hint)
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        PanelRowView()
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        schedulePreview()
    }
}

// MARK: - Search field delegate (typing filters, keys are forwarded)

extension HistoryPanelViewController: NSTextFieldDelegate {
    func controlTextDidChange(_ obj: Notification) {
        // The search field keeps keyboard focus while an alert is up, since
        // Enter and Esc are forwarded through it, but anything typed has to be
        // undone: behind the alert the user cannot see themselves typing, and
        // the guard below stops the list refreshing — dismiss the alert and
        // the query no longer matches the list contents
        guard !confirming else {
            if searchField.stringValue != queryBeforeOverlay {
                searchField.stringValue = queryBeforeOverlay
            }
            return
        }
        Task { await refresh(fetchAll: false, resetSelection: true) }
    }

    /// Keyboard forwarding while focus is in the text field, AppKit's standard
    /// mechanism
    func control(
        _ control: NSControl, textView: NSTextView, doCommandBy selector: Selector
    ) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)):
            if !confirming { moveSelection(-1) }
            return true
        case #selector(NSResponder.moveDown(_:)):
            if !confirming { moveSelection(1) }
            return true
        // Per the system's StandardKeyBinding.dict, ⇧↑/⇧↓ arrive as the
        // selection-extending commands and ⌘↑/⌘↓ as the document-jump ones;
        // both ride the same forwarding as the plain arrows. Left alone the
        // field editor stretches the selection or parks the caret in a
        // one-line filter box, which is worth nothing
        case #selector(NSResponder.moveUpAndModifySelection(_:)),
            #selector(NSResponder.moveToBeginningOfDocument(_:)):
            if !confirming { jumpSelection(toFirst: true) }
            return true
        case #selector(NSResponder.moveDownAndModifySelection(_:)),
            #selector(NSResponder.moveToEndOfDocument(_:)):
            if !confirming { jumpSelection(toFirst: false) }
            return true
        case #selector(NSResponder.insertNewline(_:)):
            confirming ? overlayPrimaryClicked() : restoreSelected()
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            if confirming {
                dismissOverlay()
            } else {
                onHide?()
            }
            return true
        default:
            return false
        }
    }
}

// MARK: - Context menu

extension HistoryPanelViewController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let row = tableView.clickedRow
        guard entries.indices.contains(row) else { return }
        let entry = entries[row]
        let pin = NSMenuItem(
            title: entry.pinned ? L10n.t.unpin : L10n.t.pin,
            action: #selector(togglePinClicked(_:)), keyEquivalent: "")
        pin.target = self
        pin.representedObject = (entry.id, !entry.pinned)
        menu.addItem(pin)
        let delete = NSMenuItem(
            title: L10n.t.deleteEntry, action: #selector(deleteClicked(_:)), keyEquivalent: "")
        delete.target = self
        delete.representedObject = entry.id
        menu.addItem(delete)
    }
}

/// Hairline border overlay: strokes the same thin edge a menu has, since
/// maskImage clips the shape without stroking it, and it swallows no mouse
/// events
final class BorderOverlayView: NSView {
    private let radius: CGFloat

    init(radius: CGFloat) {
        self.radius = radius
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        nil
    }

    override func draw(_ dirtyRect: NSRect) {
        let inset = bounds.insetBy(dx: 0.5, dy: 0.5)
        let path = NSBezierPath(roundedRect: inset, xRadius: radius, yRadius: radius)
        path.lineWidth = 1
        NSColor.separatorColor.setStroke()
        path.stroke()
    }
}

/// Footer menu row: a title with a shortcut hint on the right, highlighting
/// the whole row in the accent color on hover, exactly as a menu does
private final class MenuItemRow: NSView {
    private let titleLabel: NSTextField
    private let shortcutLabel: NSTextField
    /// Click action
    private let onClick: () -> Void
    /// Currently hovered
    private var hovering = false {
        didSet { applyColors() }
    }

    init(title: String, shortcut: String, onClick: @escaping () -> Void) {
        titleLabel = NSTextField(labelWithString: title)
        shortcutLabel = NSTextField(labelWithString: shortcut)
        self.onClick = onClick
        super.init(frame: .zero)

        titleLabel.font = .systemFont(ofSize: 13)
        shortcutLabel.font = .systemFont(ofSize: 12)
        wantsLayer = true
        layer?.cornerRadius = 5

        let stack = NSStackView(views: [titleLabel, NSView(), shortcutLabel])
        stack.orientation = .horizontal
        stack.spacing = 8
        stack.alignment = .centerY
        stack.edgeInsets = NSEdgeInsets(top: 0, left: 11, bottom: 0, right: 11)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 22),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 5),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -5),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        applyColors()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    /// Hovering turns the whole row the accent color with white text, the
    /// system menu behaviour
    private func applyColors() {
        layer?.backgroundColor =
            hovering ? NSColor.selectedContentBackgroundColor.cgColor : nil
        titleLabel.textColor = hovering ? .selectedMenuItemTextColor : .labelColor
        shortcutLabel.textColor =
            hovering
            ? NSColor.selectedMenuItemTextColor.withAlphaComponent(0.75) : .tertiaryLabelColor
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas {
            removeTrackingArea(area)
        }
        addTrackingArea(
            NSTrackingArea(
                rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                owner: self, userInfo: nil))
    }

    /// Hover suppression while an in-panel alert is up
    ///
    /// The alert only blocks clicks, through hit testing; the tracking area
    /// keeps receiving enter and exit events, so without explicit suppression
    /// a menu row still lights up as the mouse passes over it behind the alert
    var hoverSuppressed = false {
        didSet {
            if hoverSuppressed { hovering = false }
        }
    }

    override func mouseEntered(with event: NSEvent) {
        guard !hoverSuppressed else { return }
        hovering = true
    }

    override func mouseExited(with event: NSEvent) {
        hovering = false
    }

    override func mouseUp(with event: NSEvent) {
        onClick()
    }
}
