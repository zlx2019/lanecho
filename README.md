<h1 align="center">lanecho</h1>

<p align="center">
  Shared clipboard over LAN — copy on one device, paste on all of them.
</p>

---

> **Status: early scaffolding.** Design is being reviewed; no usable build yet.
> Sibling project of [deskmate](https://github.com/zlx2019/deskmate) (LAN file
> & text sharing), built on the same discovery / identity / TLS architecture.

Every device running lanecho becomes a node on your LAN — no server, no
account, no cloud. Pair two devices once; from then on, whatever you copy on
one is instantly available to paste on the others, over a mutually-
authenticated TLS 1.3 channel.

## Planned for v1

- **Zero-config discovery** — mDNS with a UDP multicast fallback
- **Pair once, sync forever** — explicit two-way pairing per device, revoke anytime
- **Text sync** — byte-exact, last-write-wins across the group
- **Secure by default** — TLS 1.3 mutual auth, certificate-pinned identities;
  password-manager entries (concealed clipboard content) are never broadcast
- **Tray-native** — lives in the system tray, one-click pause, low-noise
  "synced from X" hints

## License

[MIT](./LICENSE)
