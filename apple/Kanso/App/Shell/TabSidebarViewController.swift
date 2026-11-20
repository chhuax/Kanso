import AppKit
import KansoUI

/// 竖向标签栏：顶部固定工具条（搜索框 + ＋ 按钮）+ 可滚动的标签列表。
@MainActor
final class TabSidebarViewController: NSViewController, NSTextFieldDelegate {
    private let controller: WorkspaceController
    private var workspace: Workspace { controller.workspace }
    private lazy var contextMenu = TabContextMenu(controller: controller)
    /// 标签上的 MCP 标记。由主窗口注入。
    var mcpMark: (SessionID) -> McpTabMark? = { _ in nil }

    private let searchField = SidebarFilterField()
    private let plusButton = NSButton()
    private let scrollView = NSScrollView()
    private let list = SidebarListView()
    private let emptyLabel = NSTextField(labelWithString: "")
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

        searchField.placeholder = String(localized: "Filter by path")
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

        for v in [scrollView, emptyLabel, searchField, plusButton] { root.addSubview(v) }
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
        list.onMove = { [weak self] id, index in self?.workspace.moveTab(id, toIndex: index) }
        list.onBlankDoubleClick = { [weak self] in
            guard let self else { return }
            controller.perform { try controller.duplicateActiveTab() }
        }
        // 空白处右键弹出与 ＋ 按钮相同的会话启动器，锚在点击位置。
        list.onBlankRightClick = { [weak self] point in
            guard let self else { return }
            controller.sessions.showLauncher(
                relativeTo: NSRect(x: point.x, y: point.y, width: 1, height: 1), of: list, preferredEdge: .maxY
            )
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
        let toolbarH: CGFloat = 24
        let plusW: CGFloat = 24
        plusButton.frame = NSRect(x: w - pad - plusW, y: top, width: plusW, height: toolbarH)
        searchField.frame = NSRect(x: pad, y: top, width: max(0, plusButton.frame.minX - pad - DesignTokens.Spacing.xs), height: toolbarH)
        let listTop = top + toolbarH + DesignTokens.Spacing.sm
        scrollView.frame = NSRect(x: 0, y: listTop, width: w, height: max(0, view.bounds.height - listTop))
        emptyLabel.frame = NSRect(x: pad, y: listTop + DesignTokens.Spacing.xl, width: max(0, w - pad * 2), height: 18)
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
        emptyLabel.isHidden = !(filtering && sections.isEmpty)
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

    /// 快捷键打开启动器，锚在 ＋ 按钮上。按钮是非 flipped 视图，下方是 minY。
    func showSessionLauncher() {
        controller.sessions.showLauncher(relativeTo: plusButton.bounds, of: plusButton, preferredEdge: .minY)
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
