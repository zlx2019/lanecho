// History row views, native view-based NSTableView rows.
//
// The look follows system menus (Maccy, Easydict): the whole row highlights in
// the accent color with white text, and the slot shortcut hint sits on the
// right. Colors are always semantic ones, so they adapt to the appearance.

import AppKit
import LanechoKit

/// Row view with menu-style full-row highlighting: a rounded bar in the accent
/// color
///
/// The panel is never the main window, so emphasized is forced to keep the
/// system accent color — otherwise it degrades to the inactive grey — and the
/// cell flips its text to white off the back of that.
final class PanelRowView: NSTableRowView {
    override var isEmphasized: Bool {
        get { true }
        set {}
    }

    override func drawSelection(in dirtyRect: NSRect) {
        guard selectionHighlightStyle != .none else { return }
        let rect = bounds.insetBy(dx: 5, dy: 0)
        NSColor.selectedContentBackgroundColor.setFill()
        NSBezierPath(roundedRect: rect, xRadius: 5, yRadius: 5).fill()
    }
}

/// Cell: kind icon, preview, count/remote/pinned badges and the slot hint
final class HistoryCellView: NSTableCellView {
    /// Reuse identifier
    static let identifier = NSUserInterfaceItemIdentifier("history-cell")

    private let kindIcon = NSImageView()
    private let preview = NSTextField(labelWithString: "")
    private let countBadge = NSTextField(labelWithString: "")
    private let originBadge = NSImageView()
    private let pinBadge = NSImageView()
    private let slotHint = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)

        kindIcon.symbolConfiguration = .init(pointSize: 11, weight: .regular)
        preview.font = .systemFont(ofSize: 13)
        preview.lineBreakMode = .byTruncatingTail
        preview.setContentHuggingPriority(.defaultLow, for: .horizontal)
        preview.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        countBadge.font = .systemFont(ofSize: 10)
        originBadge.image = NSImage(
            systemSymbolName: "arrow.down.left.circle", accessibilityDescription: nil)
        originBadge.symbolConfiguration = .init(pointSize: 10, weight: .regular)
        pinBadge.image = NSImage(systemSymbolName: "pin.fill", accessibilityDescription: nil)
        pinBadge.symbolConfiguration = .init(pointSize: 9, weight: .regular)
        slotHint.font = .systemFont(ofSize: 11)
        slotHint.alignment = .right

        // The slot hint is pinned to the trailing edge on its own: inside the
        // same stack it would follow the text, so on a short row the hint
        // would end up in the middle
        let leading = NSStackView(views: [
            kindIcon, preview, countBadge, originBadge, pinBadge,
        ])
        leading.orientation = .horizontal
        leading.spacing = 6
        leading.alignment = .centerY
        leading.translatesAutoresizingMaskIntoConstraints = false
        slotHint.translatesAutoresizingMaskIntoConstraints = false
        slotHint.setContentHuggingPriority(.required, for: .horizontal)
        slotHint.setContentCompressionResistancePriority(.required, for: .horizontal)
        addSubview(leading)
        addSubview(slotHint)
        NSLayoutConstraint.activate([
            leading.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 11),
            leading.centerYAnchor.constraint(equalTo: centerYAnchor),
            leading.trailingAnchor.constraint(
                lessThanOrEqualTo: slotHint.leadingAnchor, constant: -8),
            slotHint.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -11),
            slotHint.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        applyColors()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    /// The row view drives the selected state: emphasized means white text,
    /// matching system menus
    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { applyColors() }
    }

    /// Fill in one projection; `slotHint` is the slot hint string such as
    /// "⌥1", empty when there is none
    func configure(with meta: HistoryEntryMeta, slotHint hint: String) {
        kindIcon.image = NSImage(
            systemSymbolName: Self.kindSymbol(of: meta.kind), accessibilityDescription: nil)
        preview.stringValue = meta.preview.isEmpty ? " " : meta.preview
        countBadge.isHidden = meta.copyCount <= 1
        countBadge.stringValue = "×\(meta.copyCount)"
        originBadge.isHidden = meta.origin == nil
        pinBadge.isHidden = !meta.pinned
        slotHint.isHidden = hint.isEmpty
        slotHint.stringValue = hint
    }

    /// Repaint the colors for the current selection state
    private func applyColors() {
        let selected = backgroundStyle == .emphasized
        preview.textColor = selected ? .selectedMenuItemTextColor : .labelColor
        kindIcon.contentTintColor =
            selected ? .selectedMenuItemTextColor : .secondaryLabelColor
        let muted: NSColor =
            selected
            ? NSColor.selectedMenuItemTextColor.withAlphaComponent(0.75) : .tertiaryLabelColor
        countBadge.textColor = muted
        originBadge.contentTintColor = muted
        slotHint.textColor = muted
        // The pin badge stays a loud orange, turning white when selected so it
        // does not clash with the accent color
        pinBadge.contentTintColor = selected ? .selectedMenuItemTextColor : .systemOrange
    }

    /// Symbol name for a kind
    private static func kindSymbol(of kind: String) -> String {
        switch kind {
        case HistoryKind.image: "photo"
        case HistoryKind.files: "doc.on.doc"
        default: "text.alignleft"
        }
    }
}
