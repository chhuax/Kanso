import AppKit
import KansoBridge
import KansoUI
import Observation
import TerminalKit

/// 工作区控制器：串起 core、事件分发、工作区模型与每个会话的终端视图。
///
/// 持有关系：控制器 → `KansoCore` / `CoreEventDispatcher` / `Workspace` / 每会话一个 `TerminalHost`。
/// 事件流：core 事件（任意线程）→ dispatcher 切主线程 → 本类 `handle` → `CoreEventMapping` 翻译 →
/// 更新 `Workspace` →（Observation）→ `refresh()` → 各视图观察者重绘。
/// 反向：UI 动作 → 本类方法 → core 调用 + `Workspace` 变更。
@MainActor
final class WorkspaceController {
    /// 拿不到目标 pane 像素尺寸（还没布局、没有 Metal 设备）时的兜底网格。
    static let fallbackGrid = FfiGridSize(cols: 100, rows: 30)

    let core: KansoCore
    let workspace: Workspace
    /// 时间戳 / 行号栏模式（全局）。新开的会话取它的当前值，变化时应用到所有已有视图并同步给 core。
    let gutter: GutterModeController
    /// 全部设置。显示 / 外观设置变化时应用到所有已有终端视图；新开的会话取它的当前值。
    let settings: SettingsStore
    /// 快捷键改绑；终端视图的按键拦截点通过它决定哪些组合键归命令（nil 时不拦截，测试用）。
    let keybindings: KeybindingController?
    private let dispatcher: CoreEventDispatcher
    var confirmer: CloseConfirmer

    private(set) var hosts: [SessionID: TerminalHost] = [:]
    private var transfers = TransferTracker()
    /// 已通知 core 为可见的会话。只靠它做差量，避免每次刷新都重复调用 FFI。
    private var visibleSessions: Set<SessionID> = []
    private var observers: [Int: @MainActor () -> Void] = [:]
    private var nextObserverToken = 0
    private var handlerToken: Int?
    private var isShutDown = false
    /// 确认框正在显示：期间再来的关闭请求直接忽略，避免叠出多个 sheet。
    private var isConfirming = false
    private lazy var gridEstimator = TerminalRenderer()

    /// 返回 pane 当前的像素尺寸（pt）；由分屏区视图提供。nil 表示拿不到，使用兜底网格。
    var paneSizeProvider: ((PaneID) -> CGSize?)?
    /// 请求把键盘焦点交给活动终端（侧栏点击后调用，因为点击同一个标签时模型不变化，观察者不会触发）。
    var requestTerminalFocus: (() -> Void)?
    /// 打开 / 复制会话失败时的 UI 回调（弹提示）。
    var errorHandler: ((Error) -> Void)?
    var onCopied: (() -> Void)?
    /// 会话启动器与编辑对话框的呈现（入口：侧栏、菜单）。
    private(set) lazy var sessions = SessionPresenter(controller: self)
    /// core 提问（主机密钥变更、keyboard-interactive、缺少凭据）的展示协调者；窗口控制器装配时注入。
    /// 为 nil 时收到提问直接回答 Cancel，不让 core 空等到超时。
    var promptPresenter: PromptPresenter?
    /// 每个会话最近一次 `legacyAlgorithms` 的各跳旧算法（空 hops 的事件视为清除）。会话关闭时移除。
    private(set) var legacyHops: [SessionID: [FfiLegacyHop]] = [:]

    init(
        core: KansoCore,
        dispatcher: CoreEventDispatcher,
        confirmer: CloseConfirmer,
        workspace: Workspace = Workspace(),
        gutter: GutterModeController = GutterModeController(),
        settings: SettingsStore = SettingsStore(),
        keybindings: KeybindingController? = nil
    ) {
        self.core = core
        self.gutter = gutter
        self.settings = settings
        self.keybindings = keybindings
        self.dispatcher = dispatcher
        self.confirmer = confirmer
        self.workspace = workspace
        handlerToken = dispatcher.addHandler { [weak self] event in self?.handle(event) }
        trackWorkspace()
        // 启动时把持久化的模式同步给 core（core 不持久化该设置，默认值未必与用户保存的一致）。
        core.setTimestampMode(sessionId: nil, mode: gutter.mode.ffiMode)
        gutter.observe(self) { [weak self] mode in self?.applyGutterMode(mode) }
        // 显示 / 外观设置：新会话与已有会话都要用上，且不需要重启。
        settings.observe(self) { [weak self] section in
            if section == .display || section == .appearance || section == .mouse { self?.applyTerminalSettings() }
            if section == .completion, let self {
                for host in hosts.values { host.completion.enabled = settings.completion.enabled }
            }
        }
        applyTerminalSettings()
    }

