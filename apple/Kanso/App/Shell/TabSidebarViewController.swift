import AppKit
import KansoUI

/// 左侧栏：顶部固定「标签 / 会话」双段切换；标签页为搜索框 + ＋ 按钮 + 实时标签列表，会话页嵌入已保存会话管理。
@MainActor
final class TabSidebarViewController: NSViewController, NSTextFieldDelegate {
    enum Page { case tabs, sessions }

    private let controller: WorkspaceController
    private var workspace: Workspace { controller.workspace }
    private lazy var contextMenu = TabContextMenu(controller: controller)
    /// 标签上的 MCP 标记。由主窗口注入。
    var mcpMark: (SessionID) -> McpTabMark? = { _ in nil }
    private(set) var activePage: Page = .tabs

    private let modeSwitch = SidebarModeSwitch()
    private let searchField = SidebarFilterField()
    private let plusButton = NSButton()
    private let scrollView = NSScrollView()
    private let list = SidebarListView()
    private let emptyLabel = NSTextField(labelWithString: "")
    private var sessionsController: SessionLauncherViewController?
    private var lastActiveSession: SessionID?
    /// 拖拽期间因结构变化被推迟的重建。
    private var needsRebuild = false

    init(controller: WorkspaceController) {
        self.controller = controller
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("TabSidebarViewController 只支持代码创建") }

    /// 当前搜索词（去掉首尾空白前的原文）。
    var query: String { searchField.stringValue }

    var listView: SidebarListView { list }

    override func loadView() {
        let root = SidebarRootView()
        view = root

        modeSwitch.onSelect = { [weak self] page in
            switch page {
            case .tabs: self?.showTabsPage()
            case .sessions: self?.showSessionsPage(focusSearch: false)
            }
        }

        searchField.placeholder = String(localized: "Filter by path")
        searchField.layer?.cornerRadius = DesignTokens.Radius.sm
        searchField.textField.delegate = self
        searchField.onClear = { [weak self] in self?.refresh() }

        plusButton.isBordered = false
        plusButton.image = .symbol("plus", description: String(localized: "New Session…"))
        plusButton.imagePosition = .imageOnly
        plusButton.contentTintColor = .token(DesignTokens.Color.textSecondary)
        plusButton.toolTip = String(localized: "New Session…")
        plusButton.target = self
        plusButton.action = #selector(newSession)

        emptyLabel.stringValue = String(localized: "No matching paths")
        emptyLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        emptyLabel.textColor = .token(DesignTokens.Color.textTertiary)
        emptyLabel.alignment = .center
        emptyLabel.isHidden = true

        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.documentView = list
        list.autoresizingMask = [.width]

        for v in [scrollView, emptyLabel, searchField, plusButton, modeSwitch] { root.addSubview(v) }
        root.onLayout = { [weak self] in self?.layoutContents() }

        list.onActivate = { [weak self] id in
            self?.workspace.activate(id)
            self?.controller.requestTerminalFocus?()
        }
        list.onActivatePane = { [weak self] pane in
            self?.workspace.activatePane(pane)
            self?.controller.requestTerminalFocus?()
        }
        list.onClose = { [weak self] ids in self?.close(ids) }
        list.onRename = { [weak self] id in self?.contextMenu.rename(id) }
        list.onMove = { [weak self] id, index in self?.workspace.moveTab(id, toIndex: index) }
        list.onBlankDoubleClick = { [weak self] in
            guard let self else { return }
            controller.perform { try controller.duplicateActiveTab() }
        }
        list.rowMenuProvider = { [weak self] id in self?.makeRowMenu(for: id) }
        list.onDragFinished = { [weak self] in
            guard let self, needsRebuild else { return }
            refresh()
        }
        controller.addObserver { [weak self] in self?.refresh() }
        refresh()
    }

    private func layoutContents() {
        let w = view.bounds.width
        let pad = DesignTokens.Spacing.sm
        // 标题栏透明并与内容融合：工具条要避开红绿灯所在区域。
        let top = max(view.safeAreaInsets.top, 0) + DesignTokens.Spacing.xs
        let switchH: CGFloat = 26
        modeSwitch.frame = NSRect(x: pad, y: top, width: max(0, w - pad * 2), height: switchH)
        let contentTop = modeSwitch.frame.maxY + DesignTokens.Spacing.sm
        let toolbarH: CGFloat = 24
        let plusW: CGFloat = 24
        plusButton.frame = NSRect(x: w - pad - plusW, y: contentTop, width: plusW, height: toolbarH)
        searchField.frame = NSRect(x: pad, y: contentTop, width: max(0, plusButton.frame.minX - pad - DesignTokens.Spacing.xs), height: toolbarH)
        let listTop = contentTop + toolbarH + DesignTokens.Spacing.sm
        scrollView.frame = NSRect(x: 0, y: listTop, width: w, height: max(0, view.bounds.height - listTop))
        emptyLabel.frame = NSRect(x: pad, y: listTop + DesignTokens.Spacing.xl, width: max(0, w - pad * 2), height: 18)
        sessionsController?.view.frame = NSRect(x: 0, y: contentTop, width: w, height: max(0, view.bounds.height - contentTop))
        syncListFrame()
    }

