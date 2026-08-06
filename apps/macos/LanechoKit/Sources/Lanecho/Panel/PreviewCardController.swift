// Floating preview card: a pure presentation layer, the native port of the
// Tauri client's preview window.
//
// Three hard rules:
// - **Never take focus**: PreviewPanel explicitly overrides canBecomeKey and
//   canBecomeMain to false rather than leaning on borderless defaults, so the
//   panel's hide-on-blur and ⌘V focus hand-back stay intact
// - Placement lives entirely in this layer (the pure function placePreview:
//   right side first, flip left when it does not fit, clamp on both axes)
// - Every path that hides the panel must hide the card too, which the panel
//   side guarantees by calling hide
//
// The content **must** sit inside a scroll view: the card view is pinned on
// all four sides to the window's contentView, so the intrinsic height of a
// long-text label propagates up the constraint chain to the window, and
// AppKit stretches the window to the content height to satisfy it —
// `setContentSize` is overridden on the spot and the height cap means
// nothing, showing up as a card that runs from the top of the screen to the
// bottom. A scroll view cuts that chain.

import AppKit
import LanechoKit

/// Preview card window: never becomes key or main
///
/// The card has to receive scroll events when its content overflows (see the
/// conditional mouse pass-through in `show`), which demands a guarantee **in
/// code** that clicking it cannot steal the panel's keyboard focus — the
/// panel would otherwise hide itself on blur. Do not rely on the borderless
/// defaults, or a later change to styleMask becomes a trap.
private final class PreviewPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Preview card content
enum PreviewContent {
    /// Text (already truncated) plus the total scalar count
    case text(String, totalChars: Int)
    /// Image (the display version; the restore path is unaffected)
    case image(NSImage)
    /// File path list
    case files([String])
}

/// Floating preview card controller
@MainActor
final class PreviewCardController {
    /// Fixed card width
    private static let cardWidth: CGFloat = 320
    /// Card height range; the cap matches the Tauri client, and anything over
    /// it scrolls inside the content area
    private static let heightRange: ClosedRange<CGFloat> = 80...480
    /// Minimum visible height for the content area — squeeze past this and
    /// the scroll area is pointless
    private static let minBodyHeight: CGFloat = 60
    /// Text display limit in scalars; same value as the Tauri client's
    /// PREVIEW_TEXT_LIMIT, and changing one side means changing the other
    static let textLimit = 20000
    /// Largest text size that still gets measured exactly, in scalars
    ///
    /// Anything larger switches to NSTextView and is treated as full height
    /// outright. 2000 is the number because it used to be the display limit,
    /// and laying out that much takes about 5ms.
    private static let exactMeasureLimit = 2000
    /// Pop-in animation duration, measured against Maccy at roughly 4 frames
    /// at 30fps
    private static let popInDuration: TimeInterval = 0.13
    /// Fade-out duration, quicker than the pop-in so it does not trail
    private static let fadeOutDuration: TimeInterval = 0.08
    /// Starting scale of the pop-in
    private static let popInScale: CGFloat = 0.94

    private let panel: NSPanel
    /// Scalable container holding the card; scaling an NSVisualEffectView
    /// directly comes out blurry
    private let container = NSView()
    /// Fade-out in progress; another show during it has to interrupt
    private var fadingOut = false

    init() {
        panel = PreviewPanel(
            contentRect: NSRect(x: 0, y: 0, width: Self.cardWidth, height: 100),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: true)
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.ignoresMouseEvents = true
        container.wantsLayer = true
        panel.contentView = container
    }

