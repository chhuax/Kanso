import AppKit
import KansoBridge
import KansoUI
import PlatformMac
import TerminalKit

/// 主窗口：左侧竖向标签栏 + 右侧（分屏区 + 状态栏）。
///
/// 默认使用真实 core（本地 shell）；启动参数 `--mock-core` 改用 mock core（纯内存、确定性数据，界面开发用：
/// 按键只会让帧序号加一，画面整体上移一行）。
/// 会话、事件与终端视图的管理在 `WorkspaceController`，这里只负责窗口、菜单动作与退出确认。
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, NSMenuItemValidation {
    /// 启动参数：使用 mock core。
    nonisolated static let mockCoreArgument = "--mock-core"

    let controller: WorkspaceController
    private let stateStore: WindowStateStore
    private let confirmer: AlertCloseConfirmer
    private let promptHost = WindowPromptSheetHost()
    private let splitController = MainSplitViewController()
    private let paneContainer: PaneContainerView
    private var sidebarAccessory: NSTitlebarAccessoryViewController?
    /// 当前生效的标签栏位置；与设置不同时由 `applyTabBarPosition` 切换。
    private(set) var tabBarPosition: TabBarPosition = .left
    /// 顶部模式下标签条是否显示（⌘⌥← 切换）。与左侧竖栏的收起状态分开记：两种位置各自有自己的显隐。
    private var topTabBarVisible: Bool
    static let topTabBarVisibleKey = "kanso.topTabBar.visible"
    private let senderSplit = NSSplitViewController()
    private let senderItem: NSSplitViewItem
    let senderModel: SenderModel
    private lazy var senderPanel: SenderPanel = {
        let panel = SenderPanel(model: senderModel)
        panel.onStateChange = { [weak self] in self?.updateSenderStatus() }
        return panel
    }()
    private var senderCollapsedObservation: NSKeyValueObservation?
    private var senderResizeObserver: NSObjectProtocol?
    private var senderHeight: CGFloat
    private var restoringSenderHeight = true
    private let sidebarItem: NSSplitViewItem
    /// 普通（非系统样式）侧栏 item：`toggleSidebar` 对它无效，折叠要直接切 `isCollapsed`。
    private let usesPlainSidebar: Bool
    /// 辅助 Filer（位于标签栏与分屏区之间，可收起；filer.md §1.1）。
    private let filerItem: NSSplitViewItem
    let filer: FilerCoordinator
    let gitModel = GitPanelModel()
    private let gitItem: NSSplitViewItem
    private lazy var gitPanel: GitPanelView = {
        let panel = GitPanelView(model: gitModel, settings: controller.settings)
        panel.onClose = { [weak self] in self?.setGitCollapsed(true) }
        return panel
    }()
    private let gitButton = GitTitlebarButton()
    private let sidebarButton = KButton(symbol: "sidebar.left", style: .icon)
    private let gitReadsEnabled: Bool
    static let gitDefaultWidth: CGFloat = 480
    static let gitMinWidth: CGFloat = 280
    static let gitMaxWidth: CGFloat = 760
    /// 用户上次使用的 Git 面板宽度；收起时记录，展开时恢复。
    private static let gitWidthKey = "kanso.git.width"
    private(set) var commandPalette: CommandPalettePresenter!
    private var filerCollapsedObservation: NSKeyValueObservation?
    private var filerCollapsedBeforeSftp: Bool?
    /// 首次展开时把 Filer 设为默认宽度；之后以用户拖动后的宽度（autosave）为准。
    static let filerWidthInitializedKey = "kanso.filer.widthInitialized"
    static let filerDefaultWidth: CGFloat = 320
    static let filerMinWidth: CGFloat = 220
    static let filerMaxWidth: CGFloat = 560
    private let defaults: UserDefaults
    private let statusBar = StatusBarView()
    let mcp: McpCoordinator
    /// MCP 活动记录窗口；第一次打开时才创建，之后一直持有，保证同一时间只有一个。
    private var mcpActivity: McpActivityWindowController?
    /// 活动记录窗口的位置记忆名；mock core（测试、截图）下为 nil，不写用户真实的偏好。
    private let mcpActivityAutosaveName: String?
    private var terminalTransfers: [UInt64: (FfiTransferDirection, FfiXferProtocol)] = [:]
    private let baseTitle = "Kanso · \(KansoChannel.current)"
    /// 用户已确认退出（关窗确认过）。之后的 `applicationShouldTerminate` 不再询问，避免确认两次。
    private(set) var quitConfirmed = false

    /// - Parameters:
    ///   - defaults: 保存窗口状态用的偏好存储；测试注入临时 suite。
    ///   - gutter: 时间戳 / 行号栏模式控制器，与主菜单共用；测试注入与 `settings` 共用存储的实例（nil 时自建）。
    ///   - settings: 全部设置。core 创建后立即连接并载入（首次启动时把 `defaults` 里的旧偏好迁移进去）。
    ///   - keybindings: 快捷键改绑，接到每个终端视图的按键拦截点；nil 不拦截。
    /// - Throws: 真实 core 创建失败（数据目录不可用等）或本地 shell 启动失败时抛出，由调用方向用户展示。
    init(
        useMockCore: Bool = CommandLine.arguments.contains(MainWindowController.mockCoreArgument),
        defaults: UserDefaults = .standard,
        gutter: GutterModeController? = nil,
        settings: SettingsStore = SettingsStore(),
        keybindings: KeybindingController? = nil,
        keychain: any KeychainClient = SystemKeychainClient(),
        mcpSupported: Bool = mcpSupported()
    ) throws {
        stateStore = WindowStateStore(defaults: defaults)
        self.defaults = defaults
        mcpActivityAutosaveName = useMockCore ? nil : "KansoMcpActivityWindow"
        gitReadsEnabled = !useMockCore
        confirmer = AlertCloseConfirmer()
        let dispatcher = CoreEventDispatcher()
        let (core, dataDirectory) = try Self.makeCore(useMock: useMockCore, listener: dispatcher)
        PerformanceProbe.mark("core")
        let bundleId = Bundle.main.bundleIdentifier ?? "com.chhuax.kanso"
        let mcp = McpCoordinator(
            core: core,
            makeServer: { core, delegate in
                useMockCore
                    ? KansoMcpServer.newMock(core: core, delegate: delegate)
                    : KansoMcpServer(core: core, delegate: delegate)
            },
            tokens: KeychainSecretStore(service: bundleId, client: keychain),
            supported: mcpSupported, bundleId: bundleId
        )
        self.mcp = mcp
        // 必须在创建任何视图之前：载入会应用外观、时间轴模式，并把终端选项（回滚、默认光标）下发给 core，
        // 之后打开的第一个会话就已经是用户的设置。
        settings.attach(
            backend: core, legacyDefaults: defaults,
            themesDirectory: dataDirectory?.appendingPathComponent("themes", isDirectory: true)
        )
        PerformanceProbe.mark("settings")
        let gutter = gutter ?? GutterModeController(settings: settings)
        let controller = WorkspaceController(
            core: core, dispatcher: dispatcher, confirmer: confirmer, gutter: gutter,
            settings: settings, keybindings: keybindings
        )
        self.controller = controller

        let paneContainer = PaneContainerView(controller: controller)
        self.paneContainer = paneContainer
        topTabBarVisible = defaults.object(forKey: Self.topTabBarVisibleKey) as? Bool ?? true
        senderModel = SenderModel(core: core)
        PerformanceProbe.mark("sender-panel")
        senderHeight = min(400, max(80, defaults.object(forKey: "kanso.sender.height") as? Double ?? 160))
        let terminalController = NSViewController()
        terminalController.view = paneContainer
        let terminalItem = NSSplitViewItem(viewController: terminalController)
        terminalItem.minimumThickness = 80
        senderSplit.splitView.isVertical = false
        senderSplit.splitView.dividerStyle = .thin
        senderSplit.addSplitViewItem(terminalItem)
        let senderController = NSViewController()
        senderController.view = NSView()
        senderItem = NSSplitViewItem(viewController: senderController)
        senderItem.minimumThickness = 80
        senderItem.maximumThickness = 400
        senderItem.canCollapse = true
        senderItem.holdingPriority = NSLayoutConstraint.Priority(260)
        senderSplit.addSplitViewItem(senderItem)
        senderItem.isCollapsed = !defaults.bool(forKey: "kanso.sender.visible")
        let content = ShellContentView(panes: senderSplit.view, statusBar: statusBar)
        let contentController = NSViewController()
        contentController.view = content
        let filer = FilerCoordinator(controller: controller)
        PerformanceProbe.mark("filer-panel")
        self.filer = filer
        paneContainer.sftpViewProvider = { [weak filer] in filer?.dualWorkspace(for: $0) }
        let filerController = NSViewController()
        filerController.view = NSView()

        let sidebarController = TabSidebarViewController(controller: controller)
        // macOS 26 的系统侧栏是悬浮的 Liquid Glass 面板，在非 Apple Silicon 显卡上显示成带缝的实心圆角块；
        // 27 起系统侧栏贴边，与设计一致。因此只有 26.x 用普通 item（贴边、直角、无缝），其余版本保持系统侧栏。
        usesPlainSidebar = ProcessInfo.processInfo.operatingSystemVersion.majorVersion == 26
        sidebarItem = usesPlainSidebar
            ? NSSplitViewItem(viewController: sidebarController)
            : NSSplitViewItem(sidebarWithViewController: sidebarController)
        sidebarItem.minimumThickness = DesignTokens.Spacing.sidebarMinWidth
        sidebarItem.canCollapse = true
        // 系统侧栏 item 默认 holdingPriority 为 260（窗口变宽时保持宽度）；普通 item 只有 251，会被拉伸，需显式对齐并高于内容区。
        if usesPlainSidebar { sidebarItem.holdingPriority = NSLayoutConstraint.Priority(261) }
        splitController.addSplitViewItem(sidebarItem)
        filerItem = NSSplitViewItem(viewController: filerController)
        filerItem.minimumThickness = Self.filerMinWidth
        filerItem.maximumThickness = Self.filerMaxWidth
        filerItem.canCollapse = true
        // 窗口变窄时先压缩分屏区（它的 holdingPriority 最低），分屏区到最小宽度后才压缩 Filer：
        // 同时保证终端区始终保留最小可用宽度。
        filerItem.holdingPriority = NSLayoutConstraint.Priority(255)
        splitController.addSplitViewItem(filerItem)
        let contentItem = NSSplitViewItem(viewController: contentController)
        contentItem.minimumThickness = 240
        contentItem.holdingPriority = NSLayoutConstraint.Priority(260)
        splitController.addSplitViewItem(contentItem)
        PerformanceProbe.mark("git-panel")
        let gitController = NSViewController()
        gitController.view = NSView()
        gitItem = NSSplitViewItem(viewController: gitController)
        gitItem.minimumThickness = Self.gitMinWidth
        gitItem.maximumThickness = Self.gitMaxWidth
        gitItem.canCollapse = true
        gitItem.isCollapsed = true
        gitItem.holdingPriority = NSLayoutConstraint.Priority(254)
        splitController.addSplitViewItem(gitItem)
        // 记住侧栏宽度与收起状态。
        if !useMockCore && PerformanceProbe.root == nil { splitController.splitView.autosaveName = "MainSplitView" }
        splitController.splitView.dividerStyle = .thin

        PerformanceProbe.mark("panels")
        let window = KansoWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1100, height: 680),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        // 不用 fullSizeContentView：标题栏是贯通的一条（与窗口同底色、底部一条细线），侧栏、文件浏览器、终端都从它下方开始。
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .line
        window.backgroundColor = .token(DesignTokens.Color.bgWindow)
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 640, height: 360)
        window.contentViewController = splitController
        window.setContentSize(NSSize(width: 1100, height: 680))
        window.center()
        // 必须在 center() 之后：autosave 恢复的位置要覆盖居中。
        if !useMockCore && PerformanceProbe.root == nil { window.setFrameAutosaveName("MainWindow") }
        super.init(window: window)
        sidebarController.mcpMark = { [weak self] id in self?.mcp.mark(for: id) }
        paneContainer.mcpMark = { [weak self] id in self?.mcp.mark(for: id) }
        // 点指示器看的是 AI 工具做了什么，所以打开活动记录；开关与接入命令在设置页，菜单里有入口。
        statusBar.onOpenMcp = { [weak self] in self?.showMcpActivity() }
        mcp.onShowActivity = { [weak self] in self?.showMcpActivity() }
        mcp.onPlaceSession = { [weak self] id in self?.placeMcpSession(id) }
        controller.sessions.mcpSupported = { [weak self] in self?.mcp.supported ?? false }
        controller.onSessionClosed = { [weak self] id in self?.mcp.forget(session: id) }
        // 只刷新侧栏标签行与状态栏指示器；`controller.refresh()` 会刷新整个工作区，MCP 事件用不着。
        // 只有标签标记或服务状态变化才刷新侧栏；clients 变化只影响状态栏指示器；
        // `.audit`（审计库变化）只与活动记录窗口有关，这里忽略。
        mcp.addChangeHandler { [weak self, weak sidebarController] change in
            guard !change.isDisjoint(with: [.clients, .status, .marks]) else { return }
            self?.updateMcpIndicator()
            if !change.isDisjoint(with: [.marks, .status]) {
                sidebarController?.refresh()
                if self?.tabBarPosition == .top { self?.paneContainer.reload() }
            }
        }
        mcp.startIfEnabled()
        senderModel.isVisible = !senderItem.isCollapsed
        senderModel.onNotice = { [weak self] in self?.statusBar.showNotice($0, isError: $1) }
        controller.sessions.onDataImported = { [weak self] in self?.senderModel.cancelEditing() }
        statusBar.onShowSender = { [weak self] in self?.setSenderVisible(true) }
        controller.onCopied = { [weak self] in self?.statusBar.showNotice(String(localized: "Copied"), isError: false) }
        senderCollapsedObservation = senderItem.observe(\.isCollapsed) { [weak self] _, _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.loadVisiblePanels()
                self.senderModel.isVisible = !self.senderItem.isCollapsed
                self.defaults.set(self.senderModel.isVisible, forKey: "kanso.sender.visible")
                self.updateSenderStatus()
            }
        }
        senderResizeObserver = NotificationCenter.default.addObserver(forName: NSSplitView.didResizeSubviewsNotification, object: senderSplit.splitView, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.saveSenderHeight() }
        }
        let previousEventHandler = controller.onCoreEvent
        controller.onCoreEvent = { [weak self] event in
            previousEventHandler?(event)
            self?.showTerminalTransfer(event)
            if case .activityChanged(let session, .idle) = event {
                Task { @MainActor [weak self] in
                    guard let self, self.controller.workspace.activeSession == session else { return }
                    self.gitModel.refresh()
                }
            }
        }
        window.delegate = self
        confirmer.window = window
        controller.sessions.window = window
        promptHost.window = window
        commandPalette = CommandPalettePresenter(windowController: self)
        commandPalette.installTitlebarEntry(on: window)
        // 标题栏左侧（红绿灯之后）：收起 / 展开左侧标签栏，与"显示 / 隐藏标签栏"菜单项同一动作。
        sidebarButton.onPress = { [weak self] in self?.kansoToggleTabBar(nil) }
        sidebarButton.toolTip = String(localized: "Hide Tab Bar")
        sidebarButton.setAccessibilityLabel(String(localized: "Hide Tab Bar"))
        let sidebarAccessory = NSTitlebarAccessoryViewController()
        sidebarAccessory.layoutAttribute = .left
        sidebarAccessory.view = NSView(frame: NSRect(x: 0, y: 0, width: 40, height: 28))
        sidebarButton.frame = NSRect(x: 8, y: 2, width: 28, height: 24)
        sidebarAccessory.view.addSubview(sidebarButton)
        window.addTitlebarAccessoryViewController(sidebarAccessory)
        self.sidebarAccessory = sidebarAccessory
        gitButton.onPress = { [weak self] in
            guard let self else { return }
            self.setGitCollapsed(!self.gitItem.isCollapsed)
        }
        // 右侧：分支图标 + 改动统计；附件宽度随统计文字变化。
        let gitAccessory = NSTitlebarAccessoryViewController()
        gitAccessory.layoutAttribute = .right
        let gitHost = NSView(frame: NSRect(x: 0, y: 0, width: gitButton.fittingWidth + 12, height: 28))
        gitAccessory.view = gitHost
        gitButton.frame = NSRect(x: 4, y: 2, width: gitButton.fittingWidth, height: 24)
        gitHost.addSubview(gitButton)
        gitButton.onSizeChange = { [weak gitHost, weak gitButton] width in
            gitHost?.setFrameSize(NSSize(width: width + 12, height: 28))
            gitButton?.frame.size.width = width
        }
        window.addTitlebarAccessoryViewController(gitAccessory)
        let previousGitChange = gitModel.onChange
        gitModel.onChange = { [weak self] in
            previousGitChange?()
            guard let self else { return }
            let summary = self.gitModel.summary
            self.gitButton.update(added: summary?.added, deleted: summary?.deleted)
        }
        controller.promptPresenter = PromptPresenter(
            responder: CorePromptResponder(core: core),
            host: promptHost,
            sessionTitle: { [weak controller] in controller?.promptSessionTitle($0) }
        )

        if stateStore.wasZoomed, !window.isZoomed { window.zoom(nil) }
        applyDefaultSidebarWidthIfNeeded()
        // 在 autosave 恢复侧栏状态之后：顶部模式要压过上次记住的"侧栏展开"。
        applyTabBarPosition(settings.appearance.tabBarPosition)
        settings.observe(self) { [weak self, weak settings] section in
            guard section == .appearance, let self, let settings else { return }
            self.applyTabBarPosition(settings.appearance.tabBarPosition)
        }
        // 产品决定（2026-11-16）：Filer 每次启动都收起，用户需要时再打开；宽度仍由 autosave 记住。
        // 必须在 autosave 恢复之后、loadVisiblePanels 之前：否则上次展开的状态会被恢复并提前构建面板。
        filerItem.isCollapsed = true
        filer.prompter.window = window
        loadVisiblePanels()
        filer.model.isVisible = !filerItem.isCollapsed
        filer.onRequestShow = { [weak self] in self?.setFilerCollapsed(false) }
        filer.onStatusMessage = { [weak self] in self?.statusBar.showNotice($0.text, isError: $0.isError) }
        filer.onStateChange = { [weak self] in self?.updateFilerStatusItem() }
        filerCollapsedObservation = filerItem.observe(\.isCollapsed) { [weak self] _, _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.loadVisiblePanels()
                self.filer.model.isVisible = !self.filerItem.isCollapsed
                self.updateFilerStatusItem()
            }
        }
        statusBar.onToggleFiler = { [weak self] in self?.toggleFilerFromStatusBar() }

        controller.requestTerminalFocus = { [weak paneContainer] in paneContainer?.focusActiveTerminal() }
        controller.errorHandler = { [weak self] error in self?.presentOpenFailure(error) }
        controller.addObserver { [weak self] in self?.modelChanged() }
        // 开关作用于活动会话：AI 会话里只改它自己（ui-shell.md §9）。高亮状态在 `modelChanged` 里按活动会话刷新，
        // 它在每次模型 / 活动变化与全局模式变化（`applyGutterMode` 会 `refresh`）之后都会被调用。
        statusBar.onToggleTimeline = { [weak controller] in controller?.toggleTimeline() }
        statusBar.setTimelineOn(gutter.isTimelineOn)

        // 先把视图布局出来：打开会话要用 pane 的像素尺寸估算网格，不是写死的。
        window.contentView?.layoutSubtreeIfNeeded()
        if !senderItem.isCollapsed { restoreSenderHeight() }
        restoringSenderHeight = false
        PerformanceProbe.mark("window")
        controller.perform { try controller.openLocalShell() }
        PerformanceProbe.mark("shell")
        modelChanged()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("MainWindowController 只支持代码创建")
    }

    /// 普通分栏首次布局完成后再应用默认宽度，避免系统把侧栏拉宽。
    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        if ProcessInfo.processInfo.operatingSystemVersion.majorVersion == 26 {
            splitController.view.layoutSubtreeIfNeeded()
            applyDefaultSidebarWidthIfNeeded()
        }
    }

    /// 创建 core。真实 core 的数据目录是 `~/Library/Application Support/<bundle id>/`，目录不存在由 core 创建。
    private static func makeCore(useMock: Bool, listener: CoreEventDispatcher) throws -> (KansoCore, URL?) {
        if useMock { return (KansoCore.newMock(listener: listener), nil) }
        if let root = PerformanceProbe.root {
            return (try PerformanceProbe.makeCore(root: root, listener: listener), root.appendingPathComponent("data", isDirectory: true))
        }
        let bundleId = Bundle.main.bundleIdentifier ?? "com.chhuax.kanso"
        let support = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: false
        )
        let dataDirectory = support.appendingPathComponent(bundleId, isDirectory: true)
        let config = FfiCoreConfig(
            dataDir: dataDirectory.path,
            appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0",
            bundleId: bundleId,
            preferredLanguage: Locale.preferredLanguages.first ?? "en",
            region: Locale.current.region?.identifier
        )
        // 凭据存入钥匙串，service 用 bundle id，与数据目录同一命名空间。
        return (try KansoCore(config: config, listener: listener, secrets: KeychainSecretStore(service: bundleId)), dataDirectory)
    }

    /// 首次启动（还没有侧栏宽度的 autosave 记录）时把侧栏设成默认宽度；之后以用户上次的宽度为准。
    private func applyDefaultSidebarWidthIfNeeded() {
        let key = "NSSplitView Subview Frames MainSplitView"
        guard defaults.object(forKey: key) == nil else { return }
        splitController.splitView.setPosition(DesignTokens.Spacing.sidebarDefaultWidth, ofDividerAt: 0)
        gitItem.isCollapsed = true
    }

    // MARK: - 标签栏位置

    /// 切换标签栏位置（ui-shell.md §2.5）。位置不变时什么都不做，所以每次外观设置变化都可以放心调用。
    ///
    /// 顶部模式下竖栏保持收起并锁住分隔条，不能被拖出来；切回左侧时总是展开竖栏，
    /// 否则用户切回后看到的是一个收起的竖栏，会以为标签栏不见了。
    func applyTabBarPosition(_ position: TabBarPosition) {
        guard position != tabBarPosition else { return }
        tabBarPosition = position
        let top = position == .top
        splitController.locksFirstDivider = top
        sidebarItem.isCollapsed = top
        sidebarAccessory?.isHidden = top
        paneContainer.showsTabStrips = top && topTabBarVisible
        splitController.splitView.adjustSubviews()
        if !top { sidebarButton.toolTip = String(localized: "Hide Tab Bar") }
    }

    // MARK: - Filer 显隐

    private func loadVisiblePanels() {
        func install(_ panel: @autoclosure () -> NSView, in item: NSSplitViewItem) {
            let container = item.viewController.view
            guard !item.isCollapsed, container.subviews.isEmpty else { return }
            let view = panel()
            view.frame = container.bounds
            view.autoresizingMask = [.width, .height]
            container.addSubview(view)
        }
        install(senderPanel, in: senderItem)
        install(filer.panel, in: filerItem)
        install(gitPanel, in: gitItem)
    }

    var isFilerCollapsed: Bool { filerItem.isCollapsed }
    var isFilerStatusEnabled: Bool { statusBar.isFilerEnabled }
    func clickFilerStatusItem() { statusBar.clickFilerItem() }

    /// 展开 / 收起 Filer。宽度由 `splitView.autosaveName` 跨重启恢复（启动时总是收起）；
    /// 只在从未设置过宽度时，首次展开套用默认宽度。
    func setFilerCollapsed(_ collapsed: Bool) {
        guard collapsed || filer.model.availability != .unavailableSftp else { return }
        guard filerItem.isCollapsed != collapsed else { return }
        filerItem.animator().isCollapsed = collapsed
        if !collapsed, !defaults.bool(forKey: Self.filerWidthInitializedKey) {
            defaults.set(true, forKey: Self.filerWidthInitializedKey)
            let sidebarWidth = sidebarItem.isCollapsed ? 0 : sidebarItem.viewController.view.frame.width
            splitController.splitView.setPosition(sidebarWidth + Self.filerDefaultWidth, ofDividerAt: 1)
        }
        fitAuxiliaryPanels()
    }

    var isGitCollapsed: Bool { gitItem.isCollapsed }

    /// 展开时每次都显式恢复宽度：Git 面板的 holdingPriority 最低，交给分栏自己分配时
    /// 它总是先被压缩，常常只剩最小宽度（2026-11-16 用户反馈"每次打开都太窄"）。
    func setGitCollapsed(_ collapsed: Bool) {
        let split = splitController.splitView
        if collapsed, !gitItem.isCollapsed, gitItem.viewController.view.frame.width >= Self.gitMinWidth {
            defaults.set(Double(gitItem.viewController.view.frame.width), forKey: Self.gitWidthKey)
        }
        gitItem.isCollapsed = collapsed
        loadVisiblePanels()
        if !collapsed {
            split.layoutSubtreeIfNeeded()
            let stored = defaults.double(forKey: Self.gitWidthKey)
            // 给分屏区留出最小宽度，侧栏与 Filer 按当前实际宽度计算。
            let others = 244 + (sidebarItem.isCollapsed ? 0 : sidebarItem.viewController.view.frame.width)
                + (filerItem.isCollapsed ? 0 : filerItem.viewController.view.frame.width)
            let available = split.bounds.width - others
            let target = min(stored > 0 ? stored : Self.gitDefaultWidth, Self.gitMaxWidth, max(Self.gitMinWidth, available))
            split.setPosition(split.bounds.width - target - split.dividerThickness, ofDividerAt: 2)
        }
        fitAuxiliaryPanels()
    }

    private func fitAuxiliaryPanels() {
        let width = splitController.splitView.bounds.width
        func required() -> CGFloat {
            244 + (sidebarItem.isCollapsed ? 0 : sidebarItem.minimumThickness)
                + (filerItem.isCollapsed ? 0 : Self.filerMinWidth)
                + (gitItem.isCollapsed ? 0 : Self.gitMinWidth)
        }
        if required() > width, !filerItem.isCollapsed { filerItem.isCollapsed = true }
        if required() > width, !sidebarItem.isCollapsed { sidebarItem.isCollapsed = true }
        if required() > width, !gitItem.isCollapsed { gitItem.isCollapsed = true }
    }

    func windowDidResize(_ notification: Notification) { fitAuxiliaryPanels() }

    /// 状态栏"文件浏览器"：已展开时收起；隐藏时先尝试定位当前 shell 目录，成功后才展开（filer.md §2 末段）。
    private func toggleFilerFromStatusBar() {
        filerTimingLog.info("状态栏点击文件浏览器，当前\(self.filerItem.isCollapsed ? "收起" : "展开")")
        if !filerItem.isCollapsed {
            setFilerCollapsed(true)
        } else {
            // 先展开再定位：查工作目录是异步的，等它回来再展开会让点击看起来没反应。
            setFilerCollapsed(false)
            filer.locate(session: nil)
        }
    }

    private func updateFilerStatusItem() {
        let sftp = controller.workspace.activeSession.flatMap { controller.workspace.item($0) }?.kind == .sftp
        if sftp {
            if filerCollapsedBeforeSftp == nil { filerCollapsedBeforeSftp = filerItem.isCollapsed }
            if !filerItem.isCollapsed { filerItem.isCollapsed = true }
        } else if let previous = filerCollapsedBeforeSftp {
            filerCollapsedBeforeSftp = nil
            if filerItem.isCollapsed != previous { filerItem.isCollapsed = previous }
        }
        statusBar.setFiler(expanded: !filerItem.isCollapsed, unavailableReason: filer.model.unavailableReason)
    }

    @objc func kansoToggleFiler(_ sender: Any?) {
        // 纯 SFTP 标签的主区已是文件浏览器：不额外展开辅助栏。
        guard filer.model.availability != .unavailableSftp else { return }
        setFilerCollapsed(!filerItem.isCollapsed)
    }

    /// ⌘J / 终端右键菜单："在 Filer 中显示工作目录"。右键菜单项带发起标签的 id（representedObject）。
    @objc func kansoLocateWorkingDirectory(_ sender: Any?) {
        let session = (sender as? NSMenuItem)?.representedObject as? SessionID
        filer.locate(session: session)
    }

    /// ⌘⇧P：命令面板。
    @objc func kansoCommandPalette(_ sender: Any?) {
        commandPalette.toggle()
    }

    // MARK: - 模型变化

    private func modelChanged() {
        let workspace = controller.workspace
        let active = workspace.activeSession.flatMap { workspace.item($0) }
        gitModel.follow(gitReadsEnabled ? active : nil)
        if let active {
            window?.title = TabPresentation.primaryTitle(active, homeDirectory: NSHomeDirectory())
        } else {
            window?.title = baseTitle
        }
        statusBar.setTimelineOn(controller.gutter.isTimelineOn(for: workspace.activeSession))
        statusBar.setStatus(TabDescriptions.statusBarText(for: active))
        statusBar.setLegacySshToolTip(controller.activeLegacyToolTip)
        statusBar.setDirectory(TabDescriptions.directoryText(for: active, homeDirectory: NSHomeDirectory()))
        updateFilerStatusItem()
        senderModel.refresh(context: active, tabs: workspace.sidebarOrder.compactMap { workspace.item($0) })
    }

    private func updateSenderStatus() {
        statusBar.setSenderRunning(senderModel.repeatRunning && !senderModel.isVisible)
    }

    /// MCP 打开的标签放进当前分屏，不切换活动标签，也不把窗口变成 key。
    private func placeMcpSession(_ id: SessionID) {
        controller.placeMcpSession(id)
        guard let window, !window.isVisible else { return }
        window.orderFront(nil)
    }

    private func updateMcpIndicator() {
        statusBar.setMcpClientCount(mcp.supported ? mcp.clients.count : 0)
    }

    /// 打开或前置 MCP 活动记录窗口。入口：菜单栏「窗口 → MCP 活动记录」、状态栏指示器、设置页按钮。
    /// MAS / Setapp 渠道（`mcp.supported` 为假）没有这个窗口，直接忽略。
    func showMcpActivity() {
        guard mcp.supported else { return }
        let controller = mcpActivity ?? McpActivityWindowController(mcp: mcp, autosaveName: mcpActivityAutosaveName)
        mcpActivity = controller
        controller.present()
    }

    /// 已创建的活动记录窗口，供测试确认单例。
    var mcpActivityWindowController: McpActivityWindowController? { mcpActivity }

    var isMcpIndicatorVisible: Bool { statusBar.isMcpIndicatorVisible }
    func clickMcpIndicator() { statusBar.clickMcpIndicator() }

    private func saveSenderHeight() {
        guard !senderItem.isCollapsed, !restoringSenderHeight else { return }
        senderHeight = min(400, max(80, senderPanel.frame.height))
        defaults.set(Double(senderHeight), forKey: "kanso.sender.height")
    }

    private func restoreSenderHeight() {
        senderSplit.splitView.setPosition(max(80, senderSplit.splitView.bounds.height - senderHeight - senderSplit.splitView.dividerThickness), ofDividerAt: 0)
    }

    func setSenderVisible(_ visible: Bool) {
        if !visible { saveSenderHeight() }
        restoringSenderHeight = true
        senderItem.isCollapsed = !visible
        senderModel.isVisible = visible
        if visible {
            senderSplit.view.layoutSubtreeIfNeeded()
            restoreSenderHeight()
            window?.makeFirstResponder(senderPanel.editor)
        }
        restoringSenderHeight = false
        updateSenderStatus()
    }

    private func presentOpenFailure(_ error: Error) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "Failed to open the session")
        // core 的用户错误按语言渲染；其余技术细节是英文，不本地化（只用于排查）。
        alert.informativeText = CoreMessageText.text(for: error)
        if let window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    // MARK: - 退出与关窗确认（§8）

    /// 退出 / 关窗是否需要弹确认。
    var requiresQuitConfirmation: Bool {
        !quitConfirmed && (controller.workspace.closeConfirmation(for: controller.workspace.sidebarOrder) != nil || filer.hasRemoteEdits)
    }

    /// 确认退出（所有会话合并成一次）。用户同意后记住，之后不再询问。
    func confirmQuit() async -> Bool {
        if quitConfirmed { return true }
        guard await controller.confirmQuit() else { return false }
        quitConfirmed = true
        return true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard requiresQuitConfirmation else { return true }
        // 确认是异步的 sheet：先拒绝这次关闭，用户同意后再主动关窗；关窗会让应用退出，
        // 退出流程里 `quitConfirmed` 已为 true，不会再问第二次。
        Task { @MainActor [weak self, weak sender] in
            guard let self, await confirmQuit() else { return }
            sender?.close()
        }
        return false
    }

    func windowWillClose(_ notification: Notification) {
        // App 靠「最后一个窗口关闭」退出（`applicationShouldTerminateAfterLastWindowClosed`）。
        // 活动记录窗口是独立的可见窗口，不先关掉的话主窗口关闭后它还在，App 不退出，
        // `shutdown()` 不执行，SSH 会话和 MCP 服务继续运行。
        mcpActivity?.close()
        saveWindowState()
        senderModel.shutdown()
    }

    private func saveWindowState() {
        saveSenderHeight()
        if let window { stateStore.save(isZoomed: window.isZoomed) }
    }

    /// 应用退出前调用：保存窗口状态，通知 core 关闭会话并收尾（最多等几秒，不会卡住退出）。
    func shutdown() {
        mcp.stop()
        // 窗口已关闭时状态已在 windowWillClose 里保存；关闭后的 isZoomed 不可靠，不再覆盖。
        if window?.isVisible == true { saveWindowState() }
        filer.shutdown()
        gitModel.shutdown()
        senderModel.shutdown()
        if let senderResizeObserver { NotificationCenter.default.removeObserver(senderResizeObserver) }
        controller.shutdown()
    }

    // MARK: - 菜单动作（走 responder 链，target 为 nil）

    @objc func kansoNewLocalShell(_ sender: Any?) {
        controller.perform { try controller.openLocalShell() }
    }

    @objc func kansoOpenLauncher(_ sender: Any?) {
        guard tabBarPosition == .top else {
            if sidebarItem.isCollapsed {
                sidebarItem.isCollapsed = false
                sidebarButton.toolTip = String(localized: "Hide Tab Bar")
            }
            (sidebarItem.viewController as? TabSidebarViewController)?.showSessionLauncher()
            return
        }
        // 顶部模式：竖栏收起，它的 ＋ 不在屏幕上，锚到活动分屏的标签条；标签条也隐藏时锚在分屏区顶部中间。
        if !paneContainer.showSessionLauncher() {
            controller.sessions.showLauncher(
                relativeTo: NSRect(x: paneContainer.bounds.midX, y: 0, width: 1, height: 1), of: paneContainer, preferredEdge: .maxY
            )
        }
    }

    @objc func kansoNewSession(_ sender: Any?) {
        controller.sessions.presentEditor(editing: nil)
    }

    @objc func kansoImportSshConfig(_ sender: Any?) { controller.sessions.presentSshImport() }
    @objc func kansoExportData(_ sender: Any?) { controller.sessions.exportData() }
    @objc func kansoImportData(_ sender: Any?) { controller.sessions.importData() }

    @objc func kansoCancelTerminalTransfer(_ sender: Any?) {
        guard let session = controller.workspace.activeSession else { return }
        controller.core.cancelTerminalTransfer(sessionId: session)
    }

    @objc func kansoXmodemSend(_ sender: Any?) { startXmodem(.bytes128) }
    @objc func kansoXmodem1KSend(_ sender: Any?) { startXmodem(.bytes1K) }
    @objc func kansoXmodemReceive(_ sender: Any?) { startXmodem(nil) }

    private func startXmodem(_ block: FfiXmodemBlock?) {
        guard let session = controller.workspace.activeSession else { return }
        controller.perform { _ = try controller.core.xmodemStart(sessionId: session, block: block) }
    }

    private func showTerminalTransfer(_ event: FfiCoreEvent) {
        switch event {
        case .notice(_, let level, let message):
            statusBar.showNotice(CoreMessageText.text(message), isError: level == .error)
        case .transferStarted(let id, _, let proto, let direction, _, _) where proto == .zmodem || proto == .xmodem:
            terminalTransfers[id] = (direction, proto)
        case .transferProgress(let id, let done, let total) where terminalTransfers[id] != nil:
            let progress = total.map { "\(done) / \($0) B" } ?? "\(done) B"
            let proto = terminalTransfers[id]!.1 == .xmodem ? "XMODEM" : "ZMODEM"
            statusBar.showNotice("\(proto) · \(progress)", isError: false)
        case .transferFinished(let id, let outcome):
            guard let (direction, proto) = terminalTransfers.removeValue(forKey: id) else { return }
            switch outcome {
            case .failed(let message, _):
                statusBar.showNotice(CoreMessageText.text(message), isError: true)
            case .cancelled: break
            case .completed:
                if direction == .upload || proto == .xmodem {
                    statusBar.showNotice(String(localized: "Done"), isError: false)
                }
            }
        default: break
        }
    }

    @objc func kansoCloseSession(_ sender: Any?) {
        // 没有标签时 ⌘W 关闭窗口。
        guard let active = controller.workspace.activeSession else {
            window?.performClose(nil)
            return
        }
        Task { @MainActor [controller] in await controller.requestClose([active]) }
    }

    @objc func kansoPreviousSession(_ sender: Any?) { controller.workspace.selectPreviousTabInPane() }
    @objc func kansoNextSession(_ sender: Any?) { controller.workspace.selectNextTabInPane() }
    @objc func kansoPreviousSplit(_ sender: Any?) { controller.workspace.selectPreviousPane() }
    @objc func kansoNextSplit(_ sender: Any?) { controller.workspace.selectNextPane() }

    /// ⌘1–⌘9：菜单项的 `tag` 是侧栏序号（从 0 开始）。
    @objc func kansoSelectTab(_ sender: NSMenuItem) {
        controller.workspace.selectTab(atSidebarIndex: sender.tag)
    }

    @objc func kansoClearBuffer(_ sender: Any?) { controller.clearActiveBuffer() }

    private var activeFindBar: FindBarController? {
        controller.workspace.activeSession.flatMap { controller.host(for: $0)?.findBar }
    }

    @objc func kansoFind(_ sender: Any?) { activeFindBar?.show() }
    @objc func kansoFindNext(_ sender: Any?) { activeFindBar?.findNext() }
    @objc func kansoFindPrevious(_ sender: Any?) { activeFindBar?.findPrevious() }

    @objc func kansoCommandSuggestions(_ sender: Any?) {
        controller.settings.updateCompletion { $0.enabled.toggle() }
    }

    @objc func kansoCopyOnSelect(_ sender: Any?) {
        controller.settings.updateMouse { $0.copyOnSelect.toggle() }
    }

    @objc func kansoRightClickShowsMenu(_ sender: Any?) {
        controller.settings.updateMouse { $0.rightClick = $0.rightClick == .menu ? .copyOrPaste : .menu }
    }

    @objc func kansoClearCommandHistory(_ sender: Any?) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = String(localized: "Clear Command History?")
        alert.informativeText = String(localized: "Clear recorded command history from all terminals. This can’t be undone.")
        alert.addButton(withTitle: String(localized: "Clear"))
        alert.addButton(withTitle: String(localized: "Cancel"))
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            do {
                try self.controller.core.clearHistory()
                for host in self.controller.hosts.values { host.completion.invalidate() }
            } catch {
                self.statusBar.showNotice(CoreMessageText.text(for: error), isError: true)
            }
        }
    }
    @objc func kansoSplitRight(_ sender: Any?) { controller.perform { try controller.split(axis: .horizontal) } }
    @objc func kansoSplitDown(_ sender: Any?) { controller.perform { try controller.split(axis: .vertical) } }

    @objc func kansoToggleTabBar(_ sender: Any?) {
        if tabBarPosition == .top {
            topTabBarVisible.toggle()
            defaults.set(topTabBarVisible, forKey: Self.topTabBarVisibleKey)
            paneContainer.showsTabStrips = topTabBarVisible
            return
        }
        // 收起动画结束前 isCollapsed 还是旧值，提示文字按切换后的状态预先算好。
        let willCollapse = !sidebarItem.isCollapsed
        if usesPlainSidebar {
            sidebarItem.animator().isCollapsed = willCollapse
        } else {
            splitController.toggleSidebar(sender)
        }
        sidebarButton.toolTip = willCollapse ? String(localized: "Show Tab Bar") : String(localized: "Hide Tab Bar")
    }

    @objc func kansoToggleSender(_ sender: Any?) { setSenderVisible(senderItem.isCollapsed) }

    // 文本缩放直接改"显示"设置里的终端字号：所有窗口的终端一起变，并且记住。
    // 不做按标签的临时缩放，免得同一个字号有设置值和缩放值两套来源。
    @objc func kansoIncreaseFontSize(_ sender: Any?) { stepFontSize(by: 1) }
    @objc func kansoDecreaseFontSize(_ sender: Any?) { stepFontSize(by: -1) }
    @objc func kansoResetFontSize(_ sender: Any?) {
        controller.settings.updateDisplay { $0.fontSize = DisplaySettings().fontSize }
    }

    private func stepFontSize(by delta: Double) {
        controller.settings.updateDisplay {
            let range = DisplaySettings.fontSizeRange
            $0.fontSize = min(max($0.fontSize.rounded() + delta, range.lowerBound), range.upperBound)
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        let workspace = controller.workspace
        switch menuItem.action {
        case #selector(kansoCommandSuggestions(_:)):
            menuItem.state = controller.settings.completion.enabled ? .on : .off
            return true
        case #selector(kansoCopyOnSelect(_:)):
            menuItem.state = controller.settings.mouse.copyOnSelect ? .on : .off
            return true
        case #selector(kansoRightClickShowsMenu(_:)):
            menuItem.state = controller.settings.mouse.rightClick == .menu ? .on : .off
            return true
        case #selector(kansoToggleSender(_:)):
            menuItem.title = senderItem.isCollapsed ? String(localized: "Show Sender") : String(localized: "Hide Sender")
            return true
        case #selector(kansoPreviousSession(_:)), #selector(kansoNextSession(_:)):
            return workspace.tabs(in: workspace.activePane).count > 1
        case #selector(kansoPreviousSplit(_:)), #selector(kansoNextSplit(_:)):
            return workspace.allPanes.count > 1
        case #selector(kansoSelectTab(_:)):
            return menuItem.tag < workspace.sidebarOrder.count
        case #selector(kansoClearBuffer(_:)), #selector(kansoFind(_:)),
             #selector(kansoFindNext(_:)), #selector(kansoFindPrevious(_:)):
            return workspace.activeSession != nil
        case #selector(kansoToggleFiler(_:)):
            menuItem.title = filerItem.isCollapsed ? String(localized: "Show File Browser") : String(localized: "Hide File Browser")
            return filer.model.availability != .unavailableSftp
        case #selector(kansoLocateWorkingDirectory(_:)):
            guard let session = workspace.activeSession, let item = workspace.item(session) else { return false }
            return item.kind != .sftp
        case #selector(kansoToggleTabBar(_:)):
            let hidden = tabBarPosition == .top ? !topTabBarVisible : sidebarItem.isCollapsed
            menuItem.title = hidden ? String(localized: "Show Tab Bar") : String(localized: "Hide Tab Bar")
            return true
        default:
            return true
        }
    }
}

