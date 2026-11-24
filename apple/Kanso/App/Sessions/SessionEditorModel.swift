import Foundation
import KansoBridge
import KansoUI

/// 会话颜色预设：色板里的 16 个固定 RGB（两行，每行 8 个），取样自参考截图的色板（visual-style §3.3）。
///
/// `FfiProfile.color` 存的就是固定 RGB，不随外观变化，所以色板也用固定值。
/// 与侧栏的关系：配置了颜色的会话，侧栏直接用这个 RGB（`SessionColor.parse`）；
/// 没配置颜色的会话仍按名称哈希从侧栏自己的系统动态色里取（那是"未指定"的兜底，不属于预设）。
/// 顺序决定色板的排列（先第一行后第二行），已存会话按 RGB 值匹配选中项，调整顺序不影响数据。
enum SessionColorPresets {
    static let all: [FfiRgb] = [
        FfiRgb(r: 0x3D, g: 0x9D, b: 0xF2), // 蓝
        FfiRgb(r: 0x3F, g: 0xB9, b: 0x50), // 绿
        FfiRgb(r: 0xE8, g: 0xB5, b: 0x3C), // 琥珀
        FfiRgb(r: 0xF0, g: 0x48, b: 0x3E), // 红
        FfiRgb(r: 0xB0, g: 0x7E, b: 0xF5), // 紫
        FfiRgb(r: 0x2E, g: 0xC4, b: 0xCF), // 青
        FfiRgb(r: 0xF2, g: 0x6D, b: 0x62), // 珊瑚
        FfiRgb(r: 0x9C, g: 0xD0, b: 0xFF), // 浅蓝
        FfiRgb(r: 0xF5, g: 0x82, b: 0x2A), // 橙
        FfiRgb(r: 0xE0, g: 0x5A, b: 0x9C), // 粉
        FfiRgb(r: 0x7E, g: 0xE7, b: 0x87), // 浅绿
        FfiRgb(r: 0x6B, g: 0x7D, b: 0xFA), // 靛蓝
        FfiRgb(r: 0x1F, g: 0xB7, b: 0x9A), // 蓝绿
        FfiRgb(r: 0xF0, g: 0xBE, b: 0x6B), // 浅琥珀
        FfiRgb(r: 0xE3, g: 0x6C, b: 0xD0), // 洋红
        FfiRgb(r: 0xCD, g: 0xA6, b: 0xFA), // 薰衣草
    ]
}

/// 会话的 MCP 访问级别读写。会话编辑对话框和会话启动器读写同一份 `mcpAccess`，设置页只读取
/// `mcpAccesses()` 统计数量。抽成协议是为了测试注入可失败、可记录写入的替身，不碰 core。
protocol McpAccessStoring: AnyObject, Sendable {
    func mcpAccess(profileId: String) throws -> FfiMcpAccess
    func setMcpAccess(profileId: String, level: FfiMcpAccess) throws
    /// 全部级别不是「关闭」的授权。一次读出，供列表批量标记，不要逐个会话调 `mcpAccess`。
    /// 读库失败会抛错：调用方不得把失败当成「全部关闭」，应显示读取失败并禁用依赖授权状态的操作。
    func mcpAccesses() throws -> [FfiMcpGrant]
}

extension McpAccessStoring {
    /// 会话 id → 级别，只含不是「关闭」的会话；启动器与活动记录窗口共用。
    /// 读失败原样抛出，由调用方决定怎么提示；id 重复时取后出现的（core 不会返回重复项，仅作防御）。
    func mcpLevelMap() throws -> [String: FfiMcpAccess] {
        Dictionary(try mcpAccesses().map { ($0.profileId, $0.level) }, uniquingKeysWith: { _, new in new })
    }
}

extension KansoCore: McpAccessStoring {}

/// 编辑对话框里可显隐的字段行。
enum SessionEditorField: CaseIterable {
    case name, kind, color, group, mcpAccess
    case shellCommand, startDir
    case host, port, user, auth, password, keyPath, keyPassphrase, jump, proxyCommand
    case encoding, locale
    case keepaliveEnabled, keepaliveInterval, keepaliveMaxMisses
    case idleEnabled, idleInterval, idleString
}

/// `save()` 的结果。
enum SessionSaveOutcome: Equatable {
    /// 会话与凭据都已保存，对话框可以关闭。
    case saved
    /// 校验或保存失败，`errorText` 有原因；会话没有改动，对话框保持打开。
    case failed
    /// 会话已保存，但写凭据失败；对话框保持打开让用户重试（重试会覆盖同一个 id，不会产生重复会话）。
    case savedButCredentialFailed
    /// 会话已保存，但 MCP 访问级别没写成功；对话框保持打开，重试会再写一次。
    case savedButMcpAccessFailed
}

