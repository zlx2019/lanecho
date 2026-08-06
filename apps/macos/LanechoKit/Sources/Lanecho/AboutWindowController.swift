// About window: hand-drawn, not the standard system panel.
//
// The standard about panel only holds a name, a version and one credits
// string, which is not enough surface for an open-source project. This lays
// out what such apps usually show: icon, name and version, a one-line pitch,
// entry links (homepage, issues, changelog, license), dependency credits and
// a copyright line.

import AppKit

/// About window controller; one instance, reused
@MainActor
final class AboutWindowController: NSObject {
    /// Window content width
    private static let contentWidth: CGFloat = 380

    private var window: NSWindow?

    /// Show the about window; as an accessory app, activating first is what
    /// makes it visible
    func show() {
        if window == nil {
            let window = NSWindow(contentViewController: NSViewController())
            window.contentView = buildContent()
            window.styleMask = [.titled, .closable]
            window.title = L10n.t.aboutTitle
            window.isReleasedWhenClosed = false
            window.setContentSize(window.contentView?.fittingSize ?? .zero)
            window.center()
            self.window = window
        }
        window?.title = L10n.t.aboutTitle
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
    }

    /// Language switch: rebuild the whole content, since AppKit bakes its
    /// strings in at construction time
    func relocalize() {
        guard let window else { return }
        window.contentView = buildContent()
        window.title = L10n.t.aboutTitle
        window.setContentSize(window.contentView?.fittingSize ?? .zero)
    }

    // MARK: - Content assembly

    /// Assemble the about window content
    private func buildContent() -> NSView {
        let texts = L10n.t

        let icon = NSImageView(image: Assets.appIcon ?? NSApp.applicationIconImage)
        icon.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 72),
            icon.heightAnchor.constraint(equalToConstant: 72),
        ])

        let name = NSTextField(labelWithString: appName)
        name.font = .systemFont(ofSize: 22, weight: .semibold)
        name.alignment = .center

        let version = NSTextField(
            labelWithString: "\(texts.aboutVersionLabel) \(appVersion)")
        version.font = .systemFont(ofSize: 11)
        version.textColor = .secondaryLabelColor
        version.alignment = .center
        // The version must be selectable so it can be copied into a report
        version.isSelectable = true

        let tagline = NSTextField(wrappingLabelWithString: texts.aboutTagline)
        tagline.font = .systemFont(ofSize: 12)
        tagline.textColor = .secondaryLabelColor
        tagline.alignment = .center
        tagline.preferredMaxLayoutWidth = Self.contentWidth - 60

        let links = NSStackView(views: [
            linkButton(texts.aboutHomepage, url: appRepositoryURL),
            linkButton(texts.aboutIssues, url: appIssuesURL),
            linkButton(texts.aboutReleases, url: appReleasesURL),
            linkButton(texts.aboutLicense, url: appLicenseURL),
        ])
        links.orientation = .horizontal
        links.spacing = 14
        links.alignment = .centerY

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        separator.widthAnchor.constraint(equalToConstant: Self.contentWidth - 60).isActive =
            true

        let builtWith = NSTextField(labelWithString: texts.aboutBuiltWith)
        builtWith.font = .systemFont(ofSize: 11)
        builtWith.textColor = .secondaryLabelColor
        builtWith.alignment = .center

        let dependencies = NSTextField(
            labelWithString: appDependencies.joined(separator: " · "))
        dependencies.font = .systemFont(ofSize: 11)
        dependencies.textColor = .tertiaryLabelColor
        dependencies.alignment = .center

        let copyright = NSTextField(labelWithString: texts.aboutCopyright)
        copyright.font = .systemFont(ofSize: 11)
        copyright.textColor = .tertiaryLabelColor
        copyright.alignment = .center

        let stack = NSStackView(views: [
            icon, name, version, tagline, links, separator, builtWith, dependencies,
            copyright,
        ])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 24, left: 30, bottom: 20, right: 30)
        // Group spacing: the name hugs the version, the credits heading hugs
        // the dependency list
        stack.setCustomSpacing(4, after: name)
        stack.setCustomSpacing(14, after: version)
        stack.setCustomSpacing(16, after: tagline)
        stack.setCustomSpacing(16, after: links)
        stack.setCustomSpacing(14, after: separator)
        stack.setCustomSpacing(4, after: builtWith)
        stack.setCustomSpacing(14, after: dependencies)
        stack.translatesAutoresizingMaskIntoConstraints = false

        let container = NSView()
        container.addSubview(stack)
        NSLayoutConstraint.activate([
            container.widthAnchor.constraint(equalToConstant: Self.contentWidth),
            stack.topAnchor.constraint(equalTo: container.topAnchor),
            stack.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor),
        ])
        return container
    }

    /// Link button: borderless, blue text, opens externally on click
    private func linkButton(_ title: String, url: URL) -> NSButton {
        let button = NSButton(title: title, target: nil, action: nil)
        button.isBordered = false
        button.attributedTitle = NSAttributedString(
            string: title,
            attributes: [
                .font: NSFont.systemFont(ofSize: 12),
                .foregroundColor: NSColor.linkColor,
            ])
        button.target = LinkOpener.shared
        button.action = #selector(LinkOpener.open(_:))
        button.identifier = NSUserInterfaceItemIdentifier(url.absoluteString)
        return button
    }
}

/// Link opener, the shared button target; the URL rides on the identifier so
/// each link does not need its own object
@MainActor
private final class LinkOpener: NSObject {
    static let shared = LinkOpener()

    @objc func open(_ sender: NSButton) {
        guard let raw = sender.identifier?.rawValue, let url = URL(string: raw) else { return }
        NSWorkspace.shared.open(url)
    }
}
