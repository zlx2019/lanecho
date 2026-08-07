// The Ignore tab: four rule kinds behind a segmented switch (apps /
// pasteboard types / regex / file patterns), each with its own list editor
// and its own "don't sync" / "don't record" toggle pair.
//
// The list area has a fixed height so the window height stays stable across
// panes (the window is sized once from the tallest page and never resized on
// a tab switch — see SettingsWindowController).

import LanechoKit
import SwiftUI

/// Fixed height of the list / editor area
private let ignoreListHeight: CGFloat = 216

/// The four panes
private enum IgnorePane: String, CaseIterable {
    case apps, types, regex, files
}

/// Ignore rules tab
struct IgnoreTab: View {
    @Bindable var model: SettingsModel
    @State private var pane: IgnorePane = .apps
    /// Entry field for the types / regex panes
    @State private var draft = ""
    /// Local editor text for the file pane, committed on blur (a TextEditor
    /// writing straight into settings would hit the disk on every keystroke)
    @State private var fileText = ""
    @FocusState private var fileFocused: Bool

    var body: some View {
        SettingsPage {
            Picker("", selection: $pane) {
                Text(model.texts.ignorePaneApps).tag(IgnorePane.apps)
                Text(model.texts.ignorePaneTypes).tag(IgnorePane.types)
                Text(model.texts.ignorePaneRegex).tag(IgnorePane.regex)
                Text(model.texts.ignorePaneFiles).tag(IgnorePane.files)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .onChange(of: pane) { draft = "" }

            SettingsSection(footer: note) {
                switch pane {
                case .apps: appsPane
                case .types: listPane(items: model.settings.ignore.types, showReset: true) {
                        model.removeIgnoredType($0)
                    }
                case .regex: listPane(items: model.settings.ignore.regexes, showReset: false) {
                        model.removeIgnoredRegex($0)
                    }
                case .files: filesPane
                }
            }

            SettingsSection {
                SettingsCheckRow(model.texts.ignoreSuppressSync, isOn: toggleBinding(sync: true))
                SettingsDivider()
                SettingsCheckRow(
                    model.texts.ignoreSuppressRecord, isOn: toggleBinding(sync: false))
            }
        }
        .onAppear { fileText = model.settings.ignore.filePatterns }
        .onChange(of: fileFocused) {
            if !fileFocused { model.saveFilePatterns(fileText) }
        }
    }

    /// Footnote of the current pane
    private var note: String {
        switch pane {
        case .apps: model.texts.ignoreAppsNote
        case .types: model.texts.ignoreTypesNote
        case .regex: model.texts.ignoreRegexNote
        case .files: model.texts.ignoreFilesNote
        }
    }

    // MARK: - Applications

    private var appsPane: some View {
        VStack(spacing: 0) {
            scrollingList(isEmpty: model.settings.ignore.apps.isEmpty) {
                ForEach(model.settings.ignore.apps, id: \.id) { app in
                    entryRow {
                        HStack(spacing: 8) {
                            Image(nsImage: Self.appIcon(bundleId: app.id))
                                .resizable()
                                .frame(width: 18, height: 18)
                            Text(app.name)
                            Text(app.id)
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                        }
                    } onRemove: {
                        model.removeIgnoredApp(id: app.id)
                    }
                }
            }
            SettingsDivider()
            HStack {
                Button(model.texts.ignoreAddApp) { model.addIgnoredApp() }
                    .controlSize(.small)
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
        }
    }

    // MARK: - Types / regex (a plain string list plus an entry field)

    private func listPane(
        items: [String], showReset: Bool, onRemove: @escaping (String) -> Void
    ) -> some View {
        VStack(spacing: 0) {
            scrollingList(isEmpty: items.isEmpty) {
                ForEach(items, id: \.self) { item in
                    entryRow {
                        Text(item)
                            .font(.system(.body, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } onRemove: {
                        onRemove(item)
                    }
                }
            }
            SettingsDivider()
            HStack(spacing: 6) {
                TextField("", text: $draft)
                    .labelsHidden()
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .onSubmit(addDraft)
                Button(model.texts.ignoreAdd, action: addDraft)
                    .controlSize(.small)
                    .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                if showReset {
                    Button(model.texts.ignoreReset) { model.resetIgnoredTypes() }
                        .controlSize(.small)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
        }
    }

    /// Commit the entry field of the current pane
    private func addDraft() {
        switch pane {
        case .types: model.addIgnoredType(draft)
        case .regex: model.addIgnoredRegex(draft)
        default: break
        }
        draft = ""
    }

    // MARK: - Files (a gitignore-style text editor)

    private var filesPane: some View {
        TextEditor(text: $fileText)
            .font(.system(.body, design: .monospaced))
            .scrollContentBackground(.hidden)
            .focused($fileFocused)
            .frame(height: ignoreListHeight)
            .padding(6)
    }

    // MARK: - Shared pieces

    /// Fixed-height scroll area with an empty-state placeholder
    private func scrollingList(
        isEmpty: Bool, @ViewBuilder rows: () -> some View
    ) -> some View {
        Group {
            if isEmpty {
                Text(model.texts.ignoreEmpty)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        rows()
                    }
                }
            }
        }
        .frame(height: ignoreListHeight)
    }

    /// One list row: content on the left, a remove button on the right
    private func entryRow(
        @ViewBuilder content: () -> some View, onRemove: @escaping () -> Void
    ) -> some View {
        HStack {
            content()
            Spacer(minLength: 12)
            Button(action: onRemove) {
                Image(systemName: "minus.circle.fill")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
    }

    /// The toggle pair of the current pane (sync: true → the "don't sync"
    /// switch, false → "don't record")
    private func toggleBinding(sync: Bool) -> Binding<Bool> {
        let keyPath: WritableKeyPath<LanechoKit.Settings, Bool> =
            switch (pane, sync) {
            case (.apps, true): \.ignore.appsSync
            case (.apps, false): \.ignore.appsRecord
            case (.types, true): \.ignore.typesSync
            case (.types, false): \.ignore.typesRecord
            case (.regex, true): \.ignore.regexSync
            case (.regex, false): \.ignore.regexRecord
            case (.files, true): \.ignore.filesSync
            case (.files, false): \.ignore.filesRecord
            }
        return Binding(
            get: { model.settings[keyPath: keyPath] },
            set: { value in model.patch { $0[keyPath: keyPath] = value } })
    }

    /// Icon for a bundle identifier (generic when the app is gone)
    private static func appIcon(bundleId: String) -> NSImage {
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleId) {
            return NSWorkspace.shared.icon(forFile: url.path)
        }
        return NSWorkspace.shared.icon(for: .applicationBundle)
    }
}
