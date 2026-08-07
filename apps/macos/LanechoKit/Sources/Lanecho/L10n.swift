// UI strings in Chinese and English, following the two-table pattern of the
// Tauri client's locale.rs.
//
// The language comes from settings.language ("zh"/"en"; empty follows the
// system). A new string has to be added to both tables — miss one and it is a
// compile error, since no field of the struct has a default.

import Foundation
import LanechoKit

/// One table of UI strings
struct Texts: Sendable {
    let searchPlaceholder: String
    let emptyHistory: String
    let emptySearch: String
    let quit: String
    let settings: String
    let about: String
    // About window
    let aboutTitle: String
    let aboutTagline: String
    let aboutVersionLabel: String
    let aboutHomepage: String
    let aboutIssues: String
    let aboutReleases: String
    let aboutLicense: String
    let aboutBuiltWith: String
    let aboutCopyright: String
    let startFailedTitle: String
    let syncLabel: String
    let clearHistory: String
    let confirmClearTitle: String
    let confirmClearBody: String
    let clearConfirm: String
    let cancel: String
    let pin: String
    let unpin: String
    let deleteEntry: String
    // Settings window
    let settingsTitle: String
    let tabGeneral: String
    let tabDevices: String
    let tabSync: String
    let tabStorage: String
    let tabHotkeys: String
    let tabIgnore: String
    let ignorePaneApps: String
    let ignorePaneTypes: String
    let ignorePaneRegex: String
    let ignorePaneFiles: String
    let ignoreAppsNote: String
    let ignoreTypesNote: String
    let ignoreRegexNote: String
    let ignoreFilesNote: String
    let ignoreSuppressSync: String
    let ignoreSuppressRecord: String
    let ignoreAddApp: String
    let ignoreReset: String
    let ignoreAdd: String
    let ignoreEmpty: String
    let deviceName: String
    let save: String
    let fingerprintLabel: String
    let languageLabel: String
    let langSystem: String
    let previewDelay: String
    let noDevices: String
    let localDevice: String
    let pairedBadge: String
    let pairAction: String
    let unpairAction: String
    let notifyOnSync: String
    let notifyBundleNote: String
    let incognitoLabel: String
    let incognitoNote: String
    let portLabel: String
    let portRestartNote: String
    let maxEntries: String
    let sortLabel: String
    let sortRecent: String
    let sortFrequent: String
    let recordText: String
    let recordImages: String
    let recordFiles: String
    let recordNote: String
    let usageLabel: String
    let panelHotkeyLabel: String
    let slotHotkeysLabel: String
    let slotModifierLabel: String
    let slotHotkeysNote: String
    let hotkeyConflict: String
    let hotkeyUnset: String
    let hotkeyRecord: String
    let hotkeyPressKeys: String
    let hotkeyClear: String
    // Pair request window
    let pairRequestTitle: String
    let pairRequestBody: String
    let accept: String
    let reject: String
    // Preview card metadata
    let sourceAppLabel: String
    let originLabel: String
    let firstCopiedLabel: String
    let lastCopiedLabel: String
    let copyCountLabel: String
    let hintPin: String
    let hintUnpin: String
    let hintDelete: String
    // Startup guard and system capabilities
    let gotIt: String
    let openSystemSettings: String
    let guardDuplicateTitle: String
    let guardDuplicateBody: String
    let localNetworkTitle: String
    let localNetworkBody: String
    let notifySyncTitleFormat: String
    let autostartLabel: String
    let autostartUnavailable: String
    let autoPasteLabel: String
    let autoPasteNote: String
    let autoPastePermissionTitle: String
    let autoPastePermissionBody: String
    // Menu bar tooltip ({n} = number of pending pairing requests)
    let trayPendingFormat: String
    // Panel alerts; error messages reuse the in-panel overlay built for the
    // clear confirmation
    let alertOK: String
    let errHistoryMissing: String
    let errFilesMissing: String
    let errClipboardWriteFailed: String
    let errSettingsSaveFailed: String
    let errUnknown: String
    // Sync direction policy and type toggles
    let syncModeLabel: String
    let syncModeOff: String
    let syncModeBoth: String
    let syncModeSend: String
    let syncModeReceive: String
    let syncTypesNote: String
    let syncTypeText: String
    let syncTypeImages: String
    let syncTypeFiles: String
    let fileLimitLabel: String
    let fileLimitNote: String
    // Pairing failure overlay and sync rejection reasons; the wording matches
    // the Tauri client's errors table
    let pairFailedTitle: String
    let errPeerUnreachable: String
    let errPairRejected: String
    let errFingerprintMismatch: String
    let errTimeout: String
    let errNotPaired: String
    let errTooLarge: String
    let errSyncDisabled: String
    let errUnsupportedType: String
    let errChecksumMismatch: String
    let errIdentityMismatch: String
    let errUnsupportedVersion: String
}