    /// 文档视图至少和可视区一样高：空白处才能接收双击 / 右键。
    private func syncListFrame() {
        let visible = scrollView.contentSize
        list.setFrameSize(NSSize(width: visible.width, height: max(list.contentHeight, visible.height)))
    }

    // MARK: - 刷新

    /// 按模型与筛选词更新列表。
    func refresh() {
        let q = searchField.stringValue
        let filtering = !q.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let order = workspace.sidebarOrder
        let indexes = Dictionary(uniqueKeysWithValues: order.enumerated().map { ($0.element, $0.offset) })
        let showHeaders = workspace.showsSectionHeaders
        let activePane = workspace.activePane
        let sections: [SidebarListView.Section] = workspace.filteredSidebar(q).compactMap { section in
            // 筛选后整组为空的组隐藏；没有筛选时空组只有"唯一的空 pane"，同样不画。
            guard !section.tabs.isEmpty else { return nil }
            let activeTab = workspace.activeTab(in: section.pane)
            let rows = section.tabs.map { item -> TabRowView.Model in
                let selection: TabRowSelection = item.id == activeTab ? (section.pane == activePane ? .windowActive : .paneActive) : .none
                return TabRowView.Model(
                    item: item, sidebarIndex: indexes[item.id] ?? 0, selection: selection, mcp: mcpMark(item.id)
                )
            }
            return SidebarListView.Section(
                pane: section.pane, titleIndex: section.titleIndex, showsHeader: showHeaders,
                isActivePane: section.pane == activePane, rows: rows
            )
        }
        list.allowsReorder = !filtering
        needsRebuild = !list.update(sections)
        let showTabs = activePage == .tabs
        emptyLabel.isHidden = !(showTabs && filtering && sections.isEmpty)
        let hasUnread = order.contains { workspace.item($0)?.unreadDone == true }
        modeSwitch.update(activePage: activePage, tabCount: order.count, hasUnread: hasUnread)
        view.needsLayout = true
        list.needsLayout = true
        list.layoutSubtreeIfNeeded()
        syncListFrame()

        // 活动标签变化（点击 / 快捷键 / 新开）时自动滚动到可见。
        let active = workspace.activeSession
        if active != lastActiveSession {
            lastActiveSession = active
            if let active {
                list.layoutSubtreeIfNeeded()
                list.scrollToVisible(session: active)
            }
        }
    }

    // MARK: - 动作

    @objc private func newSession() {
        controller.sessions.presentEditor(editing: nil)
    }

    /// 切换到「标签」页。
    func showTabsPage() {
        setActivePage(.tabs, focusSessionsSearch: false)
    }

    /// 切换到「会话」页，并刷新会话与 MCP 状态。
    func showSessionsPage(focusSearch: Bool = true) {
        setActivePage(.sessions, focusSessionsSearch: focusSearch)
    }

    /// 菜单「会话启动器」入口：切到侧栏「会话」页并聚焦搜索框。
    func showSessionLauncher() {
        showSessionsPage(focusSearch: true)
    }

    private func setActivePage(_ page: Page, focusSessionsSearch: Bool) {
        activePage = page
        let showTabs = page == .tabs
        searchField.isHidden = !showTabs
        plusButton.isHidden = !showTabs
        scrollView.isHidden = !showTabs
        if !showTabs {
            let vc = ensureSessionsController()
            vc.view.isHidden = false
            vc.reloadData()
            view.needsLayout = true
            view.layoutSubtreeIfNeeded()
            if focusSessionsSearch { vc.focusSearchField() }
        } else {
            sessionsController?.view.isHidden = true
        }
        refresh()
    }

    private func ensureSessionsController() -> SessionLauncherViewController {
        if let sessionsController { return sessionsController }
        let vc = controller.sessions.makeEmbeddedSessionsViewController(
            onDidOpen: { [weak self] in self?.showTabsPage() },
            onDismiss: { [weak self] in self?.controller.requestTerminalFocus?() }
        )
        addChild(vc)
        view.addSubview(vc.view)
        sessionsController = vc
        return vc
    }

    func controlTextDidChange(_ obj: Notification) { refresh() }

    /// Esc 清空搜索框。
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        searchField.stringValue = ""
        refresh()
        return true
    }

    private func close(_ ids: [SessionID]) { contextMenu.close(ids) }

    // MARK: - 右键菜单

    func makeRowMenu(for session: SessionID) -> NSMenu {
        contextMenu.makeMenu(for: session, horizontal: false)
    }
}

/// 侧栏根视图：背景 `bg_sidebar`，布局交给控制器。
@MainActor
final class SidebarRootView: NSView {
    var onLayout: (() -> Void)?

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SidebarRootView 只支持代码创建") }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgSidebar))
    }

    override func layout() {
        super.layout()
        onLayout?()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}

