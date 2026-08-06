// Settings page contents (five tabs; the tab shell lives in
// SettingsWindowController, a classic preferences window).
//
// Laid out with SettingsKit's non-scrolling grouped cards: the window height
// follows the content, page width is 460 (at 560 the toggle rows are far too
// airy on either side for this sparse content).

import LanechoKit
import SwiftUI

/// Page width
let settingsPageWidth: CGFloat = 460

/// Page frame (uniform padding and a fixed width)
///
/// The trailing `Spacer(minLength: 0)` pushes short content to the top and
/// leaves the slack at the bottom, and **does not raise fittingSize** — that is
/// what lets the window size be fixed once from the tallest page so switching
/// tabs never makes it jump
private struct SettingsPage<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            content
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(width: settingsPageWidth)
    }
}

/// General: display name / language / preview card delay / version
struct GeneralTab: View {
    @Bindable var model: SettingsModel
    @FocusState private var nameFocused: Bool

    var body: some View {
        SettingsPage {
            SettingsSection {
                SettingsRow(model.texts.deviceName) {
                    // Width follows the content (a short name leaves no wide
                    // gap); saves on return or blur. alignment must be spelled
                    // out as .trailing: once fixedSize shrinks the field to its
                    // content width, the enclosing frame **centers** it by
                    // default, leaving a gap on the right that does not line up
                    // with the other rows on this page (fingerprint, language,
                    // delay)
                    TextField("", text: $model.deviceName)
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                        .fixedSize()
                        .frame(minWidth: 120, maxWidth: 300, alignment: .trailing)
                        .focused($nameFocused)
                        .onSubmit { model.saveName() }
                        .onChange(of: nameFocused) {
                            if !nameFocused { model.saveName() }
                        }
                }
                SettingsDivider()
                SettingsRow(model.texts.fingerprintLabel) {
                    Text(String(model.localFingerprint.prefix(16)))
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
            SettingsSection(
                footer: model.autostartAvailable ? nil : model.texts.autostartUnavailable
            ) {
                SettingsToggleRow(label: model.texts.autostartLabel, isOn: autostartBinding)
                    .disabled(!model.autostartAvailable)
            }
            SettingsSection {
                SettingsRow(model.texts.languageLabel) {
                    Picker("", selection: languageBinding) {
                        Text(model.texts.langSystem).tag("")
                        Text("中文").tag("zh")
                        Text("English").tag("en")
                    }
                    .labelsHidden()
                    .fixedSize()
                }
                SettingsDivider()
                SettingsRow(model.texts.previewDelay) {
                    HStack(spacing: 8) {
                        Text("\(model.settings.previewDelayMs) ms")
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                        Stepper(
                            "", value: previewDelayBinding, in: 0...2000, step: 50
                        )
                        .labelsHidden()
                    }
                }
            }
        }
    }

    private var languageBinding: Binding<String> {
        Binding(
            get: { model.settings.language },
            set: { model.changeLanguage($0) })
    }

    private var previewDelayBinding: Binding<UInt32> {
        Binding(
            get: { model.settings.previewDelayMs },
            set: { value in model.patch { $0.previewDelayMs = value } })
    }

    private var autostartBinding: Binding<Bool> {
        Binding(
            get: { model.autostart },
            set: { model.toggleAutostart($0) })
    }
}

/// Online devices: the discovery list plus pairing management (the list area
/// scrolls within a capped height so the page height stays stable)
struct DevicesTab: View {
    @Bindable var model: SettingsModel

    var body: some View {
        SettingsPage {
            SettingsSection(
                footer:
                    "\(model.texts.localDevice): \(model.deviceName) [\(model.localFingerprint.prefix(8))]"
            ) {
                if model.devices.isEmpty {
                    Text(model.texts.noDevices)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 24)
                } else {
                    ScrollView {
                        VStack(spacing: 0) {
                            ForEach(Array(model.devices.enumerated()), id: \.element.id) {
                                index, device in
                                if index > 0 {
                                    SettingsDivider()
                                }
                                deviceRow(device)
                            }
                        }
                    }
                    .frame(height: min(CGFloat(model.devices.count) * 46, 240))
                }
            }
        }
        .task { await model.refreshDevices() }
    }

    @ViewBuilder
    private func deviceRow(_ device: SettingsModel.DeviceRow) -> some View {
        HStack(spacing: 8) {
            Circle()
                .fill(device.online ? Color.green : Color.gray.opacity(0.5))
                .frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 1) {
                Text(device.name)
                HStack(spacing: 6) {
                    if !device.platform.isEmpty {
                        Text(device.platform)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    Text(String(device.id.prefix(8)))
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            Spacer()
            if device.paired {
                Text(model.texts.pairedBadge)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button(model.texts.unpairAction) {
                    model.unpair(fingerprint: device.id)
                }
                .controlSize(.small)
            } else if model.pairingBusy.contains(device.id) {
                ProgressView()
                    .controlSize(.small)
            } else if device.online {
                Button(model.texts.pairAction) {
                    model.pair(fingerprint: device.id)
                }
                .controlSize(.small)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
    }
}

/// Sync: direction policy / type toggles and file limit / notifications /
/// incognito / port
struct SyncTab: View {
    @Bindable var model: SettingsModel
    @FocusState private var portFocused: Bool
    @FocusState private var limitFocused: Bool

    var body: some View {
        SettingsPage {
            SettingsSection {
                // Single-select semantics use the native segmented control: it
                // fits four mutually exclusive options on one line, whereas a
                // system radio group stretches this row into a tall column
                SettingsRow(model.texts.syncModeLabel) {
                    Picker("", selection: syncModeBinding) {
                        Text(model.texts.syncModeOff).tag("off")
                        Text(model.texts.syncModeBoth).tag("both")
                        Text(model.texts.syncModeSend).tag("send")
                        Text(model.texts.syncModeReceive).tag("receive")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                }
            }
            SettingsSection(footer: model.texts.syncTypesNote) {
                SettingsCheckRow(model.texts.syncTypeText, isOn: checkBinding(\.syncText))
                SettingsDivider()
                SettingsCheckRow(model.texts.syncTypeImages, isOn: checkBinding(\.syncImages))
                SettingsDivider()
                SettingsCheckRow(model.texts.syncTypeFiles, isOn: checkBinding(\.syncFiles))
                SettingsDivider()
                // The limit only applies to file sync: it gets its own row and
                // is greyed out while "files" is unchecked
                SettingsRow(model.texts.fileLimitLabel) {
                    HStack(spacing: 6) {
                        TextField("", text: $model.fileLimitText)
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 56)
                            .multilineTextAlignment(.trailing)
                            .focused($limitFocused)
                            .onSubmit { model.saveFileLimit() }
                            .onChange(of: limitFocused) {
                                if !limitFocused { model.saveFileLimit() }
                            }
                        Text("MB")
                            .foregroundStyle(.secondary)
                    }
                }
                .disabled(!model.settings.syncFiles)
                .opacity(model.settings.syncFiles ? 1 : 0.45)
            }
            SettingsSection(footer: model.texts.notifyBundleNote) {
                SettingsToggleRow(
                    label: model.texts.notifyOnSync, isOn: toggleBinding(\.notifyOnSync))
            }
            SettingsSection(footer: model.texts.incognitoNote) {
                SettingsToggleRow(label: model.texts.incognitoLabel, isOn: incognitoBinding)
            }
            SettingsSection(footer: model.texts.autoPasteNote) {
                SettingsToggleRow(label: model.texts.autoPasteLabel, isOn: autoPasteBinding)
            }
            SettingsSection(footer: model.texts.portRestartNote) {
                SettingsRow(model.texts.portLabel) {
                    TextField("", text: $model.portText)
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 80)
                        .multilineTextAlignment(.trailing)
                        .focused($portFocused)
                        .onSubmit { model.savePort() }
                        .onChange(of: portFocused) {
                            if !portFocused { model.savePort() }
                        }
                }
            }
        }
    }

    private var syncModeBinding: Binding<String> {
        Binding(
            get: { model.settings.syncMode },
            set: { value in model.patch { $0.syncMode = value } })
    }

    private func toggleBinding(
        _ keyPath: WritableKeyPath<LanechoKit.Settings, Bool>
    ) -> Binding<Bool> {
        Binding(
            get: { model.settings[keyPath: keyPath] },
            set: { value in model.patch { $0[keyPath: keyPath] = value } })
    }

    private func checkBinding(
        _ keyPath: WritableKeyPath<LanechoKit.Settings, Bool>
    ) -> Binding<Bool> {
        toggleBinding(keyPath)
    }

    private var incognitoBinding: Binding<Bool> {
        Binding(
            get: { model.incognito },
            set: { model.toggleIncognito($0) })
    }

    private var autoPasteBinding: Binding<Bool> {
        Binding(
            get: { model.settings.autoPaste },
            set: { model.toggleAutoPaste($0) })
    }
}

/// Storage: entry cap / sort order / recorded types / usage and clearing
struct StorageTab: View {
    @Bindable var model: SettingsModel
    @State private var confirmingClear = false

    var body: some View {
        SettingsPage {
            SettingsSection {
                SettingsRow(model.texts.maxEntries) {
                    HStack(spacing: 8) {
                        Text("\(model.settings.historyMaxEntries)")
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                        Stepper("", value: maxEntriesBinding, in: 50...1000, step: 50)
                            .labelsHidden()
                    }
                }
                SettingsDivider()
                SettingsRow(model.texts.sortLabel) {
                    Picker("", selection: sortBinding) {
                        Text(model.texts.sortRecent).tag("recent")
                        Text(model.texts.sortFrequent).tag("frequent")
                    }
                    .labelsHidden()
                    .fixedSize()
                }
            }
            SettingsSection(footer: model.texts.recordNote) {
                // Multi-select semantics (which types to record) use
                // checkboxes, matching the sync type toggles
                SettingsCheckRow(
                    model.texts.recordText, isOn: recordBinding(\.historyRecordText))
                SettingsDivider()
                SettingsCheckRow(
                    model.texts.recordImages, isOn: recordBinding(\.historyRecordImages))
                SettingsDivider()
                SettingsCheckRow(
                    model.texts.recordFiles, isOn: recordBinding(\.historyRecordFiles))
            }
            SettingsSection {
                SettingsRow(model.texts.usageLabel) {
                    Text(
                        ByteCountFormatter.string(
                            fromByteCount: Int64(model.usageBytes), countStyle: .file)
                    )
                    .foregroundStyle(.secondary)
                }
                SettingsDivider()
                HStack {
                    Button(model.texts.clearHistory, role: .destructive) {
                        confirmingClear = true
                    }
                    .controlSize(.small)
                    .confirmationDialog(
                        model.texts.confirmClearTitle, isPresented: $confirmingClear
                    ) {
                        Button(model.texts.clearConfirm, role: .destructive) {
                            model.clearHistory()
                        }
                        Button(model.texts.cancel, role: .cancel) {}
                    } message: {
                        Text(model.texts.confirmClearBody)
                    }
                    Spacer()
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
            }
        }
    }

    private var maxEntriesBinding: Binding<Int> {
        Binding(
            get: { model.settings.historyMaxEntries },
            set: { value in model.patch { $0.historyMaxEntries = value } })
    }

    private var sortBinding: Binding<String> {
        Binding(
            get: { model.settings.historySort },
            set: { value in model.patch { $0.historySort = value } })
    }

    private func recordBinding(
        _ keyPath: WritableKeyPath<LanechoKit.Settings, Bool>
    ) -> Binding<Bool> {
        Binding(
            get: { model.settings[keyPath: keyPath] },
            set: { value in model.patch { $0[keyPath: keyPath] = value } })
    }
}

/// Hotkeys: rebinding the panel key / slot direct-paste / conflict list
struct HotkeysTab: View {
    @Bindable var model: SettingsModel

    var body: some View {
        SettingsPage {
            SettingsSection {
                SettingsRow(model.texts.panelHotkeyLabel) {
                    HotkeyRecorderField(
                        current: model.settings.panelHotkey, texts: model.texts
                    ) { accelerator in
                        model.changePanelHotkey(accelerator)
                    }
                }
            }
            SettingsSection(footer: model.texts.slotHotkeysNote) {
                SettingsToggleRow(label: model.texts.slotHotkeysLabel, isOn: slotBinding)
            }
            if !model.hotkeyFailures.isEmpty {
                SettingsSection {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(model.hotkeyFailures, id: \.self) { failure in
                            Label(
                                "\(model.texts.hotkeyConflict): \(HotkeyRecorderField.symbolize(failure))",
                                systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                            .font(.callout)
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                }
            }
        }
    }

    private var slotBinding: Binding<Bool> {
        Binding(
            get: { model.settings.slotHotkeys },
            set: { model.changeSlotHotkeys($0) })
    }
}