/// Content of one in-panel error message
struct PanelAlert {
    /// Alert title, the only body text; it may wrap
    let title: String
    /// This error means the entry itself is dead, so drop the record while
    /// showing the message
    let discardsEntry: Bool
}

extension Texts {
    /// "Synced from X"
    func notifySyncTitle(_ device: String) -> String {
        notifySyncTitleFormat.replacingOccurrences(of: "{device}", with: device)
    }

    /// Menu bar tooltip: "lanecho — n pairing request(s) pending"
    func trayPending(_ count: Int) -> String {
        trayPendingFormat.replacingOccurrences(of: "{n}", with: String(count))
    }

    /// Error to readable message
    ///
    /// An error we do not cover is not swallowed: fall back to the generic
    /// message and append the raw description in parentheses, or the user
    /// sees only "Something went wrong" and we do not even learn what it was
    /// (same as the detail semantics of the Tauri client's formatError)
    func panelAlert(_ error: Error) -> PanelAlert {
        guard let core = error as? CoreError else {
            return PanelAlert(
                title: "\(errUnknown) (\(String(describing: error)))", discardsEntry: false)
        }
        // Match CoreError exhaustively, with no default: when the engine
        // gains a new error case this fails at compile time instead of
        // quietly falling through to the generic message
        switch core {
        case .historyMissing:
            return PanelAlert(title: errHistoryMissing, discardsEntry: true)
        case .filesMissing:
            return PanelAlert(title: errFilesMissing, discardsEntry: true)
        // Nothing wrong with the entry, this write just failed — deleting it
        // is what would actually lose data
        case .clipboardWriteFailed:
            return PanelAlert(title: errClipboardWriteFailed, discardsEntry: false)
        case .settingsSaveFailed:
            return PanelAlert(title: errSettingsSaveFailed, discardsEntry: false)
        }
    }

    /// Sync rejection code to readable message; an unknown code falls back to
    /// the generic failure plus the raw code, leaving room for the protocol
    /// to evolve
    func syncReasonText(_ code: String) -> String {
        switch code {
        case ReasonCode.notPaired: errNotPaired
        case ReasonCode.tooLarge: errTooLarge
        case ReasonCode.disabled: errSyncDisabled
        case ReasonCode.unsupportedType: errUnsupportedType
        case ReasonCode.checksumMismatch: errChecksumMismatch
        case ReasonCode.identityMismatch: errIdentityMismatch
        case ReasonCode.unsupportedVersion: errUnsupportedVersion
        default: "\(errUnknown) (\(code))"
        }
    }

    /// Transport error to readable message, used by the pairing failure
    /// overlay; TransportError is matched exhaustively so a new case fails at
    /// compile time rather than quietly landing on the generic message
    func transportErrorText(_ error: Error) -> String {
        guard let transport = error as? TransportError else {
            return "\(errUnknown) (\(String(describing: error)))"
        }
        return switch transport {
        case .peerUnreachable: errPeerUnreachable
        case .pairRejected: errPairRejected
        case .fingerprintMismatch: errFingerprintMismatch
        case .timeout: errTimeout
        case .rejected(let code): syncReasonText(code)
        case .disconnected: errPeerUnreachable
        case .io(let detail): "\(errUnknown) (\(detail))"
        }
    }
}

