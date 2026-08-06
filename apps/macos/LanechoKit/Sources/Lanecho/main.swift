// lanecho menu bar app entry point.
//
// Always an accessory app: no Dock icon, absent from ⌘Tab. Every entry point
// is either the menu bar icon or a global hotkey.

import AppKit

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
// An unbundled build has no icon in its bundle, so set it once explicitly:
// system UI such as the pair request window reads it from here
if let icon = Assets.appIcon {
    app.applicationIconImage = icon
}
app.run()
