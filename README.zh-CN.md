<p align="center">
  <img src="./assets/logo.svg" width="96" alt="Lanecho logo" />
</p>

<h1 align="center">Lanecho</h1>

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

每台运行 Lanecho 的设备，都会成为你自己网络里的一个节点。**没有服务端，不用注册账号，不经过云端** —— 不需要信任任何人，只需要信任你眼前的这几台机器。

两台设备只配对一次。此后在其中一台上复制的东西，就能直接在其余设备上粘贴，全程在设备之间点对点传输，走双向认证的 TLS 1.3 通道。最近复制过的内容都留在一份可搜索的历史里，一个快捷键就能唤起。

## ✨ 功能特性

- 🔗 **局域网内点对点** —— 设备之间直接对话。没有服务端要搭，没有账号要注册，也没有一个字节经由别人的云。
- ⚡ **即时同步** —— 在一台机器上复制，内容就已经躺在其余设备的剪贴板里，逐字节一致。组内以最后写入者为准，并有回声抑制：一次复制只传播一次，绝不会来回打转。
- 📋 **剪贴板历史** —— 最近复制过的一切，一个快捷键就在眼前（默认 `Cmd/Ctrl+Shift+V`）：搜索、置顶想留的、删掉不想要的。`Alt+1..6` 可把前六条里的任意一条直接放回剪贴板。鼠标悬停在条目上，详情卡会展示完整内容，以及它是何时、由哪个应用复制的。
- 🖼️ **文本、图片与文件** —— 一张截图、一批文件，和一行文本走的是同一条路；大块内容带外流式传输，不会把通道卡住。
- 📡 **零配置发现** —— mDNS 为主、UDP 组播兜底；只要局域网路由得到，附近的设备自己就会出现。
- 🤝 **配对一次，长期有效** —— 双向显式配对，核对指纹短码，随时可解除。未配对的设备在协议层就被拒绝，而不是靠某个开关拦着。
- ↔️ **同步方向** —— 可将当前客户端设为双向、只发送、只接收或关闭；所选方向统一作用于它的所有已配对设备。
- 🔐 **默认安全** —— TLS 1.3 双向认证，设备身份以证书指纹绑定；同步只会发往你明确配对过的设备。
- 🛡️ **识别密码管理器** —— 带有标准「敏感」剪贴板标记的条目（1Password、钥匙串、Windows 云剪贴板的约定标记等）既不会被记录，也不会离开本机。
- 🍎 **macOS 上有原生版** —— 用 Swift 与 AppKit 写成、进程里没有 WebView 的客户端，与跨平台版并行提供。
- 📍 **不挡道** —— 没有 Dock 图标，也不会有窗口跳出来：托盘图标就是全部的界面入口；你闲着的时候它也闲着。

### 设备之间会传些什么

| 内容 | 单条上限 | 说明 |
|---|---|---|
| 文本 | 512 KiB | 逐字节一致 —— 不 trim，也不重新编码 |
| 图片 | 16 MiB | 走带外流式传输，大截图不会把通道卡住 |
| 文件 | 单次 64 个 | 同样是流式传输；传的是文件本身，不是路径 |

组内状态以最后写入者为准，并有回声抑制：一次复制只传播一次，绝不会来回打转。

## 📥 安装

