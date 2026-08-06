// App delegate: start the shell core, attach the menu bar icon and the history
// panel, and pump UI events.

import AppKit
import LanechoKit

/// App delegate
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Menu bar icon
    private var statusItem: StatusItemController?
    /// History panel
    private var panel: PanelController?
    /// Shell core
    private var core: AppCore?
    /// Global hotkeys
    private var hotkeys: HotkeyCoordinator?
    /// Settings window
    private var settingsWindow: SettingsWindowController?
    /// Settings window model, the forwarding target for events
    private var settingsModel: SettingsModel?
    /// About window
    private let aboutWindow = AboutWindowController()
    /// UI event pump
    private var eventTask: Task<Void, Never>?
    /// Pair request window (non-modal; only one shows at a time, the rest
    /// queue up)
    private let pairWindow = PairRequestWindowController()
    /// Queue of pairing requests waiting to be shown
    private var pairQueue: [PeerInfo] = []
    /// Fingerprint of the requester currently on screen, used for dedup
    private var presentingFingerprint: String?

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard LaunchGuard.check() else {
            NSApp.terminate(nil)
            return
        }
        let dataDir = Self.resolveDataDir()
        Task { @MainActor in
            do {
                let (core, events) = try await AppCore.start(dataDir: dataDir)
                self.core = core
                let settings = await core.currentSettings()
                L10n.apply(language: settings.language)
                let panel = PanelController(core: core)
                self.panel = panel
                self.statusItem = StatusItemController { panel.toggle() }
                let hotkeys = HotkeyCoordinator(core: core) { panel.toggle() }
                hotkeys.reregister(settings: settings)
                self.hotkeys = hotkeys
                let model = SettingsModel(core: core, hotkeys: hotkeys) { [weak self] in
                    self?.panel?.relocalize()
                    self?.settingsWindow?.relocalize()
                    self?.aboutWindow.relocalize()
                    self?.pairWindow.relocalize()
                    self?.refreshPendingTooltip()
                }
                self.settingsModel = model
                let settingsWindow = SettingsWindowController(model: model)
                self.settingsWindow = settingsWindow
                panel.onOpenSettings = { [weak self] in
                    self?.settingsWindow?.show()
                }
                panel.onShowAbout = { [weak self] in self?.aboutWindow.show() }
                self.eventTask = Task { [weak self] in
                    for await event in events {
                        self?.handle(event)
                    }
                }
                // Building the settings window creates and measures all five
                // SwiftUI tabs on the main thread. Defer that work until the
                // launch path yields, then keep the hidden window ready so
                // the first settings click is as cheap as later ones.
                Task { @MainActor [weak settingsWindow] in
                    await Task.yield()
                    settingsWindow?.prepare()
                }
                // Explain local network permission on first launch: once
                // denied, discovery fails silently and the user has no way
                // to find out why
                LocalNetworkOnboarding.presentIfNeeded()
            } catch {
                // Startup failed (port taken, data directory not writable):
                // say so, then quit
                let alert = NSAlert()
                alert.alertStyle = .critical
                alert.messageText = L10n.t.startFailedTitle
                alert.informativeText = String(describing: error)
                alert.runModal()
                NSApp.terminate(nil)
            }
        }
    }

    /// UI event dispatch (system notifications only fire in a packaged .app)
    private func handle(_ event: CoreEvent) {
        settingsModel?.handle(event)
        switch event {
        case .historyChanged:
            panel?.historyChanged()
        case .pairRequested(let peer):
            presentPairRequest(peer)
            refreshPendingTooltip()
        case .paired:
            refreshPendingTooltip()
        case .appliedRemote(let from, let preview):
            // The notification toggle is a persisted setting; read its
            // current value per event so a change takes effect at once
            Task { [weak self] in
                guard let core = self?.core, await core.currentSettings().notifyOnSync
                else { return }
                Notifications.syncArrived(from: from, preview: preview)
            }
        default:
            break
        }
    }

    /// Take inbound pairing requests one at a time
    ///
    /// The window is **non-modal** (see [`PairRequestWindowController`]): it
    /// returns as soon as it is shown and the event pump keeps running. That
    /// is exactly why `NSAlert.runModal()` is not used — while a modal is up,
    /// not one task on MainActor runs, the pump lives on MainActor, and the
    /// decision window lasts up to 300 seconds; the event stream would back
    /// up and drop from the head of the queue.
    ///
    /// Only one shows at a time: later requests queue, and the next one pops
    /// once a decision is made.
    private func presentPairRequest(_ peer: PeerInfo) {
        // Queueing the same peer twice is pointless; the engine already
        // dedups its pending table by fingerprint
        guard !pairQueue.contains(where: { $0.fingerprint == peer.fingerprint }),
            pairWindow.isPresenting == false || peer.fingerprint != presentingFingerprint
        else { return }
        pairQueue.append(peer)
        presentNextPairRequest()
    }

    /// Show the next queued request; if one is already up, wait for its
    /// decision
    private func presentNextPairRequest() {
        guard !pairWindow.isPresenting, !pairQueue.isEmpty, let core else { return }
        let peer = pairQueue.removeFirst()
        presentingFingerprint = peer.fingerprint
        pairWindow.present(peer) { accepted in
            Task { [weak self] in
                await core.respondPair(fingerprint: peer.fingerprint, accept: accepted)
                guard let self else { return }
                self.presentingFingerprint = nil
                // Fallback re-fetch: with the pump no longer held up,
                // events should not be lost — but the stream is still
                // bufferingNewest and can overflow if something else
                // backs it up. One actor call buys a second safety net
                for pending in await core.pendingPairRequests()
                where !self.pairQueue.contains(where: {
                    $0.fingerprint == pending.fingerprint
                }) {
                    self.pairQueue.append(pending)
                }
                self.presentNextPairRequest()
                self.refreshPendingTooltip()
            }
        }
    }

    /// Refresh the pending pairing count in the menu bar tooltip. The engine's
    /// pending table is the only source of truth — not the local queue length,
    /// which holds only requests not yet shown and excludes the one on screen
    private func refreshPendingTooltip() {
        guard let core else { return }
        Task { [weak self] in
            let pending = await core.pendingPairRequests().count
            self?.statusItem?.updateTooltip(pendingPairs: pending)
        }
    }

    /// Shutdown work before quitting: persist history and broadcast goodbye,
    /// then let the quit through once that finishes
    ///
    /// Raced against a deadline: after `.terminateLater`, failing to `reply`
    /// leaves the app **stuck in quitting forever** — the menu bar icon is
    /// still there but dead, and the user's only way out is a force kill,
    /// which loses exactly the history that has not been persisted. The
    /// shutdown chain does a multicast send and disk writes, either of which
    /// can hang, so give up on the cleanup rather than wedge.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let core else { return .terminateNow }
        self.core = nil
        Task {
            _ = await withTaskGroup(of: Void.self, returning: Void.self) { group in
                group.addTask { await core.shutdown() }
                group.addTask { try? await Task.sleep(for: Self.shutdownDeadline) }
                await group.next()
                group.cancelAll()
            }
            await MainActor.run {
                NSApp.reply(toApplicationShouldTerminate: true)
            }
        }
        return .terminateLater
    }

    /// Deadline for shutdown work; on timeout the quit goes through anyway
    /// (see applicationShouldTerminate)
    private static let shutdownDeadline: Duration = .seconds(3)

    /// Data directory: `--data-dir` overrides it for an isolated development
    /// run; by default it is the same directory the Tauri client uses
    private static func resolveDataDir() -> URL {
        let args = ProcessInfo.processInfo.arguments
        if let index = args.firstIndex(of: "--data-dir"), index + 1 < args.count {
            return URL(fileURLWithPath: args[index + 1])
        }
        let support = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appendingPathComponent("io.github.zlx2019.lanecho")
    }
}
