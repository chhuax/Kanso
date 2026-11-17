import AppKit
import KansoBridge
import KansoUI
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

    /// 审计行里的参数摘要：折成一行并截断，全文放 toolTip。
    static func argsSummary(_ args: String, limit: Int = 80) -> String {
        let oneLine = args.replacingOccurrences(of: "\n", with: " ")
        return oneLine.count > limit ? String(oneLine.prefix(limit)) + "…" : oneLine
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
    /// 令牌缓存。钥匙串读取可能阻塞在系统授权框上，所以只在后台线程或用户主动操作时读，
    /// 其余路径（页面刷新、MCP 事件处理）一律用这份缓存。nil 表示还没读到或还没生成。
    private(set) var cachedToken: String?
    /// 开关已打开但服务没能启动时的原因（取令牌失败或 `start` 抛错）；启动成功或开关改动后清空。
    private(set) var startError: String?
    /// 启动时的后台取令牌与 `start` 任务；只为让测试能等待完成。
    private(set) var startupTask: Task<Void, Never>?
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
    ///
    /// 令牌在后台线程读取（可能弹钥匙串授权框，不能卡住主线程启动），读到后回主线程再 `start`。
    /// 失败只记日志并写入 `startError`，由设置页告诉用户服务没有启动。
    func startIfEnabled() {
        guard supported, core.mcpEnabled() else { return }
        let tokens = tokens
        startupTask = Task { [weak self] in
            let result = await Task.detached { Result { try Self.ensureToken(in: tokens) } }.value
            self?.finishStartup(result)
        }
    }

    private func finishStartup(_ result: Result<String, Error>) {
        // 等待钥匙串期间用户可能已经关掉开关，或已经手动启动了服务。
        guard core.mcpEnabled() else { return }
        if case .running = status { return }
        do {
            let token = try result.get()
            cachedToken = token
            try server.start(bundleId: bundleId, token: token)
            startError = nil
        } catch {
            mcpLog.error("MCP startup failed: \(String(describing: error), privacy: .public)")
            startError = CoreMessageText.text(for: error)
        }
        emitChange()
    }

    /// 在后台读取令牌填充缓存（不生成）。设置页打开时调用；读失败时缓存保持不变。
    func refreshTokenCache() {
        guard cachedToken == nil else { return }
        let tokens = tokens
        Task { [weak self] in
            let token = await Task.detached { try? McpClientToken.load(from: tokens) }.value
            guard let self, cachedToken == nil, let token else { return }
            cachedToken = token
            emitChange()
        }
    }

    /// 打开总开关时 start，关闭时 stop。start 失败不把开关拨回去，页面要留着让用户能关掉。
    func setEnabled(_ enabled: Bool) throws {
        startError = nil
        if enabled {
            // 用户主动操作，缓存没有时在这里同步取（可能弹授权框，但用户正在等这个结果）。
            let token = try cachedToken ?? Self.ensureToken(in: tokens)
            cachedToken = token
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

    /// 写入新令牌并通知服务断开旧连接。保存失败时抛出，旧令牌仍然有效、缓存不变。
    func regenerateToken() throws {
        let token = McpClientToken.generate()
        try McpClientToken.save(token, to: tokens)
        cachedToken = token
        server.setToken(token: token)
    }

    /// 标签关闭后清掉它的 MCP 标记与待执行的清除计时，避免会话 id 对应的状态一直留着。
    func forget(session: UInt64) {
        openedBy[session] = nil
        activities[session] = nil
        activityClears.removeValue(forKey: session)?()
        emitChange()
    }

    func apply(_ batch: McpDelegateBridge.Batch) {
        if let status = batch.status {
            self.status = status
            if case .unavailable(.failed(let detail)) = status {
                mcpLog.error("MCP start failed: \(detail, privacy: .public)")
            }
        }
        for (session, client) in batch.opened {
            // 回调到达时会话可能已被关掉；这时放置标签会留下没人能关的标签。
            guard core.sessionInfo(sessionId: session) != nil else { continue }
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

    /// 读令牌，没有就生成并保存。会访问钥匙串，不要在主线程调用（`setEnabled` 的用户操作路径除外）。
    nonisolated private static func ensureToken(in tokens: KeychainSecretStore) throws -> String {
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
