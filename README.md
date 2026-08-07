<p align="center">
  <img src="./assets/logo.svg" width="96" alt="Lanecho logo" />
</p>

<h1 align="center">Lanecho</h1>

<p align="center">
  Shared clipboard history over your LAN — copy on one device, paste on all of them.
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

Every device running Lanecho becomes a node on the local network. Nodes can pair with each other to form smaller groups, and the nodes in each group share their clipboards.

Pair two devices once. From then on, whatever you copy on one is ready to paste on the others, carried device-to-device over a mutually-authenticated TLS 1.3 channel. Everything you copied recently stays in a searchable history, one hotkey away.

## ✨ Features

- 🔗 **LAN P2P** — data travels directly between devices, with no server or cloud required.
- ⚡ **Instant sync** — copied data is immediately synced to the clipboards of other devices and stored in history. Each copy propagates only once.
- 📋 **Clipboard history** — open the history panel with a hotkey (default `Cmd/Ctrl+Shift+C`) and quickly select previously copied content. Use `Cmd/Ctrl+1..6` (modifier configurable) to put any of the top six entries straight back on the clipboard. Hover an entry to view its details.
- 🖼️ **Multiple content types** — record and sync text, screenshots, and files.
- 📡 **Zero-config discovery** — mDNS with a UDP multicast fallback; nearby devices just show up, on any subnet your LAN routes.
- 🤝 **Pair once** — explicitly pair both devices once; the pairing remains valid until either side removes it.
- ↔️ **Directional sync** — set this client to two-way, send-only, receive-only, or off. The selected direction applies to all of its paired devices.
- 🔐 **Secure by default** — TLS 1.3 mutual authentication with certificate-pinned identities; sync only ever reaches devices you explicitly paired.
- 🛡️ **Password-manager aware** — entries carrying the standard "concealed" clipboard markers (1Password, Keychain, the Windows cloud-clipboard convention, …) are never recorded and never leave the machine.
- 🍎 **Native macOS client** — a native macOS client built with Swift and AppKit.

### What travels between devices

| Content | Cap | Notes |
|---|---|---|
| Text | 512 KiB | Transferred byte-for-byte to preserve the content |
| Images | 16 MiB | Streamed out of band, so a large screenshot never stalls the channel |
| Files | Up to 64 files per copy | Streamed the same way; the files themselves are transferred, not just their paths |

## 📥 Install

Grab a build from [Releases](https://github.com/zlx2019/lanecho/releases).

### 🍎 macOS

**Use the native client.** It is written in Swift and AppKit with no web view in the process — leaner in memory, and it behaves the way a Mac app should: the panel never steals focus from whatever you were typing in, and it can paste for you the moment you pick an entry.

| Build | Requires | Artifact |
|---|---|---|
| **Native — recommended** | macOS 14 Sonoma or later | `Lanecho-x.y.z-macos-native-universal.dmg` |
| Cross-platform | macOS 13 or earlier | `Lanecho-x.y.z-macos-aarch64.dmg` / `Lanecho-x.y.z-macos-x64.dmg` |

Reach for the cross-platform build on a Mac only when the native one will not run on your system version. Both speak the same protocol and read the same data directory, so either one syncs with every other device on your LAN, and you can switch between them without losing history or pairings.

### 🪟 Windows

`Lanecho-x.y.z-windows-x64-setup.exe` — the NSIS installer registers the inbound firewall rule discovery needs.

### 🐧 Linux

`Lanecho-x.y.z-linux-amd64.AppImage` or `Lanecho-x.y.z-linux-amd64.deb`. See the FAQ for what is and is not supported there.

> Builds are currently unsigned. On macOS, right-click the app and choose **Open** the first time (or run `xattr -cr /Applications/Lanecho.app`). Windows SmartScreen may ask for confirmation as well.

## 🔒 Privacy

Lanecho is local-first by design; still, a clipboard tool deserves explicit fine print:

- **Nothing leaves your LAN.** Sync traffic goes device-to-device over TLS 1.3, only to devices you paired. There is no server and no telemetry.
- **Concealed content is exempt.** Entries carrying the standard markers (macOS `org.nspasteboard.ConcealedType`, Windows `ExcludeClipboardContentFromMonitorProcessing`) are neither recorded nor synced. On Linux these markers are not detected — pause syncing from the panel, or turn on incognito mode, when handling secrets there.
- **History is stored in plain files** (a JSON index plus image blobs) under your OS app-data directory, unencrypted. Anyone with access to your user account can read it. Cap the entry count, turn on incognito mode, or clear the history from the panel at any time.
- **Recording and syncing are independent.** Text, images and files each have separate recording and sync switches; this client's direction mode applies to all of its paired devices.

## ❓ FAQ

**macOS says the app is damaged / from an unidentified developer.**
The build is not notarized yet. Right-click → Open once, or clear the quarantine flag with `xattr -cr /Applications/Lanecho.app`.

**Devices never show up on macOS.**
macOS 15+ asks for **Local Network** permission on first launch — it must be allowed, otherwise discovery fails silently. Re-enable it under System Settings → Privacy & Security → Local Network.

**Devices never show up on Windows.**
Discovery needs an inbound firewall rule. The NSIS installer registers it automatically; if you run a portable binary instead, allow `Lanecho.exe` for private networks when Windows asks.

**A device I unplugged is still listed.**
A node that crashes or drops off the network takes up to about 30 seconds to disappear, because it never got to say goodbye and its mDNS record lingers in the cache. Quitting normally removes it at once. The timeouts are deliberately not more aggressive — Wi-Fi hiccups and wake-from-sleep would otherwise make the device list flicker.

**What are the Linux limitations?**
Clipboard access goes through X11 (XWayland on Wayland compositors). Text sync works both ways on X11; on Wayland, whether an unfocused app can read the clipboard depends on the compositor's XWayland bridging, so sending local copies may not work reliably. The Linux watcher currently detects locally copied text only: local image and file copies are neither recorded nor sent. Incoming images and files are still handled when the X11/XWayland clipboard backend accepts them. Concealed-content markers are not detected.

**Can both macOS builds run at the same time?**
They share the sync port by default, so whichever starts second cannot bind it. Quit one before starting the other — they read the same data directory, so nothing is lost either way.

## 🔨 Build from source

The native macOS client is a plain SwiftPM package (`apps/macos`); the cross-platform client is Tauri 2 + Rust (`apps/desktop`), on a shared UI-free engine (`deps/lanecho-core`).

```bash
cd apps/macos   && ./build-app.sh                    # Lanecho.app
cd apps/desktop && pnpm install && pnpm tauri build  # installer for the current platform
```

## 📄 License

[MIT](./LICENSE)
