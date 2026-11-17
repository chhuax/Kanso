import AppKit
import KansoBridge
import OSLog
import PlatformMac

private let mcpLog = Logger(subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "mcp")

/// 标签上的 MCP 标记。`openedByClient` 非 nil 表示这个标签是 MCP 打开的（空串表示客户端没报名称）。
struct McpTabMark: Equatable {
    var openedByClient: String?
    var activityClient: String?
    var activityTool: String?

    var tooltipLines: [String] {
        var lines: [String] = []
        if let openedByClient {
            lines.append(String(localized: "Opened by MCP · \(McpText.clientName(openedByClient))"))
        }
        if let activityTool {
            let client = McpText.clientName(activityClient ?? "")
            lines.append(String(localized: "MCP · \(client) · \(activityTool)"))
        }
        return lines
    }
}

enum McpText {
    static let accessLevels: [FfiMcpAccess] = [.off, .readOnly, .full]

    static func accessTitle(_ level: FfiMcpAccess) -> String {
        switch level {
        case .off: String(localized: "Off")
        case .readOnly: String(localized: "Read only")
        case .full: String(localized: "Full")
        }
    }

    static func accessIndex(_ level: FfiMcpAccess) -> Int {
        accessLevels.firstIndex(of: level) ?? 0
    }

    static func clientName(_ name: String) -> String {
        name.isEmpty ? String(localized: "Unknown client") : name
    }

    static var sensitiveNotice: String {
        String(localized: "Terminal contents may include sensitive information.")
    }

    /// 审计行里的会话名。没有目标会话时返回 nil；id 还在但配置已删除时显示「已删除的会话」。
    static func auditSessionTitle(profileId: String?, names: [String: String]) -> String? {
        guard let profileId else { return nil }
        return names[profileId] ?? String(localized: "Deleted session")
    }

    static func unavailableReason(_ reason: FfiMcpUnavailable) -> String {
        switch reason {
        case .notSupported:
            String(localized: "MCP is not available in this build.")
        case .anotherInstance:
            String(localized: "Another copy of Kanso is already providing MCP.")
        case .failed:
            String(localized: "MCP failed to start.")
        }
    }
}

/// App 包内 `kanso-mcp` 的路径。文件还没打进包时也照样返回这个路径。
func mcpHelperPath(bundle: Bundle = .main) -> String {
    bundle.bundleURL.appendingPathComponent("Contents/MacOS/kanso-mcp").path
}

/// MCP 服务的生命周期、令牌和界面状态。回调先入队，再异步切到主线程，队列里不调用 core 或 server。
@MainActor
final class McpCoordinator {
    let core: KansoCore
    let server: KansoMcpServer
    let supported: Bool
    let bundleId: String
    private let tokens: KeychainSecretStore
    private let bridge: McpDelegateBridge

    private(set) var status: FfiMcpServerStatus = .stopped
    private(set) var clients: [FfiMcpClientInfo] = []
    private var openedBy: [UInt64: String] = [:]
    private var activities: [UInt64: FfiMcpActivity] = [:]
    private var activityClears: [UInt64: () -> Void] = [:]
    private var changeHandlers: [() -> Void] = []

    /// 把 MCP 打开的标签放进窗口。由主窗口设置。
    var onPlaceSession: ((UInt64) -> Void)?

    /// 测试可替换。默认 2 秒后在主线程执行；返回取消闭包。
    var scheduleAfter: (TimeInterval, @escaping @MainActor () -> Void) -> () -> Void

    init(
        core: KansoCore,
        useMockServer: Bool,
        tokens: KeychainSecretStore,
        supported: Bool,
        bundleId: String,
        scheduleAfter: @escaping (TimeInterval, @escaping @MainActor () -> Void) -> () -> Void = McpCoordinator.defaultSchedule
    ) {
        self.core = core
        self.tokens = tokens
        self.supported = supported
        self.bundleId = bundleId
        self.scheduleAfter = scheduleAfter
        let bridge = McpDelegateBridge()
        self.bridge = bridge
        server = useMockServer
            ? KansoMcpServer.newMock(core: core, delegate: bridge)
            : KansoMcpServer(core: core, delegate: bridge)
        bridge.owner = self
    }

    func addChangeHandler(_ handler: @escaping () -> Void) {
        changeHandlers.append(handler)
    }

    func mark(for session: UInt64) -> McpTabMark? {
        let opened = openedBy[session]
        let activity = activities[session]
        guard opened != nil || activity != nil else { return nil }
        return McpTabMark(
            openedByClient: opened,
            activityClient: activity?.clientName,
            activityTool: activity?.tool
        )
    }