    /// Show the preview card; the content measures its own height and the
    /// pure placement function positions it after clamping
    ///
    /// The first pop-in fades in with a slight scale-up, matching how Maccy
    /// feels; while already visible only the content and position change and
    /// the animation does not replay — repeated popping while sweeping across
    /// rows is jarring
    func show(
        content: PreviewContent, meta: HistoryEntryMeta, appIcon: NSImage?,
        panelFrame: NSRect, anchorTopY: CGFloat, screen: NSScreen?
    ) {
        let (card, scrollable) = Self.buildCard(content: content, meta: meta, appIcon: appIcon)
        // Self-measured height at the fixed width; buildCard has already
        // squeezed the content area within the cap
        let fitting = card.fittingSize
        let height = min(
            max(fitting.height, Self.heightRange.lowerBound), Self.heightRange.upperBound)
        let size = NSSize(width: Self.cardWidth, height: height)
        // Conditional pass-through: while the content fits, keep ignoring the
        // mouse so moving onto the card does not disturb the panel's hover
        // decision; only when it does not fit is pass-through given up so the
        // wheel can scroll — focus safety falls back on PreviewPanel's
        // canBecomeKey = false, which stops a click stealing panel focus
        panel.ignoresMouseEvents = !scrollable

        container.subviews.forEach { $0.removeFromSuperview() }
        card.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(card)
        NSLayoutConstraint.activate([
            card.topAnchor.constraint(equalTo: container.topAnchor),
            card.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            card.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            card.trailingAnchor.constraint(equalTo: container.trailingAnchor),
        ])

        panel.setContentSize(size)
        let visible = screen?.visibleFrame ?? panelFrame
        panel.setFrameOrigin(
            placePreview(
                panelFrame: panelFrame, cardSize: size, anchorTopY: anchorTopY,
                visibleFrame: visible))

        let wasVisible = panel.isVisible && !fadingOut
        fadingOut = false
        panel.alphaValue = wasVisible ? 1 : 0
        panel.orderFront(nil)
        guard !wasVisible else { return }
        // Fade in
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.popInDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().alphaValue = 1
        }
        // Slight scale-up as a layer animation: AppKit's layer-backed views
        // have implicit animations off by default, so make it explicit
        let pop = CABasicAnimation(keyPath: "transform.scale")
        pop.fromValue = Self.popInScale
        pop.toValue = 1
        pop.duration = Self.popInDuration
        pop.timingFunction = CAMediaTimingFunction(name: .easeOut)
        container.layer?.add(pop, forKey: "pop-in")
    }

    /// Hide the card: a quick fade-out, then it really goes away; a show in
    /// the meantime interrupts it
    func hide() {
        guard panel.isVisible, !fadingOut else {
            panel.orderOut(nil)
            return
        }
        fadingOut = true
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.fadeOutDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().alphaValue = 0
        } completionHandler: { [weak self] in
            guard let self, fadingOut else { return }
            fadingOut = false
            panel.orderOut(nil)
        }
    }

    // MARK: - Card construction

    /// Assemble the card view: a vibrancy root plus content, metadata and
    /// action hints. The layout follows Maccy — a blank line between the three
    /// blocks, and metadata as single "label: value" lines rather than a table
    private static func buildCard(
        content: PreviewContent, meta: HistoryEntryMeta, appIcon: NSImage?
    ) -> (view: NSView, scrollable: Bool) {
        let effect = NSVisualEffectView()
        effect.material = .popover
        effect.blendingMode = .behindWindow
        effect.state = .active
        effect.maskImage = roundedMask(radius: 16)

        // Put the content in a scroll view and give it an **explicit height**:
        // let the height propagate up from the content through constraints
        // and the window blows out (see the note at the top of the file)
        let body = contentView(of: content)
        let scroll = scrollBox(body.view, autoLayoutDocument: body.autoLayout)
        let bodyHeightConstraint = scroll.heightAnchor.constraint(equalToConstant: body.height)
        bodyHeightConstraint.isActive = true

        // Divider between the content and the info block, as on the Tauri
        // client's preview card
        let divider = NSBox()
        divider.boxType = .separator
        divider.translatesAutoresizingMaskIntoConstraints = false
        divider.widthAnchor.constraint(equalToConstant: cardWidth - 34).isActive = true
        let metadata = metadataView(meta: meta, appIcon: appIcon)
        let hints = hintsView(meta: meta)
        var rows: [NSView] = [scroll]
        let badge = truncationBadge(for: content)
        if let badge { rows.append(badge) }
        rows.append(contentsOf: [divider, metadata, hints])
        let stack = NSStackView(views: rows)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 13
        stack.edgeInsets = NSEdgeInsets(top: 17, left: 17, bottom: 16, right: 17)
        stack.translatesAutoresizingMaskIntoConstraints = false
        // The badge annotates the content, so it hugs the content area
        // instead of taking the between-block spacing
        if badge != nil { stack.setCustomSpacing(5, after: scroll) }
        effect.addSubview(stack)
        // Hairline border, so the card edge still holds up over a busy
        // wallpaper
        let border = BorderOverlayView(radius: 16)
        border.translatesAutoresizingMaskIntoConstraints = false
        effect.addSubview(border)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: effect.topAnchor),
            stack.bottomAnchor.constraint(equalTo: effect.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: effect.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: effect.trailingAnchor),
            effect.widthAnchor.constraint(equalToConstant: cardWidth),
            border.topAnchor.constraint(equalTo: effect.topAnchor),
            border.bottomAnchor.constraint(equalTo: effect.bottomAnchor),
            border.leadingAnchor.constraint(equalTo: effect.leadingAnchor),
            border.trailingAnchor.constraint(equalTo: effect.trailingAnchor),
        ])

        // Measure the whole card: over the cap, squeeze the content area down
        // to what fits, and the difference becomes the scrollable amount
        let overflow = effect.fittingSize.height - heightRange.upperBound
        guard overflow > 0 else { return (effect, false) }
        bodyHeightConstraint.constant = max(minBodyHeight, body.height - overflow)
        return (effect, true)
    }

    /// Wrap the content in a scroll view: vertical scrolling, overlay
    /// scrollers, transparent background so the vibrancy shows through
    ///
    /// - `autoLayoutDocument`: NSTextView must use autoresizing because it
    ///   manages its own textContainer size; pinning it with Auto Layout
    ///   fights it
    private static func scrollBox(_ body: NSView, autoLayoutDocument: Bool) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.verticalScrollElasticity = .allowed
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.widthAnchor.constraint(equalToConstant: cardWidth - 34).isActive = true
        scroll.documentView = body
        guard autoLayoutDocument else { return scroll }
        body.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            // Pin width and top only; leaving the bottom free makes the
            // content's natural height the scrollable range
            body.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            body.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            body.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])
        return scroll
    }

    /// Result of building the content area
    private struct Body {
        let view: NSView
        /// Desired height, already accounting for the "does not fit, so take
        /// full height" decision
        let height: CGFloat
        /// Whether the documentView uses Auto Layout; NSTextView must use
        /// autoresizing
        let autoLayout: Bool
    }

    /// Content area
    ///
    /// Text takes one of two paths, split by **layout cost**:
    /// - Short text (≤ exactMeasureLimit): still a wrapping label measured
    ///   exactly so the card hugs the content — layout at that scale is a few
    ///   milliseconds
    /// - Long text: switch to NSTextView and **never measure**. At 20k
    ///   scalars, `NSTextField.fittingSize` takes 55ms, which runs on every
    ///   hover and fires row by row while sweeping, a visible stutter;
    ///   NSTextView merely setting the text is 2.8ms because TextKit lays out
    ///   by viewport and never touches what is off screen. The answer is
    ///   always going to be "does not fit", so report the cap as the height
    ///   and skip the measurement
    private static func contentView(of content: PreviewContent) -> Body {
        switch content {
        case .text(let text, let totalChars):
            guard totalChars > exactMeasureLimit else {
                let label = NSTextField(wrappingLabelWithString: text)
                label.font = .systemFont(ofSize: 13)
                label.preferredMaxLayoutWidth = cardWidth - 34
                return Body(view: label, height: label.fittingSize.height, autoLayout: true)
            }
            return Body(
                view: longTextView(text), height: heightRange.upperBound, autoLayout: false)
        case .image(let image):
            let imageView = NSImageView(image: image)
            imageView.imageScaling = .scaleProportionallyDown
            // Leave room for the metadata and hint blocks so the whole image
            // card fits under the height cap — a full image should not turn
            // on a scroller over a few dozen points of overflow, which would
            // also give up mouse pass-through
            let maxHeight: CGFloat = 270
            let scale = min(
                1, (cardWidth - 34) / max(1, image.size.width),
                maxHeight / max(1, image.size.height))
            imageView.translatesAutoresizingMaskIntoConstraints = false
            let drawn = NSSize(
                width: max(40, image.size.width * scale),
                height: max(40, image.size.height * scale))
            NSLayoutConstraint.activate([
                imageView.widthAnchor.constraint(equalToConstant: drawn.width),
                imageView.heightAnchor.constraint(equalToConstant: drawn.height),
            ])
            return Body(view: imageView, height: drawn.height, autoLayout: true)
        case .files(let paths):
            let shown = paths.prefix(12).joined(separator: "\n")
            let text = paths.count > 12 ? shown + "\n…(\(paths.count))" : shown
            let label = NSTextField(wrappingLabelWithString: text)
            label.font = .systemFont(ofSize: 11)
            label.textColor = .secondaryLabelColor
            label.preferredMaxLayoutWidth = cardWidth - 34
            label.lineBreakMode = .byTruncatingMiddle
            return Body(view: label, height: label.fittingSize.height, autoLayout: true)
        }
    }

    /// Long-text view; TextKit lays out by viewport, so setting the text does
    /// not lay out the whole document
    ///
    /// Not selectable: it matches the label behaviour it replaces and keeps
    /// the card from grabbing any interaction.
    private static func longTextView(_ text: String) -> NSTextView {
        let width = cardWidth - 34
        let view = NSTextView(frame: NSRect(x: 0, y: 0, width: width, height: 0))
        view.isEditable = false
        view.isSelectable = false
        view.drawsBackground = false
        view.font = .systemFont(ofSize: 13)
        view.textColor = .labelColor
        view.textContainerInset = .zero
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.minSize = NSSize(width: 0, height: 0)
        view.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.containerSize = NSSize(
            width: width, height: CGFloat.greatestFiniteMagnitude)
        view.textContainer?.widthTracksTextView = true
        view.string = text
        return view
    }

    /// Truncation badge showing the total length; it sits **outside** the
    /// scroll area, or it would only appear after scrolling to the bottom
    private static func truncationBadge(for content: PreviewContent) -> NSView? {
        guard case .text(_, let totalChars) = content, totalChars > textLimit else {
            return nil
        }
        let label = NSTextField(labelWithString: "… \(totalChars)")
        label.font = .systemFont(ofSize: 10)
        label.textColor = .tertiaryLabelColor
        return label
    }

    /// Metadata block: one "label: value" per line, as in Maccy; the source
    /// application's icon is inlined in the same line's rich text rather than
    /// getting a column of its own
    private static func metadataView(meta: HistoryEntryMeta, appIcon: NSImage?) -> NSView {
        let texts = L10n.t
        var lines: [NSView] = []
        if let origin = meta.origin {
            lines.append(metaLine(texts.originLabel, value: origin))
        } else if let app = meta.sourceApp {
            lines.append(metaLine(texts.sourceAppLabel, value: app, icon: appIcon))
        }
        lines.append(metaLine(texts.firstCopiedLabel, value: formatTimestamp(meta.firstCopiedAt)))
        lines.append(metaLine(texts.lastCopiedLabel, value: formatTimestamp(meta.lastCopiedAt)))
        lines.append(metaLine(texts.copyCountLabel, value: "\(meta.copyCount)"))

        let stack = NSStackView(views: lines)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 5
        return stack
    }

    /// One metadata line: secondary color for the label, primary for the
    /// value; an icon, when present, is inlined ahead of the value
    private static func metaLine(_ label: String, value: String, icon: NSImage? = nil)
        -> NSTextField
    {
        let font = NSFont.systemFont(ofSize: 12)
        let line = NSMutableAttributedString(
            string: "\(label): ",
            attributes: [.font: font, .foregroundColor: NSColor.secondaryLabelColor])
        if let icon {
            let attachment = NSTextAttachment()
            let sized = NSImage(size: NSSize(width: 13, height: 13), flipped: false) { rect in
                icon.draw(in: rect)
                return true
            }
            attachment.image = sized
            // Drop it 2pt so it lines up with the text baseline visually
            attachment.bounds = NSRect(x: 0, y: -2, width: 13, height: 13)
            line.append(NSAttributedString(attachment: attachment))
            line.append(NSAttributedString(string: " "))
        }
        line.append(
            NSAttributedString(
                string: value,
                attributes: [.font: font, .foregroundColor: NSColor.labelColor]))
        let field = NSTextField(labelWithAttributedString: line)
        field.lineBreakMode = .byTruncatingTail
        return field
    }

    /// Action hint block (pin, delete); every shortcut it advertises is
    /// implemented in the panel's key-equivalent chain
    private static func hintsView(meta: HistoryEntryMeta) -> NSView {
        let texts = L10n.t
        let stack = NSStackView(views: [
            hintLine(meta.pinned ? texts.hintUnpin : texts.hintPin),
            hintLine(texts.hintDelete),
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        return stack
    }

    /// One hint line, small text in the secondary color
    private static func hintLine(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: 12)
        label.textColor = .secondaryLabelColor
        return label
    }

    /// Absolute time, formatted for the region the UI language implies, as in
    /// the Tauri client
    private static func formatTimestamp(_ ms: UInt64) -> String {
        let formatter = DateFormatter()
        formatter.locale = L10n.locale
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    /// Rounded mask, built the same way as the panel's
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
}
