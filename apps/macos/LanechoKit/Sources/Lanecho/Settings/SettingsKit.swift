// Settings page building blocks: non-scrolling grouped cards that look like a
// grouped Form.
//
// Why not .formStyle(.grouped): it is a List scroll container and cannot report
// a real content height, so every page would need a hand-picked height
// constant → dead space the moment the content changes. Non-scrolling VStack
// cards have a real intrinsic height, so the window can size itself from
// fittingSize.

import SwiftUI

/// Grouped card (rounded background, hairline border, optional footnote)
struct SettingsSection<Content: View>: View {
    /// Footnote (optional)
    var footer: String?
    @ViewBuilder let content: Content

    init(footer: String? = nil, @ViewBuilder content: () -> Content) {
        self.footer = footer
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            VStack(spacing: 0) {
                content
            }
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color(nsColor: .textBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(Color.primary.opacity(0.08), lineWidth: 1)
            )
            if let footer {
                Text(footer)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
            }
        }
    }
}

/// One settings row (label on the left, control on the right)
struct SettingsRow<Control: View>: View {
    let label: String
    @ViewBuilder let control: Control

    init(_ label: String, @ViewBuilder control: () -> Control) {
        self.label = label
        self.control = control()
    }

    var body: some View {
        HStack {
            Text(label)
            Spacer(minLength: 20)
            control
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
    }
}

/// Separator between rows (inside a card, inset to line up with the labels)
struct SettingsDivider: View {
    var body: some View {
        Divider()
            .padding(.leading, 10)
    }
}

/// Toggle row (always the switch style; inside a bare VStack a Toggle defaults
/// to a checkbox, so it has to be set explicitly)
struct SettingsToggleRow: View {
    let label: String
    let isOn: Binding<Bool>

    var body: some View {
        SettingsRow(label) {
            Toggle("", isOn: isOn)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
        }
    }
}

/// Checkbox row (checkboxes carry multi-select semantics; laid out like every
/// other row, label left and control right — putting the checkbox at the head
/// of the row fights the rhythm of the toggle and text-field rows and reads
/// like a second UI pasted in)
struct SettingsCheckRow: View {
    let label: String
    let isOn: Binding<Bool>

    init(_ label: String, isOn: Binding<Bool>) {
        self.label = label
        self.isOn = isOn
    }

    var body: some View {
        SettingsRow(label) {
            Toggle("", isOn: isOn)
                .labelsHidden()
                .toggleStyle(.checkbox)
        }
    }
}
