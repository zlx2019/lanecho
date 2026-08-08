// The Ignore tab: four rule kinds behind a segmented switch (apps /
// pasteboard types / regex / file patterns).
//
// Layout follows the classic preferences list editor (the Maccy reference):
// a selectable list with hairline separators, a +/− button pair on the
// bottom edge (− acts on the selected row), and the two suppression toggles
// as a single row of small checkboxes shared by all four panes.
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
    /// Selected row (apps select by bundle id, types/regex by the value)
    @State private var selection: String?
    /// Inline entry row (types/regex panes): + appends an editable row at the
    /// list tail, return commits it, escape cancels
    @State private var adding = false
    /// Text of the inline entry row
    @State private var draft = ""
    @FocusState private var draftFocused: Bool
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
            .onChange(of: pane) {
                adding = false
                draft = ""
                selection = nil
            }

            SettingsSection(footer: note) {
                switch pane {
                case .apps: appsPane
                case .types: listPane(items: model.settings.ignore.types, showReset: true)
                case .regex: listPane(items: model.settings.ignore.regexes, showReset: false)
                case .files: filesPane
                }
            }

            // The suppression pair as small checkboxes on one shared row:
            // two long toggle rows for two booleans reads as pure padding
            SettingsSection {
                HStack(spacing: 24) {
                    Toggle(model.texts.ignoreSuppressSync, isOn: toggleBinding(sync: true))
                    Toggle(model.texts.ignoreSuppressRecord, isOn: toggleBinding(sync: false))
                    Spacer()
                }
                .toggleStyle(.checkbox)
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
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
                ForEach(Array(model.settings.ignore.apps.enumerated()), id: \.element.id) {
                    index, app in
                    if index > 0 {
                        SettingsDivider()
                    }
                    selectableRow(id: app.id) {
                        HStack(spacing: 8) {
                            Image(nsImage: Self.appIcon(bundleId: app.id))
                                .resizable()
                                .frame(width: 18, height: 18)
                            Text(app.name)
                            Text(app.id)
                                .font(.caption)
                                .opacity(0.6)
                                .lineLimit(1)
                        }
                    }
                }
            }
            SettingsDivider()
            HStack {
                addRemoveButtons
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
    }

    // MARK: - Types / regex (a plain string list plus an entry field)

    private func listPane(items: [String], showReset: Bool) -> some View {
        VStack(spacing: 0) {
            scrollingList(isEmpty: items.isEmpty && !adding, revealTail: adding) {
                ForEach(Array(items.enumerated()), id: \.element) { index, item in
                    if index > 0 {
                        SettingsDivider()
                    }
                    selectableRow(id: item) {
                        Text(item)
                            .font(.system(.body, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                if adding {
                    if !items.isEmpty {
                        SettingsDivider()
                    }
                    draftRow
                }
            }
            SettingsDivider()
            HStack(spacing: 8) {
                addRemoveButtons
                Spacer()
                if showReset {
                    Button(model.texts.ignoreReset) { model.resetIgnoredTypes() }
                        .controlSize(.small)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
    }

    /// The inline entry row appended at the list tail while adding
    private var draftRow: some View {
        TextField("", text: $draft)
            .labelsHidden()
            .textFieldStyle(.plain)
            .font(.system(.body, design: .monospaced))
            .focused($draftFocused)
            .onSubmit(commitDraft)
            .onExitCommand {
                adding = false
                draft = ""
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .onAppear { draftFocused = true }
            .onChange(of: draftFocused) {
                // Focus moving elsewhere commits what was typed (return
                // semantics); escape already cleared `adding`, so a cancel
                // never falls through to here
                if !draftFocused && adding {
                    commitDraft()
                }
            }
    }

    /// Commit the inline entry row (an empty draft just closes it)
    private func commitDraft() {
        let value = draft.trimmingCharacters(in: .whitespaces)
        if !value.isEmpty {
            switch pane {
            case .types: model.addIgnoredType(value)
            case .regex: model.addIgnoredRegex(value)
            default: break
            }
        }
        draft = ""
        adding = false
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

    /// The +/− pair on the bottom edge (the classic list-editor control):
    /// + adds (open panel / inline entry row), − removes the selection
    private var addRemoveButtons: some View {
        HStack(spacing: 0) {
            Button(action: addAction) {
                Image(systemName: "plus")
                    .frame(width: 24, height: 18)
                    .contentShape(Rectangle())
            }
            Divider()
                .frame(height: 12)
            Button(action: removeSelected) {
                Image(systemName: "minus")
                    .frame(width: 24, height: 18)
                    .contentShape(Rectangle())
            }
            .disabled(selection == nil)
        }
        .buttonStyle(.borderless)
        .imageScale(.small)
        .overlay(
            RoundedRectangle(cornerRadius: 5)
                .stroke(Color.primary.opacity(0.15), lineWidth: 1)
        )
    }

    /// Add for the current pane
    private func addAction() {
        switch pane {
        case .apps:
            model.addIgnoredApp()
        case .types, .regex:
            // Open the inline entry row; a second press just refocuses it
            if adding {
                draftFocused = true
            } else {
                adding = true
            }
        case .files:
            break
        }
    }

    /// Remove the selected row of the current pane
    private func removeSelected() {
        guard let selected = selection else { return }
        switch pane {
        case .apps: model.removeIgnoredApp(id: selected)
        case .types: model.removeIgnoredType(selected)
        case .regex: model.removeIgnoredRegex(selected)
        case .files: break
        }
        selection = nil
    }

    /// Fixed-height scroll area with an empty-state placeholder; a click on
    /// the blank area clears the selection. `revealTail` scrolls the list
    /// tail into view (where the inline entry row appears).
    private func scrollingList(
        isEmpty: Bool, revealTail: Bool = false, @ViewBuilder rows: () -> some View
    ) -> some View {
        // Materialized before the ScrollViewReader closure: the builder
        // parameter is non-escaping and the reader's content escapes
        let content = rows()
        return Group {
            if isEmpty {
                Text(model.texts.ignoreEmpty)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(spacing: 0) {
                            content
                            Color.clear.frame(height: 0).id("ignore-list-tail")
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { selection = nil }
                    .onChange(of: revealTail) {
                        if revealTail {
                            proxy.scrollTo("ignore-list-tail")
                        }
                    }
                }
            }
        }
        .frame(height: ignoreListHeight)
    }

    /// One selectable list row (menu-style full-row highlight, white text
    /// while selected)
    private func selectableRow(
        id: String, @ViewBuilder content: () -> some View
    ) -> some View {
        let selected = selection == id
        return HStack {
            content()
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .foregroundStyle(selected ? Color.white : Color.primary)
        .background(
            selected
                ? Color(nsColor: .selectedContentBackgroundColor) : Color.clear
        )
        .contentShape(Rectangle())
        .onTapGesture { selection = id }
    }

    /// The toggle pair of the current pane (sync: true → the "don't sync"
    /// checkbox, false → "don't record")
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
