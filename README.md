<p align="center">
  <img src="./assets/logo.svg" width="96" alt="lanecho logo" />
</p>

<h1 align="center">lanecho</h1>

<p align="center">
  Shared clipboard over LAN — copy on one device, paste on all of them.
</p>

<p align="center">
  <a href="https://github.com/zlx2019/lanecho/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/lanecho/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/lanecho/releases"><img src="https://img.shields.io/github/v/release/zlx2019/lanecho?include_prereleases" alt="Release" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-8b96ff" alt="Platform" />
</p>

<p align="center">
  <b>English</b> · <a href="./README.zh-CN.md">简体中文</a>
</p>

---

Every device running lanecho becomes a node on your LAN — no server, no account, no cloud. Pair two devices once; from then on, text you copy on one is instantly available to paste on the others, over a mutually-authenticated TLS 1.3 channel. A Maccy-style clipboard history lives one hotkey away.

Sibling project of [deskmate](https://github.com/zlx2019/deskmate) (LAN file & text sharing), built on the same discovery / identity / TLS architecture.

## Features

- **Zero-config discovery** — mDNS with a UDP multicast fallback; nearby devices just show up
- **Pair once, sync forever** — explicit two-way pairing with fingerprint short-codes, revoke anytime; unpaired devices are rejected at the protocol layer
- **Text sync** — byte-exact (no trimming, no escaping), last-write-wins across the group, echo-suppressed so nothing ever loops
- **Clipboard history** — floating panel on a global hotkey (default `Cmd/Ctrl+Shift+V`): search, pin, delete; `Alt+1..6` puts any of the panel's top six entries straight back on the clipboard, ready to paste; records text, images and file references with de-duplication. Hover an entry and a preview card shows the full content plus where and when it was copied
- **Password-manager aware** — clipboard entries marked as concealed (1Password, Keychain, Windows cloud-clipboard convention, …) are never recorded and never leave the machine
- **Secure by default** — TLS 1.3 mutual auth with certificate-pinned identities; sync only ever goes to devices you explicitly paired
- **Tray-native** — no dock icon, no window in your way: click the tray icon for the history panel, with sync pause, clear history and settings right at its foot; low-noise "synced from X" hints
- **Featherweight** — Tauri 2 + Rust, a few megabytes installed; macOS additionally has a native Swift/AppKit client with no web view

## Install

Grab the installer for your platform from [Releases](https://github.com/zlx2019/lanecho/releases):

| Platform | Artifact |
|---|---|
| macOS (Apple Silicon / Intel) | `lanecho_x.y.z_aarch64.dmg` / `lanecho_x.y.z_x64.dmg` |
| Windows | `lanecho_x.y.z_x64-setup.exe` (NSIS; registers the firewall rule for you) |
| Linux | `lanecho_x.y.z_amd64.AppImage` / `.deb` |
| macOS, native client | `lanecho-macos-native_x.y.z_universal.dmg` (macOS 14+) |

> Builds are currently unsigned. On macOS, right-click the app and choose **Open** the first time (or run `xattr -cr /Applications/lanecho.app`). Windows SmartScreen may ask for confirmation as well.

### Two macOS clients

macOS has a second, **native** client written in Swift and AppKit, with no
web view in the process. It is the same product: same protocol, same data
directory, same file formats — the two interoperate on the network and can
read each other's history, so you can switch between them freely.

Pick whichever you prefer. The native one adds paste-on-select (off by
default; needs Accessibility permission) and a panel that never takes
focus from the app you were working in. The cross-platform one is what
runs everywhere else and gets platform features first.

Run only one at a time on a given machine — they share the sync port, so
the second one to start will fail to bind it.

## Privacy

lanecho is local-first by design; still, a clipboard tool deserves explicit fine print:

- **Nothing leaves your LAN.** Sync traffic goes device-to-device over TLS 1.3, only to devices you paired. There is no server and no telemetry.
- **Only text is synced** (up to 512 KiB per entry). Images and files stay on the machine they were copied on — they appear in the local history only.
- **Concealed content is exempt.** Entries carrying the standard "concealed" clipboard markers (macOS `org.nspasteboard.ConcealedType`, Windows `ExcludeClipboardContentFromMonitorProcessing`) are neither recorded nor synced. On Linux these markers are not yet detected — pause syncing from the panel, or turn on incognito mode (Settings → Sync), when handling secrets there.
- **History is stored in plain files** (JSON index + PNG blobs) under your OS app-data directory, unencrypted. Anyone with access to your user account can read it. Cap the entry count (Settings → Storage), turn on incognito mode (Settings → Sync), or clear the history from the panel at any time.

## Develop

Prerequisites: **Rust ≥ 1.96**, **Node ≥ 22**, **pnpm**. On Linux you also need the Tauri system packages (`libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, ...).

```bash
git clone https://github.com/zlx2019/lanecho.git
cd lanecho/apps/desktop
pnpm install
pnpm tauri dev     # run the desktop app with hot reload
pnpm tauri build   # produce installers for the current platform
```

The engine is a UI-free Rust library (`deps/lanecho-core`) shared by the desktop app and a CLI (`deps/lanecho-cli`) that is handy for protocol debugging:

```bash
cargo run -p lanecho-cli -- listen --data-dir /tmp/le-a   # run a pairable node
cargo run -p lanecho-cli -- scan --data-dir /tmp/le-b     # list nearby devices
cargo run -p lanecho-cli -- watch                         # print clipboard changes
cargo nextest run --workspace                             # run the test suite
```

The native macOS client (`apps/macos`) is a plain SwiftPM package — no
Xcode project in the repo. Needs **Swift 6.1** / Xcode 16.4 and macOS 14+:

```bash
cd apps/macos/LanechoKit
swift run Lanecho --data-dir /tmp/le-mac   # run it against an isolated data dir
swift test                                 # run its test suite
cd .. && ./build-app.sh                    # produce lanecho.app (needs xcodegen)
```

It re-implements the protocol rather than binding to the Rust core, so CI
runs a set of interop tests against a real `lanecho-cli` process to keep
the two wire-compatible.

## FAQ

**macOS says the app is damaged / from an unidentified developer.**
The build is not notarized yet. Right-click → Open once, or clear the quarantine flag with `xattr -cr /Applications/lanecho.app`.

**Devices never show up on macOS.**
macOS 15+ asks for **Local Network** permission on first launch — it must be allowed, otherwise discovery fails silently. Re-enable it under System Settings → Privacy & Security → Local Network.

**Devices never show up on Windows.**
Discovery needs an inbound firewall rule. The NSIS installer registers it automatically; if you run a portable binary instead, allow `lanecho.exe` for private networks when Windows asks.

**What are the Linux limitations?**
Clipboard access goes through X11 (XWayland on Wayland compositors). On X11 sync works both ways; on Wayland, whether an unfocused app can read the clipboard depends on the compositor's XWayland bridging, so broadcasting your copies may not work reliably — receiving synced text is unaffected. Clipboard history records text only for now, and concealed-content markers are not detected on Linux.

**Can both macOS clients run at the same time?**
No. They are two clients of the same product and share the sync port, so
whichever starts second cannot bind it. Quit one before starting the
other — they read the same data directory, so nothing is lost either way.

**Can lanecho and deskmate run on the same machine?**
Yes — they use disjoint ports and service names (TCP 42524 / multicast 224.0.0.169:42525 / `_lanecho._tcp` vs. deskmate's 42424 / 224.0.0.168:42425 / `_deskmate._tcp`).

## License

[MIT](./LICENSE)