/// 左侧栏顶部的「标签 / 会话」双段切换栏：小圆角（4pt 外框 / 3pt 选中块），与侧栏克制风格一致。
@MainActor
private final class SidebarModeSwitch: NSView {
    var onSelect: ((TabSidebarViewController.Page) -> Void)?

    private var activePage: TabSidebarViewController.Page = .tabs
    private var tabCount = 0
    private var hasUnread = false

    private let activePill = StyledBoxView()
    private let tabsLabel = PassthroughLabel(labelWithString: String(localized: "Tabs"))
    private let countLabel = PassthroughLabel(labelWithString: "")
    private let unreadDot = NSView()
    private let sessionsLabel = PassthroughLabel(labelWithString: String(localized: "Sessions"))

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = DesignTokens.Radius.sm
        layer?.borderWidth = 1

        activePill.cornerRadius = DesignTokens.Radius.sm - 1
        activePill.fillToken = DesignTokens.Color.bgTabSelected
        activePill.borderToken = DesignTokens.Color.borderSubtle
        activePill.borderWidth = 1

        tabsLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: .medium)
        countLabel.font = .monospacedDigitSystemFont(ofSize: 10, weight: .regular)
        countLabel.textColor = .token(DesignTokens.Color.textTertiary)
        sessionsLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: .medium)

        unreadDot.wantsLayer = true
        unreadDot.layer?.cornerRadius = 3
        unreadDot.isHidden = true

        for v in [activePill, tabsLabel, countLabel, unreadDot, sessionsLabel] { addSubview(v) }
        applyState()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SidebarModeSwitch 只支持代码创建") }

    func update(activePage: TabSidebarViewController.Page, tabCount: Int, hasUnread: Bool) {
        self.activePage = activePage
        self.tabCount = tabCount
        self.hasUnread = hasUnread
        applyState()
        needsLayout = true
    }

    private func applyState() {
        let onTabs = activePage == .tabs
        tabsLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: onTabs ? .semibold : .medium)
        tabsLabel.textColor = .token(onTabs ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary)
        sessionsLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: onTabs ? .medium : .semibold)
        sessionsLabel.textColor = .token(onTabs ? DesignTokens.Color.textSecondary : DesignTokens.Color.textPrimary)
        countLabel.stringValue = tabCount > 0 ? "\(tabCount)" : ""
        countLabel.isHidden = tabCount == 0
        // 只在停在「会话」页且有后台未读完成时点亮圆点；停在「标签」页时标签行本身已显示状态。
        unreadDot.isHidden = onTabs || !hasUnread
        needsDisplay = true
    }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgField))
        layer?.borderColor = resolvedCGColor(.token(DesignTokens.Color.borderField))
        unreadDot.layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.statusUnreadDone))
    }

    override func layout() {
        super.layout()
        let inset: CGFloat = 2
        let halfW = max(0, (bounds.width - inset * 2) / 2)
        let pillH = max(0, bounds.height - inset * 2)
        let pillX = activePage == .tabs ? inset : inset + halfW
        activePill.frame = NSRect(x: pillX, y: inset, width: halfW, height: pillH)

        // 左侧「标签」文字 + 计数 + 未读圆点整体水平居中。
        let tSize = tabsLabel.intrinsicContentSize
        let cSize = countLabel.isHidden ? .zero : countLabel.intrinsicContentSize
        let dotW: CGFloat = unreadDot.isHidden ? 0 : 6
        let cGap: CGFloat = countLabel.isHidden ? 0 : 4
        let dGap: CGFloat = unreadDot.isHidden ? 0 : 4
        let groupW = ceil(tSize.width) + cGap + ceil(cSize.width) + dGap + dotW
        var x = inset + (halfW - groupW) / 2
        let tH = ceil(tSize.height)
        tabsLabel.frame = NSRect(x: x, y: (bounds.height - tH) / 2, width: ceil(tSize.width), height: tH)
        x = tabsLabel.frame.maxX
        if !countLabel.isHidden {
            let cH = ceil(cSize.height)
            countLabel.frame = NSRect(x: x + cGap, y: (bounds.height - cH) / 2 + 0.5, width: ceil(cSize.width), height: cH)
            x = countLabel.frame.maxX
        }
        if !unreadDot.isHidden {
            unreadDot.frame = NSRect(x: x + dGap, y: (bounds.height - 6) / 2, width: 6, height: 6)
        }

        let sSize = sessionsLabel.intrinsicContentSize
        let sW = ceil(sSize.width)
        let sH = ceil(sSize.height)
        sessionsLabel.frame = NSRect(x: inset + halfW + (halfW - sW) / 2, y: (bounds.height - sH) / 2, width: sW, height: sH)
    }

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        let target: TabSidebarViewController.Page = p.x < bounds.width / 2 ? .tabs : .sessions
        guard target != activePage else { return }
        onSelect?(target)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}