    /// 把当前显示 / 外观设置应用到所有终端视图。回滚与默认光标样式由 `SettingsStore` 下发给 core，不在这里。
    private func applyTerminalSettings() {
        // 估算网格用的渲染器也要用当前字体，否则新会话的初始网格按默认字体算，要等视图布局后才校正。
        gridEstimator?.font = TerminalFontResolver.font(
            family: settings.display.fontFamily, size: settings.display.fontSize
        ).font
        for host in hosts.values {
            TerminalSettingsApplier.apply(settings, to: host.view)
        }
    }

    /// 全局模式变化：应用到会话的视图 + core 全局设置。视图改栏宽会改变列数，
    /// 由 `onGridSizeChange` → `core.resize` 链路通知 core，这里不用额外处理。
    /// 有 AI 会话临时设置的会话保持它自己的设置（terminal.md §2）。
    private func applyGutterMode(_ mode: GutterMode) {
        for (id, host) in hosts where gutter.override(for: id) == nil { host.view.gutterMode = mode }
        core.setTimestampMode(sessionId: nil, mode: mode.ffiMode)
        refresh()
    }

    /// 只作用于某个会话的模式（AI 会话的临时设置，或作废后回到全局）。
    private func applySessionGutterMode(_ mode: GutterMode, to session: SessionID) {
        hosts[session]?.view.gutterMode = mode
        core.setTimestampMode(sessionId: session, mode: mode.ffiMode)
    }

    /// 状态栏"时间轴"开关（ui-shell.md §9）：活动会话是 AI 会话时只改它，否则切换全局模式。
    func toggleTimeline() {
        let session = workspace.activeSession
        if let mode = gutter.toggleTimeline(for: session), let session {
            applySessionGutterMode(mode, to: session)
        }
        refresh()
    }

    /// 会话的活动变化：agent 活动让该会话的时间栏默认隐藏，工具退出时恢复（terminal.md §2）。
    private func updateAgentGutter(session: SessionID, activity: FfiActivity) {
        let change: GutterMode?
        switch activity {
        case .agentIdle(let agent), .agentWorking(let agent):
            change = gutter.agentReported(session: session, agent: agent)
        case .idle, .commandRunning:
            change = gutter.agentEnded(session: session)
        }
        if let change { applySessionGutterMode(change, to: session) }
    }

    // MARK: - 观察

    /// 登记模型变化观察者（分屏区、侧栏、状态栏、窗口标题）。返回令牌。
    @discardableResult
    func addObserver(_ observer: @escaping @MainActor () -> Void) -> Int {
        nextObserverToken += 1
        observers[nextObserverToken] = observer
        return nextObserverToken
    }