    /// 启动时：渠道支持且总开关已开才 start。
    func startIfEnabled() {
        guard supported, core.mcpEnabled() else { return }
        guard let token = try? ensureToken() else { return }
        try? server.start(bundleId: bundleId, token: token)
    }

    /// 打开总开关时 start，关闭时 stop。start 失败不把开关拨回去，页面要留着让用户能关掉。
    func setEnabled(_ enabled: Bool) throws {
        if enabled {
            let token = try ensureToken()
            try core.setMcpEnabled(enabled: true)
            do {
                try server.start(bundleId: bundleId, token: token)
            } catch {
                // 状态由回调变成 Unavailable。
            }
        } else {
            try core.setMcpEnabled(enabled: false)
            server.stop()
        }
    }

    func stop() {
        server.stop()
    }

    func currentToken() throws -> String? {
        try McpClientToken.load(from: tokens)
    }

    /// 写入新令牌并通知服务断开旧连接。
    func regenerateToken() throws {
        let token = McpClientToken.generate()
        try McpClientToken.save(token, to: tokens)
        server.setToken(token: token)
    }

    fileprivate func apply(_ batch: McpDelegateBridge.Batch) {
        if let status = batch.status {
            self.status = status
            if case .unavailable(.failed(let detail)) = status {
                mcpLog.error("MCP start failed: \(detail, privacy: .public)")
            }
        }
        for (session, client) in batch.opened {
            openedBy[session] = client.name
            onPlaceSession?(session)
        }
        for (session, activity) in batch.activities {
            noteActivity(session: session, activity: activity)
        }
        for session in batch.released {
            openedBy[session] = nil
        }
        if let clients = batch.clients {
            self.clients = clients
        }
        emitChange()
    }

    private func noteActivity(session: UInt64, activity: FfiMcpActivity?) {
        if let activity {
            activityClears.removeValue(forKey: session)?()
            activities[session] = activity
            return
        }
        guard activities[session] != nil else { return }
        activityClears.removeValue(forKey: session)?()
        let cancel = scheduleAfter(2) { [weak self] in
            self?.activities[session] = nil
            self?.activityClears[session] = nil
            self?.emitChange()
        }
        activityClears[session] = cancel
    }

    private func emitChange() {
        for handler in changeHandlers { handler() }
    }

    private func ensureToken() throws -> String {
        if let existing = try McpClientToken.load(from: tokens) { return existing }
        let token = McpClientToken.generate()
        try McpClientToken.save(token, to: tokens)
        return token
    }

    nonisolated private static func defaultSchedule(
        _ delay: TimeInterval, _ work: @escaping @MainActor () -> Void
    ) -> () -> Void {
        let item = DispatchWorkItem { MainActor.assumeIsolated { work() } }
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
        return { item.cancel() }
    }
}

/// 服务回调可能在任意线程、并且在服务自己的锁里发出。这里只入队，立刻返回。
final class McpDelegateBridge: FfiMcpServerDelegate, @unchecked Sendable {
    struct Batch {
        var status: FfiMcpServerStatus?
        var clients: [FfiMcpClientInfo]?
        var opened: [(UInt64, FfiMcpClientInfo)] = []
        var activities: [(UInt64, FfiMcpActivity?)] = []
        var released: [UInt64] = []
    }

    nonisolated(unsafe) weak var owner: McpCoordinator?
    private let lock = NSLock()
    private var batch = Batch()
    private var scheduled = false

    func onStatusChanged(status: FfiMcpServerStatus) {
        enqueue { $0.status = status }
    }

    func onClientsChanged(clients: [FfiMcpClientInfo]) {
        enqueue { $0.clients = clients }
    }

    func onSessionOpened(sessionId: UInt64, client: FfiMcpClientInfo) {
        enqueue { $0.opened.append((sessionId, client)) }
    }

    func onActivity(sessionId: UInt64, activity: FfiMcpActivity?) {
        enqueue { $0.activities.append((sessionId, activity)) }
    }

    func onSessionReleased(sessionId: UInt64) {
        enqueue { $0.released.append(sessionId) }
    }

    private func enqueue(_ body: (inout Batch) -> Void) {
        lock.lock()
        body(&batch)
        let shouldSchedule = !scheduled
        scheduled = true
        lock.unlock()
        guard shouldSchedule else { return }
        DispatchQueue.main.async { [weak self] in self?.deliver() }
    }

    private func deliver() {
        lock.lock()
        let pending = batch
        self.batch = Batch()
        scheduled = false
        lock.unlock()
        let owner = owner
        MainActor.assumeIsolated { owner?.apply(pending) }
    }
}