/// 右侧内容区：分屏区 + 底部状态栏。
@MainActor
final class ShellContentView: NSView {
    private let panes: NSView
    private let statusBar: NSView

    override var isFlipped: Bool { true }

    init(panes: NSView, statusBar: NSView) {
        self.panes = panes
        self.statusBar = statusBar
        super.init(frame: .zero)
        addSubview(panes)
        addSubview(statusBar)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("ShellContentView 只支持代码创建") }

    override func layout() {
        super.layout()
        let h = DesignTokens.Spacing.statusBarHeight
        statusBar.frame = NSRect(x: 0, y: bounds.height - h, width: bounds.width, height: h)
        panes.frame = NSRect(x: 0, y: 0, width: bounds.width, height: max(0, bounds.height - h))
    }
}

/// 主窗口的横向分栏。顶部标签条模式下锁住第一条分隔条（竖栏与 Filer 之间），
/// 收起的竖栏就不会被用户从窗口左边缘拖出来。
@MainActor
final class MainSplitViewController: NSSplitViewController {
    var locksFirstDivider = false {
        didSet {
            guard locksFirstDivider != oldValue else { return }
            splitView.needsDisplay = true
            splitView.window?.invalidateCursorRects(for: splitView)
        }
    }

    override func splitView(_ splitView: NSSplitView, effectiveRect proposedEffectiveRect: NSRect, forDrawnRect drawnRect: NSRect, ofDividerAt dividerIndex: Int) -> NSRect {
        if locksFirstDivider, dividerIndex == 0 { return .zero }
        return super.splitView(splitView, effectiveRect: proposedEffectiveRect, forDrawnRect: drawnRect, ofDividerAt: dividerIndex)
    }

    override func splitView(_ splitView: NSSplitView, shouldHideDividerAt dividerIndex: Int) -> Bool {
        if locksFirstDivider, dividerIndex == 0 { return true }
        return super.splitView(splitView, shouldHideDividerAt: dividerIndex)
    }
}
