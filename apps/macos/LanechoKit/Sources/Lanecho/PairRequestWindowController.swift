// Inbound pair request window: hand-drawn and **non-modal**.
//
// Why not NSAlert: during `runModal()` **not a single task** on MainActor
// runs — the counter increments by zero while the alert is up, so this is a
// full stall, not "slow scheduling". The UI event pump runs on MainActor, so
// the pump stops dead the moment the alert opens; the event stream is
// bufferingNewest and drops from the head once full, which can swallow
// history refreshes, device up/down and remote sync notifications alike. The
// decision window stays open for up to 300 seconds, far too long a stall.
//
// A hand-drawn window returns right after makeKeyAndOrderFront and the pump
// keeps running; the user's decision comes back asynchronously through a
// callback. It also shows more than a system alert would — the fingerprint
// and the platform — because pairing is a security boundary and the user has
// every reason to check the peer's fingerprint before accepting.
//
// Closing the window means reject: a pairing request may be ignored, and
// nobody should be forced to answer.

import AppKit
import LanechoKit

/// Pair request window controller; one instance, reused
@MainActor
final class PairRequestWindowController: NSObject, NSWindowDelegate {
    /// Window content width
    private static let contentWidth: CGFloat = 340

    private var window: NSWindow?
    /// The requester on screen (nil = idle)
    private var current: PeerInfo?
    /// Decision callback (true = accept, false = reject); cleared as soon as
    /// it is consumed so it cannot fire twice
    private var onDecision: ((Bool) -> Void)?

    /// Whether a request is on screen
    var isPresenting: Bool { current != nil }

    /// Show one pairing request; non-blocking, returns at once and delivers
    /// the decision through `onDecision`
    func present(_ peer: PeerInfo, onDecision: @escaping (Bool) -> Void) {
        current = peer
        self.onDecision = onDecision

        let window = window ?? makeWindow()
        self.window = window
        window.contentView = buildContent(peer)
        window.title = L10n.t.pairRequestTitle
        window.setContentSize(window.contentView?.fittingSize ?? .zero)
        window.center()
        // As an accessory app, the window cannot come to the front unless the
        // app is activated first
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    /// Language switch: rebuild the whole content if a request is on screen,
    /// since AppKit bakes its strings in at construction time
    func relocalize() {
        guard let window, let peer = current else { return }
        window.contentView = buildContent(peer)
        window.title = L10n.t.pairRequestTitle
        window.setContentSize(window.contentView?.fittingSize ?? .zero)
    }

    // MARK: - Decision

    @objc private func acceptClicked() {
        finish(accepted: true)
    }

    @objc private func rejectClicked() {
        finish(accepted: false)
    }

    /// Closing the window counts as a reject; pairing may be ignored and
    /// nobody is forced to answer
    func windowWillClose(_ notification: Notification) {
        finish(accepted: false)
    }

    /// Wrap up: fire the callback once and reset. Clear the callback first —
    /// orderOut triggers windowWillClose, which would otherwise call back a
    /// second time
    private func finish(accepted: Bool) {
        guard let callback = onDecision else { return }
        onDecision = nil
        current = nil
        window?.orderOut(nil)
        callback(accepted)
    }

    // MARK: - Content assembly

    /// Build the window once and reuse it afterwards
    private func makeWindow() -> NSWindow {
        let window = NSWindow(contentViewController: NSViewController())
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.level = .floating
        return window
    }

    /// Assemble the window content
    private func buildContent(_ peer: PeerInfo) -> NSView {
        let texts = L10n.t

        let icon = NSImageView(image: Assets.appIcon ?? NSApp.applicationIconImage)
        icon.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 56),
            icon.heightAnchor.constraint(equalToConstant: 56),
        ])

        let headline = NSTextField(labelWithString: peer.name)
        headline.font = .systemFont(ofSize: 15, weight: .semibold)
        headline.alignment = .center

        let body = NSTextField(wrappingLabelWithString: texts.pairRequestBody)
        body.font = .systemFont(ofSize: 12)
        body.textColor = .secondaryLabelColor
        body.alignment = .center
        body.preferredMaxLayoutWidth = Self.contentWidth - 40

        // Fingerprint: pairing is a security boundary, so it must be
        // checkable against the peer before accepting (16 characters, same as
        // the settings page)
        let fingerprint = NSTextField(
            labelWithString: "\(texts.fingerprintLabel): \(peer.fingerprint.prefix(16))")
        fingerprint.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        fingerprint.textColor = .tertiaryLabelColor
        fingerprint.alignment = .center

        let reject = NSButton(
            title: texts.reject, target: self, action: #selector(rejectClicked))
        reject.bezelStyle = .rounded
        reject.keyEquivalent = "\u{1b}"  // Esc = reject
        let accept = NSButton(
            title: texts.accept, target: self, action: #selector(acceptClicked))
        accept.bezelStyle = .rounded
        accept.keyEquivalent = "\r"  // Return = accept (the default button)
        let buttons = NSStackView(views: [reject, accept])
        buttons.orientation = .horizontal
        buttons.spacing = 12
        buttons.distribution = .fillEqually

        let stack = NSStackView(views: [icon, headline, body, fingerprint, buttons])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 22, left: 20, bottom: 18, right: 20)
        stack.setCustomSpacing(16, after: icon)
        stack.setCustomSpacing(18, after: fingerprint)
        stack.translatesAutoresizingMaskIntoConstraints = false

        let container = NSView()
        container.addSubview(stack)
        NSLayoutConstraint.activate([
            container.widthAnchor.constraint(equalToConstant: Self.contentWidth),
            stack.topAnchor.constraint(equalTo: container.topAnchor),
            stack.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            buttons.widthAnchor.constraint(equalToConstant: Self.contentWidth - 40),
        ])
        return container
    }
}