/// String entry point: initialized at launch from the language setting, and
/// switched the moment the settings page changes it
@MainActor
enum L10n {
    /// The string table currently in effect
    private(set) static var t: Texts = pick(language: "")
    /// The locale currently in effect; date formats and the like follow the
    /// UI language rather than the system region
    private(set) static var locale: Locale = .current

    /// Switch to the configured language (empty follows the system)
    static func apply(language: String) {
        t = pick(language: language)
        locale =
            switch language {
            case "zh": Locale(identifier: "zh_CN")
            case "en": Locale(identifier: "en_US")
            default: .current
            }
    }

    private static func pick(language: String) -> Texts {
        let isZh =
            switch language {
            case "zh": true
            case "en": false
            default: Locale.preferredLanguages.first?.hasPrefix("zh") == true
            }
        return isZh ? .zh : .en
    }
}

extension Texts {
    /// Chinese table
    static let zh = Texts(
        searchPlaceholder: "搜索历史…",
        emptyHistory: "暂无历史记录",
        emptySearch: "没有匹配的条目",
        quit: "退出",
        settings: "设置",
        about: "关于",
        aboutTitle: "关于 Lanecho",
        aboutTagline: "局域网共享剪贴板 —— 一台设备上复制, 所有设备上粘贴。",
        aboutVersionLabel: "版本",
        aboutHomepage: "项目主页",
        aboutIssues: "报告问题",
        aboutReleases: "更新日志",
        aboutLicense: "开源许可",
        aboutBuiltWith: "基于以下开源项目构建",
        aboutCopyright: "© 2026 Zero · MIT 许可证",
        startFailedTitle: "Lanecho 启动失败",
        syncLabel: "剪贴板同步",
        clearHistory: "清空",
        confirmClearTitle: "清空全部历史?",
        confirmClearBody: "包括固定条目在内的全部记录都会被删除, 不可撤销。",
        clearConfirm: "清空",
        cancel: "取消",
        pin: "固定",
        unpin: "取消固定",
        deleteEntry: "删除",
        settingsTitle: "Lanecho 偏好设置",
        tabGeneral: "通用",
        tabDevices: "在线设备",
        tabSync: "同步",
        tabStorage: "存储",
        tabHotkeys: "快捷键",
        tabIgnore: "忽略",
        ignorePaneApps: "应用",
        ignorePaneTypes: "剪贴板类型",
        ignorePaneRegex: "正则表达式",
        ignorePaneFiles: "文件",
        ignoreAppsNote: "忽略来自这些应用的文本内容。",
        ignoreTypesNote: "携带这些剪贴板类型标记的内容将被忽略; 预置列表覆盖常见密码管理器与临时内容标记。",
        ignoreRegexNote: "按输入的正则在文本中搜索, 命中任一条即忽略; 需要精确匹配请用 ^ $ 锚定。",
        ignoreFilesNote: "每行一条规则, # 开头为注释; 含 / 按完整路径匹配, 否则按文件名; 命中任一文件即忽略整批。",
        ignoreSuppressSync: "忽略同步",
        ignoreSuppressRecord: "忽略记录",
        ignoreAddApp: "添加应用…",
        ignoreReset: "重置",
        ignoreAdd: "添加",
        ignoreEmpty: "暂无条目",
        deviceName: "设备名称",
        save: "保存",
        fingerprintLabel: "设备指纹",
        languageLabel: "界面语言",
        langSystem: "跟随系统",
        previewDelay: "详情卡延迟",
        noDevices: "局域网内暂未发现设备",
        localDevice: "本机",
        pairedBadge: "已配对",
        pairAction: "配对",
        unpairAction: "解除配对",
        notifyOnSync: "同步通知",
        notifyBundleNote: "系统通知在 .app 打包版中生效。",
        incognitoLabel: "暂停记录(无痕)",
        incognitoNote: "只暂停本机历史记录, 不影响与其他设备的同步。",
        portLabel: "TCP 端口",
        portRestartNote: "端口修改在下次启动时生效。",
        maxEntries: "历史条目上限",
        sortLabel: "排序方式",
        sortRecent: "最近优先",
        sortFrequent: "次数优先",
        recordText: "记录文本",
        recordImages: "记录图像",
        recordFiles: "记录文件",
        recordNote: "记录类型只影响本机历史, 不影响同步。",
        usageLabel: "磁盘占用",
        panelHotkeyLabel: "历史面板",
        slotHotkeysLabel: "快速粘贴 (1…6)",
        slotModifierLabel: "修饰键",
        slotHotkeysNote: "不打开面板, 直接还原列表第 1~6 条。",
        hotkeyConflict: "注册失败(可能被其他应用占用)",
        hotkeyUnset: "未绑定",
        hotkeyRecord: "点击录制",
        hotkeyPressKeys: "请按快捷键…",
        hotkeyClear: "清除",
        pairRequestTitle: "配对请求",
        pairRequestBody: "希望与本机配对并同步剪贴板。",
        accept: "接受",
        reject: "拒绝",
        sourceAppLabel: "来源应用",
        originLabel: "来源设备",
        firstCopiedLabel: "首次复制",
        lastCopiedLabel: "上次复制",
        copyCountLabel: "复制次数",
        hintPin: "按 ⌥P 固定。",
        hintUnpin: "按 ⌥P 取消固定。",
        hintDelete: "按 ⌘⌫ 删除。",
        gotIt: "知道了",
        openSystemSettings: "打开系统设置",
        guardDuplicateTitle: "Lanecho 已在运行",
        guardDuplicateBody: "菜单栏里已有 Lanecho, 本次启动将退出。",
        localNetworkTitle: "允许 Lanecho 访问本地网络",
        localNetworkBody:
            "Lanecho 靠局域网发现你的其他设备。系统会在首次同步时询问权限, 拒绝后设备列表将始终为空。\n如果误点了拒绝, 可在系统设置 → 隐私与安全性 → 本地网络中重新开启。",
        notifySyncTitleFormat: "已从 {device} 同步",
        autostartLabel: "开机时启动",
        autostartUnavailable: "开机自启需要以 .app 形式安装后才可用。",
        autoPasteLabel: "选中后自动粘贴",
        autoPasteNote: "从历史选中一条后, 自动向当前应用发送 ⌘V, 省去手动粘贴。需要辅助功能权限。",
        autoPastePermissionTitle: "需要辅助功能权限",
        autoPastePermissionBody:
            "自动粘贴要模拟按键, 请在系统设置 → 隐私与安全性 → 辅助功能中勾选 Lanecho。\n授权后需重启 Lanecho 才会生效。",
        trayPendingFormat: "Lanecho — {n} 个配对请求待处理",
        alertOK: "好",
        errHistoryMissing: "这条记录的内容已丢失, 记录已移除",
        errFilesMissing: "源文件已被移动或删除, 记录已移除",
        errClipboardWriteFailed: "写入剪贴板失败, 请稍后再试",
        errSettingsSaveFailed: "设置保存失败",
        errUnknown: "操作失败",
        syncModeLabel: "同步策略",
        syncModeOff: "关闭",
        syncModeBoth: "互相同步",
        syncModeSend: "仅发出",
        syncModeReceive: "仅接收",
        syncTypesNote: "勾选的类型才会发送与接收。",
        syncTypeText: "文本",
        syncTypeImages: "图像",
        syncTypeFiles: "文件",
        fileLimitLabel: "文件大小上限",
        fileLimitNote: "MB, 超过上限的文件不同步(1~512)。",
        pairFailedTitle: "配对失败",
        errPeerUnreachable: "对方不可达, 请确认其在线",
        errPairRejected: "对方拒绝了配对",
        errFingerprintMismatch: "对方身份校验失败(指纹不一致)",
        errTimeout: "等待对方响应超时",
        errNotPaired: "尚未与对方配对",
        errTooLarge: "内容超过对方的大小限制",
        errSyncDisabled: "对方已暂停同步",
        errUnsupportedType: "对方不支持该内容类型",
        errChecksumMismatch: "传输校验失败, 内容已丢弃",
        errIdentityMismatch: "对方无法确认本机身份(证书与声明不一致)",
        errUnsupportedVersion: "对方不支持当前协议版本"
    )