到 [Releases](https://github.com/zlx2019/lanecho/releases) 下载对应平台的包。

### 🍎 macOS

**请用原生版。** 它用 Swift 与 AppKit 写成，进程里没有 WebView —— 内存占用更小，行为也更像一个 Mac 应用：面板不会从你正在打字的地方夺走焦点，选中条目后还能顺手替你粘贴。

| 版本 | 系统要求 | 安装包 |
|---|---|---|
| **原生版 —— 推荐** | macOS 14 Sonoma 及以上 | `Lanecho-macos-native_x.y.z_universal.dmg` |
| 跨平台版 | macOS 13 及更早 | `Lanecho_x.y.z_aarch64.dmg` / `Lanecho_x.y.z_x64.dmg` |

在 Mac 上只有当系统版本跑不了原生版时，才需要退回跨平台版。两者说的是同一套协议、读的是同一份数据目录，所以无论装哪个都能和局域网里的其他设备同步，换着用也不会丢历史与配对关系。

### 🪟 Windows

`Lanecho_x.y.z_x64-setup.exe` —— NSIS 安装包会自动注册设备发现所需的入站防火墙规则。

### 🐧 Linux

`Lanecho_x.y.z_amd64.AppImage` 或 `.deb`。该平台的支持范围见常见问题。

> 目前的构建尚未签名。macOS 首次打开时请右键选择**打开**（或执行 `xattr -cr /Applications/Lanecho.app`）；Windows 的 SmartScreen 也可能弹出确认提示。

## 🔒 隐私

Lanecho 从设计上就是本地优先的；不过剪贴板工具理应把细则讲明白：

- **数据不出你的局域网。** 同步流量在设备之间点对点传输，走 TLS 1.3，且只发往你配对过的设备。没有服务端，也没有任何遥测。
- **敏感内容豁免。** 带有标准标记的条目（macOS 的 `org.nspasteboard.ConcealedType`、Windows 的 `ExcludeClipboardContentFromMonitorProcessing`）既不会被记录，也不会被同步。Linux 上无法识别这些标记 —— 在该平台处理敏感信息时，请从面板暂停同步，或开启无痕模式。
- **历史以明文文件存放**（JSON 索引 + 图像文件），位于操作系统的应用数据目录下，未加密。任何能访问你用户账号的人都能读到它。你可以限制记录条数、开启无痕模式，或随时从面板清空历史。
- **记录与同步彼此独立。** 文本、图像和文件分别有记录开关与同步开关；当前客户端的同步方向统一作用于它的所有已配对设备。

## ❓ 常见问题

**macOS 提示应用已损坏 / 来自身份不明的开发者。**
当前构建尚未公证。右键 → 打开一次即可，或用 `xattr -cr /Applications/Lanecho.app` 清除隔离标记。

**macOS 上始终看不到其他设备。**
macOS 15+ 会在首次启动时申请**本地网络**权限 —— 必须允许，否则设备发现会静默失败。可在「系统设置 → 隐私与安全性 → 本地网络」中重新开启。

**Windows 上始终看不到其他设备。**
设备发现需要一条入站防火墙规则。NSIS 安装包会自动注册；若你运行的是免安装版本，请在 Windows 询问时允许 `Lanecho.exe` 在专用网络下通信。

**已经拔网线的设备还挂在列表上。**
崩溃或断网的节点最多约 30 秒才会消失 —— 它没机会道别，mDNS 记录也还滞留在缓存里。正常退出则会立刻下线。这些超时是刻意不调激进的：否则 Wi-Fi 瞬时丢包和睡眠唤醒会让设备列表不停闪烁。

**Linux 上有哪些限制？**
剪贴板访问经由 X11（Wayland 合成器下走 XWayland）。文本同步在 X11 上可双向工作；而在 Wayland 下，未获焦点的应用能否读取剪贴板取决于合成器的 XWayland 桥接实现，因此发送本机复制的内容可能并不可靠。Linux 监听器目前只检测本机复制的文本：本机复制的图像和文件既不会记录，也不会发出。收到的图像和文件仍会处理，但能否写入剪贴板取决于 X11/XWayland 后端的支持。Linux 上也无法识别敏感内容标记。

**两个 macOS 版本能同时运行吗？**
默认端口相同，后启动的那个会绑定失败。换着用时先退掉另一个即可 —— 两者读同一份数据目录，历史与配对都不会丢。

## 🔨 从源码构建

macOS 原生版是一个纯 SwiftPM 包（`apps/macos`）；跨平台版是 Tauri 2 + Rust（`apps/desktop`），两者共用一个无 UI 的引擎（`deps/lanecho-core`）。

```bash
cd apps/macos   && ./build-app.sh                    # 打包 Lanecho.app
cd apps/desktop && pnpm install && pnpm tauri build  # 构建当前平台的安装包
```

## 📄 许可证

[MIT](./LICENSE)