/// 会话编辑对话框的视图模型：新建与编辑共用，不依赖 AppKit，可直接驱动测试。
///
/// 职责：保存字段值、按类型 / 认证方式 / 发行渠道决定字段可见性、组装 `FfiProfile`、
/// 先保存会话再写凭据。校验规则（主机为空、跳板链等）以 core 为准，这里只把 core 的错误原样渲染出来。
@MainActor
final class SessionEditorModel {
    let store: SessionStore
    /// 正在编辑的已保存会话；nil 表示新建。
    let original: FfiProfile?
    /// 保存用的 id。新建时在初始化时一次性生成：保存后凭据写入失败再重试，不能产生第二个会话。
    let profileID: String
    /// 发行渠道名（`KansoChannel.current`）。ProxyCommand 只在 Direct 渠道提供。
    let channel: String

    var name = ""
    var kind: SessionKindChoice = .local
    var color: FfiRgb?
    var groupID: String?

    var shellCommand = ""
    var startDir = ""

    var host = ""
    var portText = String(SessionEditorModel.defaultPort)
    var user = ""
    var auth: FfiAuthMethod = .password
    /// 新填写的密码；留空表示不修改已保存的密码（core 不会把已存密码回传给界面）。
    var password = ""
    var keyPath = ""
    /// 新填写的私钥口令；规则同 `password`。
    var keyPassphrase = ""
    var jumpID: String?
    var proxyCommand = ""

    var encoding = ""
    var locale = ""

    var keepaliveEnabled = true
    var keepaliveIntervalText = "30"
    var keepaliveMaxMissesText = "3"
    var idleEnabled = false
    var idleIntervalText = "3600"
    var idleString = "\\r"

    /// 新建会话默认关闭。`showsMcpAccess` 为假时界面不显示，保存也不写。
    /// 只有用户改过它，保存时才写回：打开对话框后别处（设置页）改了级别，不改这个字段的保存不能用旧值覆盖。
    var mcpAccess: FfiMcpAccess = .off {
        didSet { mcpAccessEdited = true }
    }

    private var mcpAccessEdited = false
    let showsMcpAccess: Bool
    /// 打开时读取已有会话的访问级别失败的原因。非 nil 时字段禁用且不会写回（不能把读不到当成「关闭」）。
    private(set) var mcpAccessLoadError: String?
    var isMcpAccessEnabled: Bool { mcpAccessLoadError == nil }
    private let mcpAccessStore: (any McpAccessStoring)?

    /// 最近一次保存失败的错误文字（显示在对话框底部）。
    private(set) var errorText: String?

    static let defaultPort = 22

    init(
        store: SessionStore,
        editing profile: FfiProfile? = nil,
        channel: String = KansoChannel.current,
        makeID: () -> String = { UUID().uuidString.lowercased() },
        showsMcpAccess: Bool = false,
        mcpAccessStore: (any McpAccessStoring)? = nil
    ) {
        self.store = store
        self.original = profile
        self.channel = channel
        self.showsMcpAccess = showsMcpAccess
        self.mcpAccessStore = mcpAccessStore
        self.profileID = profile?.id ?? makeID()
        if showsMcpAccess, let mcpAccessStore, original != nil {
            // init 里赋值不触发 didSet，不算用户改动。
            do {
                self.mcpAccess = try mcpAccessStore.mcpAccess(profileId: profileID)
            } catch {
                mcpAccessLoadError = CoreMessageText.text(for: error)
            }
        }
        guard let profile else { return }
        name = profile.name
        color = profile.color
        groupID = profile.group
        kind = profile.kindChoice
        switch profile.kind {
        case .local(let local):
            shellCommand = local.shellCommand
            startDir = local.startDir ?? ""
            encoding = local.encoding
            locale = local.locale ?? ""
        case .ssh(let ssh):
            load(target: ssh.target)
            encoding = ssh.encoding
            locale = ssh.locale ?? ""
        case .sftp(let target):
            load(target: target)
        }
    }