    /// English table
    static let en = Texts(
        searchPlaceholder: "Search history…",
        emptyHistory: "No history yet",
        emptySearch: "No matching entries",
        quit: "Quit",
        settings: "Settings",
        about: "About",
        aboutTitle: "About Lanecho",
        aboutTagline: "Shared clipboard over LAN — copy on one device, paste on all of them.",
        aboutVersionLabel: "Version",
        aboutHomepage: "Homepage",
        aboutIssues: "Report an Issue",
        aboutReleases: "Release Notes",
        aboutLicense: "License",
        aboutBuiltWith: "Built with",
        aboutCopyright: "© 2026 Zero · MIT License",
        startFailedTitle: "Lanecho failed to start",
        syncLabel: "Clipboard Sync",
        clearHistory: "Clear",
        confirmClearTitle: "Clear all history?",
        confirmClearBody: "All entries including pinned ones will be deleted. This cannot be undone.",
        clearConfirm: "Clear",
        cancel: "Cancel",
        pin: "Pin",
        unpin: "Unpin",
        deleteEntry: "Delete",
        settingsTitle: "Lanecho Settings",
        tabGeneral: "General",
        tabDevices: "Devices",
        tabSync: "Sync",
        tabStorage: "Storage",
        tabHotkeys: "Hotkeys",
        tabIgnore: "Ignore",
        ignorePaneApps: "Apps",
        ignorePaneTypes: "Pasteboard Types",
        ignorePaneRegex: "Regex",
        ignorePaneFiles: "Files",
        ignoreAppsNote: "Ignore text copied from these applications.",
        ignoreTypesNote:
            "Content carrying any of these pasteboard types is ignored; the preset list covers common password managers and transient markers.",
        ignoreRegexNote:
            "Patterns search the text as written; any hit ignores it. Anchor with ^ $ for an exact match.",
        ignoreFilesNote:
            "One rule per line, # starts a comment; a pattern with / matches the full path, otherwise the file name; one hit ignores the whole batch.",
        ignoreSuppressSync: "Don't sync",
        ignoreSuppressRecord: "Don't record",
        ignoreAddApp: "Add Application…",
        ignoreReset: "Reset",
        ignoreAdd: "Add",
        ignoreEmpty: "No entries",
        deviceName: "Device Name",
        save: "Save",
        fingerprintLabel: "Fingerprint",
        languageLabel: "Language",
        langSystem: "System",
        previewDelay: "Preview delay",
        noDevices: "No devices discovered on this network",
        localDevice: "This device",
        pairedBadge: "Paired",
        pairAction: "Pair",
        unpairAction: "Unpair",
        notifyOnSync: "Sync notifications",
        notifyBundleNote: "System notifications take effect in the bundled .app build.",
        incognitoLabel: "Pause recording (incognito)",
        incognitoNote: "Pauses local history only; syncing with other devices continues.",
        portLabel: "TCP Port",
        portRestartNote: "Port changes take effect on next launch.",
        maxEntries: "History limit",
        sortLabel: "Sort order",
        sortRecent: "Most recent",
        sortFrequent: "Most frequent",
        recordText: "Record text",
        recordImages: "Record images",
        recordFiles: "Record files",
        recordNote: "Recording types affect local history only, not syncing.",
        usageLabel: "Disk usage",
        panelHotkeyLabel: "History panel",
        slotHotkeysLabel: "Quick paste (1…6)",
        slotModifierLabel: "Modifier",
        slotHotkeysNote: "Restores entries 1–6 directly without opening the panel.",
        hotkeyConflict: "Registration failed (possibly taken by another app)",
        hotkeyUnset: "Not bound",
        hotkeyRecord: "Click to record",
        hotkeyPressKeys: "Type shortcut…",
        hotkeyClear: "Clear",
        pairRequestTitle: "Pairing Request",
        pairRequestBody: "wants to pair with this device and sync clipboards.",
        accept: "Accept",
        reject: "Reject",
        sourceAppLabel: "Source app",
        originLabel: "From device",
        firstCopiedLabel: "First copied",
        lastCopiedLabel: "Last copied",
        copyCountLabel: "Copy count",
        hintPin: "Press ⌥P to pin.",
        hintUnpin: "Press ⌥P to unpin.",
        hintDelete: "Press ⌘⌫ to delete.",
        gotIt: "OK",
        openSystemSettings: "Open System Settings",
        guardDuplicateTitle: "Lanecho is already running",
        guardDuplicateBody: "Lanecho is already in the menu bar; this launch will quit.",
        localNetworkTitle: "Allow Lanecho on your local network",
        localNetworkBody:
            "Lanecho finds your other devices over the local network. macOS asks for permission on first sync; if you decline, the device list stays empty.\nYou can re-enable it in System Settings → Privacy & Security → Local Network.",
        notifySyncTitleFormat: "Synced from {device}",
        autostartLabel: "Launch at login",
        autostartUnavailable: "Launch at login requires the app to be installed as an .app.",
        autoPasteLabel: "Paste after selecting",
        autoPasteNote: "Sends ⌘V to the frontmost app after you pick an entry. Requires accessibility permission.",
        autoPastePermissionTitle: "Accessibility permission required",
        autoPastePermissionBody:
            "Auto paste synthesizes a keystroke. Enable Lanecho under System Settings → Privacy & Security → Accessibility.\nRestart Lanecho after granting it.",
        trayPendingFormat: "Lanecho — {n} pairing request(s) pending",
        alertOK: "OK",
        errHistoryMissing: "This entry's content is gone; it has been removed",
        errFilesMissing: "The source files were moved or deleted; the entry has been removed",
        errClipboardWriteFailed: "Could not write to the clipboard; please try again",
        errSettingsSaveFailed: "Could not save settings",
        errUnknown: "Something went wrong",
        syncModeLabel: "Sync policy",
        syncModeOff: "Off",
        syncModeBoth: "Two-way",
        syncModeSend: "Send only",
        syncModeReceive: "Receive only",
        syncTypesNote: "Only checked types are sent and received.",
        syncTypeText: "Text",
        syncTypeImages: "Images",
        syncTypeFiles: "Files",
        fileLimitLabel: "File size limit",
        fileLimitNote: "MB; larger files stay local (1–512).",
        pairFailedTitle: "Pairing failed",
        errPeerUnreachable: "Peer unreachable — make sure it is online",
        errPairRejected: "Peer declined the pairing request",
        errFingerprintMismatch: "Peer identity check failed (fingerprint mismatch)",
        errTimeout: "Timed out waiting for the peer",
        errNotPaired: "Not paired with this device yet",
        errTooLarge: "Content exceeds the peer's size limit",
        errSyncDisabled: "Peer has paused syncing",
        errUnsupportedType: "Peer does not support this content type",
        errChecksumMismatch: "Transfer checksum mismatch; content discarded",
        errIdentityMismatch: "The peer could not confirm this device's identity",
        errUnsupportedVersion: "The peer does not support this protocol version"
    )
}
