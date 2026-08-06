# Security Policy

## Supported Versions

Security updates are provided only for the latest released version.

| Version | Supported |
|---------|-----------|
| Latest release | Yes |
| Historical versions | No |

## Reporting A Vulnerability

If you discover a security vulnerability, please do **not** report it through a public issue, because doing so may expose the vulnerability before a fix is available.

Please report it privately by email: **zero9501@outlook.com**

When reporting, include as much of the following information as possible:

- Vulnerability type and impact scope
- Reproduction steps or proof of concept
- Affected versions and which client (cross-platform or native macOS)

We will confirm the report as soon as possible and disclose it publicly after a fix has been released.

## Scope

Lanecho handles clipboard contents, which routinely include passwords, tokens and other secrets. Findings in these areas are especially welcome:

- **Pairing and authentication** — anything that lets an unpaired device be accepted, or that lets one device impersonate another (identities are certificate-pinned; the fingerprint is derived from the certificate)
- **Transport** — weaknesses in the TLS 1.3 mutual authentication, or any path where clipboard data crosses the network without it
- **Concealed-content handling** — cases where an entry marked concealed by a password manager still gets recorded to history or broadcast to peers
- **Protocol parsing** — anything remotely reachable that can crash, hang or exhaust the resources of a node on the same LAN
- **History at rest** — unintended exposure of the local history beyond what the README's Privacy section already documents

Both clients implement the same protocol independently (Rust for the cross-platform client, Swift for the native macOS one), so please note which one you tested — a flaw in one is not automatically present in the other.

## Known Limitations

These are documented behaviours rather than vulnerabilities, but they define the security boundary:

- **History is stored unencrypted** in plain files under the OS app-data directory. Anyone with access to your user account can read it.
- **Concealed-content markers are not detected on Linux.** Use incognito mode or pause syncing when handling secrets there.
- **Anyone on the LAN can reach the sync port.** Unpaired devices are rejected after the TLS handshake, at the application layer.
- **Release builds are not yet notarized** and are signed ad-hoc, so their authenticity cannot be verified through the OS.