    private func load(target: FfiSshTarget) {
        host = target.host
        portText = String(target.port)
        user = target.user
        auth = target.auth
        keyPath = target.privateKeyPath ?? ""
        jumpID = target.jump
        proxyCommand = target.proxyCommand ?? ""
        keepaliveEnabled = target.keepalive.enabled
        keepaliveIntervalText = String(target.keepalive.intervalSecs)
        keepaliveMaxMissesText = String(target.keepalive.maxMisses)
        idleEnabled = target.keepalive.idleEnabled
        idleIntervalText = String(target.keepalive.idleIntervalSecs)
        idleString = target.keepalive.idleString
    }

    var isEditing: Bool { original != nil }
    var opensTemporaryShell: Bool { !isEditing && kind == .local }

    // MARK: - 选项

    var groups: [FfiGroup] {
        store.groups().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    /// 跳板机候选：其他已保存的 SSH 会话（排除自己；SFTP / 本地不能当跳板）。
    var jumpCandidates: [FfiProfile] {
        store.profiles()
            .filter { $0.id != profileID && $0.kindChoice == .ssh }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    var encodingOptions: [SessionEncodings.Option] { SessionEncodings.options(including: encoding) }

    /// 颜色选项：无 + 16 个预设（界面色板只显示非 nil 的，"无"通过再次点击取消选择表达）；已有会话带着预设之外的颜色时也要保留，避免保存时丢失。
    var colorOptions: [FfiRgb?] {
        var list: [FfiRgb?] = [nil] + SessionColorPresets.all.map { Optional($0) }
        if let color, !SessionColorPresets.all.contains(color) { list.append(color) }
        return list
    }

    // MARK: - 字段可见性与互斥

    var isSSHLike: Bool { kind == .ssh || kind == .sftp }

    /// ProxyCommand 只在 Direct 渠道提供（沙盒渠道不允许启动任意命令）。
    var supportsProxyCommand: Bool { channel == "Direct" }

    func isVisible(_ field: SessionEditorField) -> Bool {
        switch field {
        case .kind:
            true
        case .name, .color, .group:
            !opensTemporaryShell
        case .mcpAccess:
            showsMcpAccess && !opensTemporaryShell
        case .shellCommand, .startDir:
            kind == .local
        case .host, .port, .user, .auth, .jump:
            isSSHLike
        case .password:
            isSSHLike && auth == .password
        case .keyPath, .keyPassphrase:
            isSSHLike && auth == .privateKey
        case .proxyCommand:
            isSSHLike && supportsProxyCommand
        case .encoding, .locale:
            kind != .sftp
        case .keepaliveEnabled, .keepaliveInterval, .keepaliveMaxMisses:
            isSSHLike
        case .idleEnabled, .idleInterval, .idleString:
            kind == .ssh
        }
    }

    /// 选了跳板机就不能再填 ProxyCommand，反之亦然（ssh.md：二者只能选一个）。
    var isProxyCommandEnabled: Bool { jumpID == nil }
    var isJumpEnabled: Bool { proxyCommand.trimmingCharacters(in: .whitespaces).isEmpty }

    // MARK: - 端口

    /// 合法端口（1–65535）；不合法为 nil。
    var port: UInt16? {
        guard let value = UInt16(portText.trimmingCharacters(in: .whitespaces)), value >= 1 else { return nil }
        return value
    }

    /// 端口输入过滤：只保留数字，最多 5 位。
    static func sanitizedPortText(_ text: String) -> String {
        String(text.filter { $0.isASCII && $0.isNumber }.prefix(5))
    }

    // MARK: - 组装

    /// 按当前字段组装要保存的 `FfiProfile`。端口不合法时返回 nil。
    ///
    /// 不在这里替 core 做主机为空、跳板链之类的校验；也不因互斥悄悄丢掉一边：
    /// 界面已禁用冲突输入，万一同时存在，交给 core 报错比默默改数据更安全。
    func assemble() -> FfiProfile? {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let profileKind: FfiProfileKind
        switch kind {
        case .local:
            let start = startDir.trimmingCharacters(in: .whitespaces)
            profileKind = .local(profile: FfiLocalProfile(
                shellCommand: shellCommand.trimmingCharacters(in: .whitespaces),
                startDir: start.isEmpty ? nil : start,
                encoding: encoding,
                locale: normalizedLocale
            ))
        case .ssh:
            guard let target = makeTarget() else { return nil }
            profileKind = .ssh(profile: FfiSshProfile(target: target, encoding: encoding, locale: normalizedLocale))
        case .sftp:
            guard let target = makeTarget() else { return nil }
            profileKind = .sftp(target: target)
        }
        return FfiProfile(id: profileID, name: trimmedName, color: color, group: groupID, kind: profileKind)
    }

    private var normalizedLocale: String? {
        let text = locale.trimmingCharacters(in: .whitespaces)
        return text.isEmpty ? nil : text
    }

    /// 保活间隔（秒）：1–86400（`UInt16` 上限 65535）。
    private static func parsedKeepaliveSecs(_ text: String) -> UInt16? {
        guard let value = UInt16(text.trimmingCharacters(in: .whitespaces)), value >= 1 else { return nil }
        return value
    }

    /// 三项保活数字的解析结果；任一项不合法则为 nil，界面据此报错。
    private var keepaliveParsed: (interval: UInt16, maxMisses: UInt16, idleInterval: UInt16)? {
        guard let interval = Self.parsedKeepaliveSecs(keepaliveIntervalText),
              let idleInterval = Self.parsedKeepaliveSecs(idleIntervalText),
              let maxMisses = UInt16(keepaliveMaxMissesText.trimmingCharacters(in: .whitespaces)),
              maxMisses >= 1 else { return nil }
        return (interval, maxMisses, idleInterval)
    }

    private func makeTarget() -> FfiSshTarget? {
        guard let port, let keepalive = keepaliveParsed else { return nil }
        let key = keyPath.trimmingCharacters(in: .whitespaces)
        let proxy = proxyCommand.trimmingCharacters(in: .whitespaces)
        return FfiSshTarget(
            host: host.trimmingCharacters(in: .whitespaces),
            port: port,
            user: user.trimmingCharacters(in: .whitespaces),
            auth: auth,
            // 私钥路径只对私钥认证有意义；其他认证方式不保留旧值，避免配置里残留无效路径。
            privateKeyPath: auth == .privateKey && !key.isEmpty ? key : nil,
            jump: jumpID,
            // 非 Direct 渠道没有 ProxyCommand 输入，也不保存它。
            proxyCommand: supportsProxyCommand && !proxy.isEmpty ? proxy : nil,
            keepalive: FfiKeepaliveConfig(
                enabled: keepaliveEnabled,
                intervalSecs: keepalive.interval,
                maxMisses: keepalive.maxMisses,
                idleEnabled: idleEnabled,
                idleIntervalSecs: keepalive.idleInterval,
                idleString: idleString
            )
        )
    }

    // MARK: - 保存

    /// 保存会话，成功后再写凭据。
    ///
    /// 顺序不能反：`setCredential` 要求会话已存在（core 会报 NoSuchProfile）。
    /// 密码 / 口令留空 = 不修改已保存的值，所以留空时一定不调用 `setCredential`。
    func save() async -> SessionSaveOutcome {
        errorText = nil
        if name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errorText = String(localized: "Enter a name for the session.")
            return .failed
        }
        guard let profile = assemble() else {
            errorText = keepaliveParsed == nil
                ? String(localized: "Keepalive intervals must be numbers from 1 to 86400.")
                : String(localized: "The port must be a number from 1 to 65535.")
            return .failed
        }
        do {
            try store.saveProfile(profile: profile)
        } catch {
            errorText = CoreMessageText.text(for: error)
            return .failed
        }
        var accessError: Error?
        if showsMcpAccess, mcpAccessEdited, isMcpAccessEnabled, let mcpAccessStore {
            do {
                try mcpAccessStore.setMcpAccess(profileId: profileID, level: mcpAccess)
            } catch {
                accessError = error
            }
        }
        // 会话已经落库，无论访问级别写入是否成功都要通知刷新；放在写入之后，
        // 设置页收到通知重读时才能读到新级别。
        store.notifyLibraryChanged()
        do {
            if isSSHLike, auth == .password, !password.isEmpty {
                try await store.setCredential(profile: profileID, kind: .password, secret: password)
            }
            if isSSHLike, auth == .privateKey, !keyPassphrase.isEmpty {
                try await store.setCredential(profile: profileID, kind: .keyPassphrase, secret: keyPassphrase)
            }
        } catch {
            var text = String(localized: "The session was saved, but its credential could not be stored: \(CoreMessageText.text(for: error))")
            // 两处都失败时都要告诉用户，不能让凭据的提示盖掉访问级别的。
            if let accessError {
                text += "\n" + String(localized: "The session was saved, but its MCP access level could not be saved: \(CoreMessageText.text(for: accessError))")
            }
            errorText = text
            return .savedButCredentialFailed
        }
        if let accessError {
            errorText = String(localized: "The session was saved, but its MCP access level could not be saved: \(CoreMessageText.text(for: accessError))")
            return .savedButMcpAccessFailed
        }
        return .saved
    }
}