    /// 监听 `Workspace` 变化。`withObservationTracking` 的 onChange 在变更**前**触发且只触发一次，
    /// 所以要推迟到主队列（变更完成后）刷新，并重新登记。
    private func trackWorkspace() {
        withObservationTracking {
            _ = workspace.sidebarSections
            _ = workspace.layout
            _ = workspace.activePane
            _ = workspace.activeSession
        } onChange: { [weak self] in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, !self.isShutDown else { return }
                    self.refresh()
                    self.trackWorkspace()
                }
            }
        }
    }

    /// 同步可见性并通知观察者。幂等，可随时调用。
    func refresh() {
        syncVisibility()
        for observer in observers.values { observer() }
    }

    /// 只有每个 pane 的活动标签算可见：切换时旧的 `setVisible(false)`、新的 `setVisible(true)`。
    private func syncVisibility() {
        let now = Set(workspace.allPanes.compactMap { workspace.activeTab(in: $0) })
        for id in visibleSessions.subtracting(now) where hosts[id] != nil {
            core.setVisible(sessionId: id, visible: false)
            hosts[id]?.completion.visible = false
        }
        for id in now.subtracting(visibleSessions) {
            core.setVisible(sessionId: id, visible: true)
            hosts[id]?.completion.visible = true
        }
        visibleSessions = now
    }

    // MARK: - 事件

    /// 所有 core 事件的旁路订阅（Filer 的传输事件路由）。在主线程、事件处理之前调用。
    var onCoreEvent: ((FfiCoreEvent) -> Void)?
    var onWillCloseSessions: (([SessionID]) async -> Void)?
    /// 标签已关闭并清理完毕后调用，供外部清掉按会话 id 保存的状态。
    var onSessionClosed: ((SessionID) -> Void)?

    private func handle(_ event: FfiCoreEvent) {
        onCoreEvent?(event)
        // 新输出到来时刷新查找计数（terminal.md §11）；查找栏隐藏时它直接返回，内部有节流。
        if case .terminalDirty(let session) = event {
            hosts[session]?.findBar.refreshStatus()
            return
        }
        switch event {
        case .sessionStateChanged(let session, _):
            hosts[session]?.completion.invalidate(clearCache: true)
        case .locationChanged(let session, _, _, _):
            hosts[session]?.completion.invalidate(clearCache: true)
        default: break
        }
        switch event {
        case .prompt(let prompt, let session, let request):
            if let promptPresenter {
                promptPresenter.enqueue(prompt: prompt, session: session, request: request)
            } else {
                core.respondPrompt(prompt: prompt, response: .cancel)
            }
            return
        case .promptDismissed(let prompt):
            promptPresenter?.dismiss(prompt: prompt)
            return
        case .legacyAlgorithms(let session, let hops):
            // 已关闭会话的迟到事件不记录，否则字典只增不减。
            guard workspace.item(session) != nil else { return }
            legacyHops[session] = hops.isEmpty ? nil : hops
            refresh()
            return
        default:
            break
        }
        if case .activityChanged(let session, let activity) = event, hosts[session] != nil {
            updateAgentGutter(session: session, activity: activity)
        }
        let update = CoreEventMapping.update(for: event)
        if case .ignored = update { return }
        CoreEventMapping.apply(update, to: workspace, transfers: &transfers)
        // 已结束 / 变更后的状态要立刻反映到 UI，不等 Observation 的下一轮。
        refresh()
    }

    // MARK: - 打开会话

    @discardableResult
    func openLocalShell(in pane: PaneID? = nil, profile: FfiLocalProfile? = nil) throws -> SessionID {
        let size = estimateGrid(for: pane ?? workspace.activePane, splitAxis: nil)
        PerformanceProbe.mark("grid")
        let id = try core.openSession(target: profile.map { .localShell(profile: $0) } ?? .defaultLocalShell, size: size)
        PerformanceProbe.mark("pty")
        adopt(id, in: pane, splitAxis: nil)
        return id
    }

    /// 按已保存的会话打开。网格估算与登记流程同 `openLocalShell`，只是 target 不同。
    /// core 对不支持或不存在的会话抛错（例如尚未实现的类型），由调用方（`perform`）交给 `errorHandler`。
    @discardableResult
    func openProfile(_ id: String, in pane: PaneID? = nil) throws -> SessionID {
        let size = estimateGrid(for: pane ?? workspace.activePane, splitAxis: nil)
        let sessionID = try core.openSession(target: .profile(id: id), size: size)
        adopt(sessionID, in: pane, splitAxis: nil)
        return sessionID
    }

    /// 重新连接已断开的标签：用它的已保存会话配置建立一条全新会话，留在原位置并替换旧标签。
    /// core 的会话 id 是一次性的，无法复活旧会话，所以这里必然是新的 SessionID。
    /// 只有来自已保存会话的标签才有可复用的配置；临时本地 shell 不提供重连。
    @discardableResult
    func reconnectSession(_ old: SessionID) throws -> SessionID? {
        guard let item = workspace.item(old),
              case .closed = item.connection,
              let profileID = item.profileID,
              let pane = workspace.pane(of: old) else { return nil }
        let index = workspace.tabs(in: pane).firstIndex { $0.id == old } ?? 0
        let new = try openProfile(profileID, in: pane)
        workspace.moveTab(new, toIndex: index)
        closeNow(old)
        return new
    }

    /// 复制活动标签；没有活动标签时退化为新本地 shell。
    @discardableResult
    func duplicateActiveTab() throws -> SessionID {
        guard let active = workspace.activeSession else { return try openLocalShell() }
        let size = estimateGrid(for: workspace.activePane, splitAxis: nil)
        let id = try core.openSession(target: .duplicate(session: active), size: size)
        adopt(id, in: nil, splitAxis: nil)
        return id
    }

    /// 拆分活动 pane：新开会话（有活动会话时复制它，否则默认本地 shell）放进新 pane。
    @discardableResult
    func split(axis: SplitAxis) throws -> SessionID {
        let target: FfiOpenTarget = workspace.activeSession.map { .duplicate(session: $0) } ?? .defaultLocalShell
        // 活动 pane 为空时不会真的拆分（模型规则），此时按整个 pane 估算。
        let willSplit = workspace.activeSession != nil
        let size = estimateGrid(for: workspace.activePane, splitAxis: willSplit ? axis : nil)
        let id = try core.openSession(target: target, size: size)
        adopt(id, in: nil, splitAxis: axis)
        return id
    }

    /// 供菜单 / 按钮调用的包装：失败交给 `errorHandler`，不向 UI 层抛。
    func perform(_ action: () throws -> Void) {
        do { try action() } catch { errorHandler?(error) }
    }

    /// MCP 打开的标签：放进当前分屏，不激活，因此不切换标签、也不抢焦点。
    func placeMcpSession(_ id: SessionID) {
        guard hosts[id] == nil else { return }
        adopt(id, in: workspace.activePane, splitAxis: nil, activate: false)
    }

    /// 把 core 已打开的会话登记进工作区并创建终端视图。
    private func adopt(_ id: SessionID, in pane: PaneID?, splitAxis: SplitAxis?, activate: Bool = true) {
        let info = core.sessionInfo(sessionId: id)
        let item = TabItem(
            id: id,
            kind: info.map { CoreEventMapping.tabKind(for: $0.kind) } ?? .local,
            name: info?.name ?? "",
            customTitle: info?.customTitle,
            profileID: info?.profile,
            connection: info.map { CoreEventMapping.connection(for: $0.state) } ?? .connecting
        )
        let pixelSize = paneSizeProvider?(pane ?? workspace.activePane) ?? CGSize(width: 900, height: 600)
        let host = TerminalHost(core: core, session: id, frame: CGRect(origin: .zero, size: pixelSize),
                                keybindings: keybindings, remote: info?.kind == .ssh)
        host.completion.enabled = settings.completion.enabled
        // 必须在 wire 之前设置：wire 会补报一次网格，那时列数应已按当前栏宽与字体计算。
        host.view.gutterMode = gutter.mode
        TerminalSettingsApplier.apply(settings, to: host.view)
        wire(host)
        hosts[id] = host
        host.view.onCopied = { [weak self] in self?.onCopied?() }
        dispatcher.attach(view: host.view, to: id)
        if let splitAxis {
            workspace.splitActivePane(axis: splitAxis, with: item)
        } else {
            workspace.addTab(item, in: pane, activate: activate)
        }
        refresh()
    }

    private func wire(_ host: TerminalHost) {
        let id = host.session
        // 尺寸变化交给 core（调整 PTY 与终端）；core 因此置脏并发 TerminalDirty，由 dispatcher 通知视图取帧。
        // 尺寸类调用的错误（会话已关闭）无事可做，忽略。
        host.view.onGridSizeChange = { [core] cols, rows, cellWidthPx, cellHeightPx in
            guard let cols = UInt16(exactly: cols), let rows = UInt16(exactly: rows) else { return }
            try? core.resize(
                sessionId: id,
                size: FfiGridSize(cols: cols, rows: rows),
                cellWidthPx: UInt16(clamping: cellWidthPx),
                cellHeightPx: UInt16(clamping: cellHeightPx)
            )
        }
        // 用户点了某个终端：同步为活动标签。程序设焦点也会触发，activate 幂等所以无害。
        host.view.onBecomeFirstResponder = { [weak self] in
            guard let self, self.workspace.item(id) != nil, self.workspace.activeSession != id else { return }
            self.workspace.activate(id)
        }
        // 视图在设置回调之前可能已经算过网格；补报一次，让 core 知道单元格像素尺寸。
        if let grid = host.view.gridSize {
            host.view.onGridSizeChange?(grid.cols, grid.rows, grid.cellWidthPx, grid.cellHeightPx)
        }
    }

    /// 用目标 pane 的像素尺寸按单元格尺寸估算打开会话时的网格；之后视图的 `onGridSizeChange` 会校正。
    private func estimateGrid(for pane: PaneID, splitAxis: SplitAxis?) -> FfiGridSize {
        guard var size = paneSizeProvider?(pane), size.width > 0, size.height > 0, let renderer = gridEstimator else {
            return Self.fallbackGrid
        }
        switch splitAxis {
        case .horizontal: size.width /= 2
        case .vertical: size.height /= 2
        case nil: break
        }
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let grid = renderer.gridSize(for: size, scale: scale)
        guard let cols = UInt16(exactly: grid.cols), let rows = UInt16(exactly: grid.rows) else { return Self.fallbackGrid }
        return FfiGridSize(cols: cols, rows: rows)
    }

    // MARK: - 关闭

    /// 关闭会话：需要时确认一次，同意后逐个关闭。返回是否执行了关闭（取消 / 无目标 / 确认框已在显示返回 false）。
    ///
    /// 选择"立即移除"而不是等 `closed(byUser)` 事件：`closeSession` 同步生效，立即移除标签与视图，
    /// 之后到达的该会话事件在工作区里找不到标签，会被安静忽略，不会重复移除。
    @discardableResult
    func requestClose(_ sessions: [SessionID]) async -> Bool {
        let targets = sessions.filter { workspace.item($0) != nil }
        guard !targets.isEmpty, !isConfirming else { return false }
        if let confirmation = workspace.closeConfirmation(for: targets) {
            guard await confirm(confirmation, isQuit: false) else { return false }
        }
        await onWillCloseSessions?(targets)
        for id in targets { closeNow(id) }
        return true
    }

    /// 退出 / 关窗前的确认（所有会话合并成一次）。无需确认或用户同意返回 true。
    func confirmQuit() async -> Bool {
        guard !isConfirming else { return false }
        if let confirmation = workspace.closeConfirmation(for: workspace.sidebarOrder) {
            guard await confirm(confirmation, isQuit: true) else { return false }
        }
        await onWillCloseSessions?(workspace.sidebarOrder)
        return true
    }

    private func confirm(_ confirmation: CloseConfirmation, isQuit: Bool) async -> Bool {
        isConfirming = true
        defer { isConfirming = false }
        let titles = confirmation.sessions.compactMap { workspace.item($0) }.map {
            TabPresentation.primaryTitle($0, homeDirectory: NSHomeDirectory())
        }
        return await confirmer.confirm(CloseConfirmationRequest(
            titles: titles, interruptsTransfers: confirmation.interruptsTransfers, isQuit: isQuit
        ))
    }

    private func closeNow(_ id: SessionID) {
        guard workspace.item(id) != nil else { return }
        core.closeSession(sessionId: id)
        workspace.removeTab(id)
        if let host = hosts.removeValue(forKey: id) {
            host.completion.invalidate(clearCache: true)
            host.findBar.onDismiss = nil
            host.view.onGridSizeChange = nil
            host.view.onBecomeFirstResponder = nil
            host.view.removeFromSuperview()
        }
        visibleSessions.remove(id)
        transfers.forget(session: id)
        legacyHops[id] = nil
        gutter.forget(session: id)
        refresh()
        onSessionClosed?(id)
    }

    /// 应用退出前调用：通知 core 关闭所有会话并收尾（最多等几秒，不会卡住退出）。可重复调用。
    func shutdown() {
        guard !isShutDown else { return }
        isShutDown = true
        for id in hosts.keys { core.closeSession(sessionId: id) }
        core.shutdown()
    }

    // MARK: - 其他动作

    /// 设置或清除标签的自定义名（同步通知 core，让 MCP 等处看到同一标题）。
    func renameSession(_ id: SessionID, to title: String?) {
        let trimmed = title?.trimmingCharacters(in: .whitespacesAndNewlines)
        let normalized = (trimmed?.isEmpty == false) ? trimmed : nil
        core.setSessionTitle(sessionId: id, title: normalized)
        if workspace.setCustomTitle(id, normalized) {
            refresh()
        }
    }

    /// 清除活动会话的缓冲区。会话已关闭等错误无事可做，忽略。
    func clearActiveBuffer() {
        guard let id = workspace.activeSession else { return }
        try? core.clearBuffer(sessionId: id)
    }

    /// 提问对话框里显示的标签标题；会话不在工作区时为 nil。
    func promptSessionTitle(_ session: SessionID) -> String? {
        workspace.item(session).map { TabPresentation.primaryTitle($0, homeDirectory: NSHomeDirectory()) }
    }

    /// 活动会话的 Legacy SSH 悬停文字；没有活动会话或没有旧算法时为 nil（状态栏据此隐藏标记）。
    var activeLegacyToolTip: String? {
        guard let id = workspace.activeSession, let hops = legacyHops[id] else { return nil }
        return LegacySshSummary.toolTip(hops: hops)
    }

    func host(for session: SessionID) -> TerminalHost? { hosts[session] }
}
