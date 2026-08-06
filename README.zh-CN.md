<p align="center">
  <img src="./assets/logo.svg" width="96" alt="lanecho logo" />
</p>

<h1 align="center">lanecho</h1>

<p align="center">
  局域网共享剪贴板 —— 一台设备上复制，所有设备上粘贴。
</p>

<p align="center">
  <a href="https://github.com/zlx2019/lanecho/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/lanecho/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/lanecho/releases"><img src="https://img.shields.io/github/v/release/zlx2019/lanecho?include_prereleases" alt="Release" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-8b96ff" alt="Platform" />
</p>

<p align="center">
  <a href="./README.md">English</a> · <b>简体中文</b>
</p>

---

每台运行 lanecho 的设备都会成为你局域网中的一个节点 —— 没有服务端，不用注册账号，不经过云端。两台设备只需配对一次，此后在其中一台上复制的文本，就能立即在其余设备上粘贴，全程走双向认证的 TLS 1.3 通道。一个 Maccy 式的剪贴板历史面板，随时一个快捷键唤起。

[deskmate](https://github.com/zlx2019/deskmate)（局域网文件与文本共享）的姊妹项目，构建在同一套发现 / 身份 / TLS 架构之上。

## 功能特性

- **零配置发现** —— mDNS 为主、UDP 组播兜底；附近的设备自己就会出现
- **配对一次，长期有效** —— 双向显式配对并核对指纹短码，随时可解除；未配对的设备在协议层直接拒绝
- **文本同步** —— 逐字节一致（不 trim、不转义），组内以最后写入者为准，并有回声抑制，内容绝不会来回打转
- **剪贴板历史** —— 全局快捷键（默认 `Cmd/Ctrl+Shift+V`）唤出浮窗面板：搜索、置顶、删除；`Alt+1..6` 可把面板中前六条里的任意一条直接放回剪贴板，随手即可粘贴；文本、图片与文件引用都会记录并自动去重。鼠标悬停在条目上，详情卡会展示完整内容，以及它是在何时、由哪个应用复制的
- **识别密码管理器** —— 被标记为敏感的剪贴板条目（1Password、钥匙串、Windows 云剪贴板的约定标记等）既不会被记录，也不会离开本机
- **默认安全** —— TLS 1.3 双向认证，设备身份以证书指纹绑定；同步只会发往你明确配对过的设备
- **常驻托盘** —— 没有 Dock 图标，也不会有窗口挡道：点击托盘图标即唤出历史面板，暂停同步、清空历史与偏好设置都在面板底部；「已从 X 同步」的提示保持克制，不喧宾夺主
- **轻量** —— Tauri 2 + Rust，安装后仅数 MB；macOS 另有一个无 WebView 的 Swift/AppKit 原生客户端

## 安装

到 [Releases](https://github.com/zlx2019/lanecho/releases) 下载对应平台的安装包：

| 平台 | 安装包 |
|---|---|
| macOS（Apple Silicon / Intel） | `lanecho_x.y.z_aarch64.dmg` / `lanecho_x.y.z_x64.dmg` |
| Windows | `lanecho_x.y.z_x64-setup.exe`（NSIS 安装包，会自动注册防火墙规则） |
| Linux | `lanecho_x.y.z_amd64.AppImage` / `.deb` |
| macOS 原生版 | `lanecho-macos-native_x.y.z_universal.dmg`（需 macOS 14+） |

> 目前的构建尚未签名。macOS 首次打开时请右键选择**打开**（或执行 `xattr -cr /Applications/lanecho.app`）；Windows 的 SmartScreen 也可能弹出确认提示。

### 两个 macOS 客户端

macOS 另有一个用 Swift 与 AppKit 写的**原生**客户端，进程内没有 WebView。
它与跨平台版是同一个产品：协议、数据目录、文件格式完全一致 —— 两者在网络上
可以互通，也能读对方的历史，随时换着用都行。

按喜好选一个即可。原生版多了「选中即自动粘贴」（默认关闭，需辅助功能权限）
与一个永不夺走当前应用焦点的面板；跨平台版则覆盖其余系统，新特性通常先在
它上面落地。

同一台机器上只跑其中一个 —— 两者共用同步端口，后启动的那个会绑定失败。

## 隐私

lanecho 从设计上就是本地优先的；不过剪贴板工具理应把细则讲明白：

- **数据不出你的局域网。** 同步流量在设备之间点对点传输，走 TLS 1.3，且只发往你配对过的设备。没有服务端，也没有任何遥测。
- **只有文本会被同步**（单条上限 512 KiB）。图片和文件留在复制它们的那台机器上 —— 只会出现在本机的历史记录里。
- **敏感内容豁免。** 带有标准「敏感」剪贴板标记的条目（macOS 的 `org.nspasteboard.ConcealedType`、Windows 的 `ExcludeClipboardContentFromMonitorProcessing`）既不会被记录，也不会被同步。Linux 上尚未支持识别这些标记 —— 在该平台处理敏感信息时，请从面板暂停同步，或开启无痕模式（设置 → 同步）。
- **历史以明文文件存放**（JSON 索引 + PNG 图像文件），位于操作系统的应用数据目录下，未加密。任何能访问你用户账号的人都能读到它。你可以限制记录条数（设置 → 存储）、开启无痕模式（设置 → 同步），或随时从面板清空历史。

## 开发

环境要求：**Rust ≥ 1.96**、**Node ≥ 22**、**pnpm**。Linux 上还需要 Tauri 的系统依赖（`libwebkit2gtk-4.1-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev` 等）。

```bash
git clone https://github.com/zlx2019/lanecho.git
cd lanecho/apps/desktop
pnpm install
pnpm tauri dev     # 启动桌面应用（带热重载）
pnpm tauri build   # 构建当前平台的安装包
```

引擎是一个无 UI 的 Rust 库（`deps/lanecho-core`），由桌面应用与一个便于调试协议的 CLI（`deps/lanecho-cli`）共用：

```bash
cargo run -p lanecho-cli -- listen --data-dir /tmp/le-a   # 启动一个可被配对的节点
cargo run -p lanecho-cli -- scan --data-dir /tmp/le-b     # 列出附近的设备
cargo run -p lanecho-cli -- watch                         # 打印剪贴板变化
cargo nextest run --workspace                             # 运行测试
```

macOS 原生客户端（`apps/macos`）是一个纯 SwiftPM 包，仓库里不含 Xcode 工程。
需要 **Swift 6.1** / Xcode 16.4 与 macOS 14+：

```bash
cd apps/macos/LanechoKit
swift run Lanecho --data-dir /tmp/le-mac   # 用隔离的数据目录跑起来
swift test                                 # 运行它的测试
cd .. && ./build-app.sh                    # 打包 lanecho.app（需 xcodegen）
```

它是把协议重新实现了一遍，而不是绑定 Rust 核心库，所以 CI 会拿一个真实的
`lanecho-cli` 进程跑互通测试，钉住两边的 wire 兼容性。

## 常见问题

**macOS 提示应用已损坏 / 来自身份不明的开发者。**
当前构建尚未公证。右键 → 打开一次即可，或用 `xattr -cr /Applications/lanecho.app` 清除隔离标记。

**macOS 上始终看不到其他设备。**
macOS 15+ 会在首次启动时申请**本地网络**权限 —— 必须允许，否则设备发现会静默失败。可在「系统设置 → 隐私与安全性 → 本地网络」中重新开启。

**Windows 上始终看不到其他设备。**
设备发现需要一条入站防火墙规则。NSIS 安装包会自动注册；若你运行的是免安装版本，请在 Windows 询问时允许 `lanecho.exe` 在专用网络下通信。

**Linux 上有哪些限制？**
剪贴板访问经由 X11（Wayland 合成器下走 XWayland）。X11 上双向同步正常；而在 Wayland 下，未获焦点的应用能否读取剪贴板取决于合成器的 XWayland 桥接实现，因此把本机复制的内容广播出去可能并不可靠 —— 接收同步过来的文本则不受影响。剪贴板历史目前只记录文本，且 Linux 上无法识别敏感内容标记。

**两个 macOS 客户端能同时运行吗？**
不能。它们是同一产品的两种客户端，共用同步端口，后启动的那个绑定不上。
换着用时先退掉另一个即可 —— 两者读同一份数据目录，历史与配对都不会丢。

**lanecho 与 deskmate 能装在同一台机器上吗？**
可以 —— 两者的端口与服务名完全错开（TCP 42524 / 组播 224.0.0.169:42525 / `_lanecho._tcp`，而 deskmate 是 42424 / 224.0.0.168:42425 / `_deskmate._tcp`）。

## 许可证

[MIT](./LICENSE)
