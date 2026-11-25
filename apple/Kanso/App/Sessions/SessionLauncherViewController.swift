import AppKit
import KansoBridge
import KansoUI

/// 启动器对外的动作出口，由 `SessionPresenter` 实现。视图控制器只管界面与数据，不直接开会话 / 开 sheet。
struct SessionLauncherActions {
    var newLocalShell: () -> Void
    var newSession: () -> Void
    var connect: (String) -> Void
    var edit: (FfiProfile) -> Void
    var dismiss: () -> Void
    /// "已保存的会话"菜单 → 导入 OpenSSH 配置…。
    var importSshConfig: () -> Void = {}
    /// 右上角齿轮：打开设置窗口。
    var openSettings: () -> Void
    /// 需要弹确认框 / 输入框的操作：弹 sheet 会让主窗口失焦、面板随之关闭，
    /// 所以先关面板、执行完再按原锚点重新打开。
    var withDialog: (@escaping @MainActor () async -> Void) -> Void
}

/// 会话启动器浮层内容（visual-style §3.4）：顶部两个列表式操作行 + 右上角齿轮 + 搜索框 + 按分组显示的已保存会话。
///
/// 放在 `FloatingPanel` 里，不是 `NSViewController` 的内容控制器关系；面板的高度随会话数量变化，
/// 通过 `onPreferredSizeChange` 通知呈现方调整。
@MainActor
final class SessionLauncherViewController: NSViewController, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    private enum Item {
        case header(FfiGroup)
        case profile(FfiProfile)
    }

    static let width: CGFloat = 360
    static let maxHeight: CGFloat = 480
    static let profileRowHeight: CGFloat = 52
    static let headerRowHeight: CGFloat = 30

    private static let topPadding: CGFloat = 8
    private static let actionRowHeight: CGFloat = 34
    private static let searchHeight: CGFloat = DesignTokens.Spacing.fieldHeight
    private static let savedHeaderHeight: CGFloat = 30
    private static let bottomPadding: CGFloat = 8
    private static let emptyListHeight: CGFloat = 72

    /// 面板期望的尺寸变了（会话增删、搜索过滤后）。
    var onPreferredSizeChange: ((NSSize) -> Void)?

    private let model: SessionLauncherModel
    private let dialogs: SessionDialogs
    private let actions: SessionLauncherActions
    /// 嵌入左侧栏「会话」页时为 true：铺满父容器、采用紧凑边距与小圆角，顶部搜索框右侧放 ＋ 按钮。
    private let embedded: Bool

    private let newShellRow: LauncherActionRow
    private let newSessionRow = LauncherActionRow(title: String(localized: "New Session…"), symbol: "plus")
    private let settingsButton = KButton(symbol: "slider.horizontal.3", style: .icon)
    private let plusButton = NSButton()
    let searchField = SidebarFilterField()
    private let savedHeader: LauncherHeaderView
    private let tableView = NSTableView()
    private let scrollView = NSScrollView()
    private let emptyLabel = NSTextField(wrappingLabelWithString: "")
    private var items: [Item] = []

    init(model: SessionLauncherModel, dialogs: SessionDialogs, actions: SessionLauncherActions, embedded: Bool = false) {
        self.model = model
        self.dialogs = dialogs
        self.actions = actions
        self.embedded = embedded
        self.newShellRow = LauncherActionRow(title: String(localized: "Local Shell"), symbol: "terminal", compact: embedded)
        self.savedHeader = LauncherHeaderView(title: String(localized: "Saved Sessions"), compact: embedded)
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SessionLauncherViewController 只支持代码创建") }

    override func loadView() {
        let root = LauncherRootView(frame: NSRect(origin: .zero, size: NSSize(width: Self.width, height: Self.maxHeight)))
        root.onLayout = { [weak self] in self?.layoutContents() }
        view = root

        newShellRow.onClick = { [weak self] in self?.actions.newLocalShell() }
        newSessionRow.onClick = { [weak self] in self?.actions.newSession() }
        settingsButton.toolTip = String(localized: "Customize Launch")
        settingsButton.onPress = { [weak self] in self?.actions.openSettings() }

        searchField.placeholder = String(localized: "Search saved sessions")
        searchField.textField.font = .systemFont(ofSize: embedded ? DesignTokens.Font.uiCaption : DesignTokens.Font.uiBody)
        if embedded { searchField.layer?.cornerRadius = DesignTokens.Radius.sm }
        searchField.textField.delegate = self
        searchField.onClear = { [weak self] in
            self?.model.query = ""
            self?.rebuild(revealSelection: false)
        }

        if embedded {
            newSessionRow.isHidden = true
            plusButton.isBordered = false
            plusButton.image = .symbol("plus", description: String(localized: "New Session…"))
            plusButton.imagePosition = .imageOnly
            plusButton.contentTintColor = .token(DesignTokens.Color.textSecondary)
            plusButton.toolTip = String(localized: "New Session…")
            plusButton.target = self
            plusButton.action = #selector(plusClicked)
        } else {
            plusButton.isHidden = true
        }

        savedHeader.onMenu = { [weak self] anchor in self?.showSavedMenu(from: anchor) }

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main"))
        tableView.addTableColumn(column)
        tableView.headerView = nil
        // 必须用 .plain：macOS 11 起默认的 .automatic 在侧边栏 / 浮层里会变成 .inset，
        // 给每行左右再加一段内边距，行宽超出面板、内容与上方标题错位。
        tableView.style = .plain
        column.resizingMask = .autoresizingMask
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.backgroundColor = .clear
        tableView.selectionHighlightStyle = .none
        tableView.intercellSpacing = .zero
        tableView.dataSource = self
        tableView.delegate = self
        scrollView.documentView = tableView
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay

        emptyLabel.stringValue = String(localized: "No saved sessions yet. Choose “New Session…” to add one.")
        emptyLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        emptyLabel.textColor = .token(DesignTokens.Color.textTertiary)
        emptyLabel.alignment = .center

        for v in [newShellRow, newSessionRow, settingsButton, plusButton, searchField, savedHeader, scrollView, emptyLabel] {
            root.addSubview(v)
        }
        model.onReload = { [weak self] in self?.rebuild(revealSelection: false) }
        rebuild()
    }

    /// 供侧栏切回「会话」页时主动刷新（含 MCP 访问级别等未发库变更通知的状态）。
    func reloadData() {
        model.reload()
    }

    @objc private func plusClicked() {
        actions.newSession()
    }

    /// 把键盘焦点交给搜索框。面板不是内容控制器，`viewDidAppear` 不会触发，由呈现方在面板成为 key 后调用。
    func focusSearchField() {
        view.window?.makeFirstResponder(searchField.textField)
    }

    /// 列表区以上的固定高度。
    private static var headHeight: CGFloat {
        topPadding + actionRowHeight * 2 + DesignTokens.Spacing.sm + searchHeight + DesignTokens.Spacing.md + savedHeaderHeight
    }

    private func layoutContents() {
        let w = view.bounds.width
        if embedded {
            let pad = DesignTokens.Spacing.sm
            let toolbarH: CGFloat = 24
            let plusW: CGFloat = 24
            var y: CGFloat = 0
            plusButton.frame = NSRect(x: w - pad - plusW, y: y, width: plusW, height: toolbarH)
            searchField.frame = NSRect(x: pad, y: y, width: max(0, plusButton.frame.minX - pad - DesignTokens.Spacing.xs), height: toolbarH)
            y += toolbarH + DesignTokens.Spacing.sm
            let shellH: CGFloat = 32
            settingsButton.frame = NSRect(x: w - pad - 24, y: y + (shellH - 24) / 2, width: 24, height: 24)
            newShellRow.frame = NSRect(x: 0, y: y, width: max(0, settingsButton.frame.minX - DesignTokens.Spacing.xs), height: shellH)
            y += shellH + DesignTokens.Spacing.xs
            let headerH: CGFloat = 28
            savedHeader.frame = NSRect(x: 0, y: y, width: w, height: headerH)
            y += headerH
            scrollView.frame = NSRect(x: 0, y: y, width: w, height: max(0, view.bounds.height - y))
            emptyLabel.frame = NSRect(x: pad, y: y + DesignTokens.Spacing.md, width: max(0, w - pad * 2), height: 40)
            tableView.tableColumns.first?.width = w
            return
        }
        let side = DesignTokens.Spacing.lg
        var y = Self.topPadding
        // 第一行右侧留给齿轮，高亮不盖住它。
        newShellRow.frame = NSRect(x: 0, y: y, width: w - 52, height: Self.actionRowHeight)
        settingsButton.frame = NSRect(x: w - side - 26, y: y + (Self.actionRowHeight - 26) / 2, width: 26, height: 26)
        y += Self.actionRowHeight
        newSessionRow.frame = NSRect(x: 0, y: y, width: w, height: Self.actionRowHeight)
        y += Self.actionRowHeight + DesignTokens.Spacing.sm
        searchField.frame = NSRect(x: side, y: y, width: w - side * 2, height: Self.searchHeight)
        y += Self.searchHeight + DesignTokens.Spacing.md
        savedHeader.frame = NSRect(x: 0, y: y, width: w, height: Self.savedHeaderHeight)
        y += Self.savedHeaderHeight
        scrollView.frame = NSRect(x: 0, y: y, width: w, height: max(0, view.bounds.height - y - Self.bottomPadding))
        emptyLabel.frame = NSRect(x: side, y: y + DesignTokens.Spacing.md, width: w - side * 2, height: 40)
        tableView.tableColumns.first?.width = w
    }

    // MARK: - 数据

    /// 按模型重建列表项并刷新表格，同时按内容重新计算面板高度。
    private func rebuild(revealSelection: Bool = true) {
        items = model.sections.flatMap { section -> [Item] in
            (section.group.map { [Item.header($0)] } ?? []) + section.profiles.map { Item.profile($0) }
        }
        tableView.reloadData()
        emptyLabel.isHidden = !items.isEmpty
        emptyLabel.stringValue = model.hasAnyProfile
            ? String(localized: "No matching sessions")
            : String(localized: "No saved sessions yet. Choose “New Session…” to add one.")
        updatePreferredSize()
        if revealSelection { scrollToSelection() }
    }

    /// 面板高度 = 固定头部 + 列表内容（有上限，超出滚动）+ 底部留白。嵌入侧栏时由父容器决定尺寸。
    private func updatePreferredSize() {
        guard !embedded else { return }
        let list = items.isEmpty ? Self.emptyListHeight : items.reduce(CGFloat(0)) { $0 + height(of: $1) }
        // 不能叫 height：同一函数里用到了 height(of:)，Swift 6.1（Xcode 16）会报循环引用。
        let total = min(Self.maxHeight, Self.headHeight + list + Self.bottomPadding)
        let size = NSSize(width: Self.width, height: total)
        guard size != preferredContentSize else { return }
        preferredContentSize = size
        view.setFrameSize(size)
        onPreferredSizeChange?(size)
    }

    private func height(of item: Item) -> CGFloat {
        if case .header = item { return Self.headerRowHeight }
        return Self.profileRowHeight
    }

    private func scrollToSelection() {
        if let row = items.firstIndex(where: { if case .profile(let p) = $0 { p.id == model.selectedID } else { false } }) {
            tableView.scrollRowToVisible(row)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { items.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat { height(of: items[row]) }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch items[row] {
        case .header(let group):
            let collapsed = model.isSearching ? nil : model.collapsedGroupIDs.contains(group.id)
            let view = LauncherHeaderView(title: group.name, collapsed: collapsed, compact: embedded)
            view.onToggle = { [weak self] in
                guard let self else { return }
                model.toggleGroup(group.id)
                rebuild(revealSelection: false)
            }
            view.onMenu = { [weak self] anchor in self?.showGroupMenu(group, from: anchor) }
            return view
        case .profile(let profile):
            let view = LauncherProfileRowView(profile: profile, compact: embedded)
            // 读失败时授权未知，不显示标记（`.off` 即不显示），免得把未知当成已关闭或已授权。
            view.mcpLevel = model.mcpReadError == nil ? model.mcpLevel(of: profile.id) : .off
            view.isSelected = profile.id == model.selectedID
            view.onClick = { [weak self] in self?.actions.connect(profile.id) }
            view.onMenu = { [weak self] anchor in self?.showProfileMenu(profile, from: anchor) }
            view.onContextMenu = { [weak self] anchor in self?.showProfileMenu(profile, from: anchor) }
            return view
        }
    }

    private func refreshSelection() {
        for row in 0..<items.count {
            guard case .profile(let p) = items[row],
                  let view = tableView.view(atColumn: 0, row: row, makeIfNecessary: false) as? LauncherProfileRowView
            else { continue }
            view.isSelected = p.id == model.selectedID
        }
        scrollToSelection()
    }

    // MARK: - 动作

    private func showSavedMenu(from sender: NSView) {
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "New Group…")) { [weak self] in self?.createGroup() })
        menu.addItem(ActionMenuItem(String(localized: "Import OpenSSH Config…")) { [weak self] in self?.actions.importSshConfig() })
        popUp(menu, from: sender)
    }

    /// 在锚点下方弹出菜单。菜单展开期间暂停面板的"失焦自动关闭"，否则菜单一出现面板就没了。
    private func popUp(_ menu: NSMenu, from anchor: NSView) {
        let show: () -> Void = { _ = menu.popUp(positioning: nil, at: NSPoint(x: 0, y: anchor.bounds.height + 2), in: anchor) }
        if let panel = anchor.window as? FloatingPanel { panel.withoutAutoDismiss(show) } else { show() }
    }

    private func showProfileMenu(_ profile: FfiProfile, from anchor: NSView) {
        popUp(makeProfileMenu(profile), from: anchor)
    }

    /// 会话行「…」菜单。单独成函数是为了测试能不弹出就检查菜单内容。
    func makeProfileMenu(_ profile: FfiProfile) -> NSMenu {
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "Edit")) { [weak self] in self?.actions.edit(profile) })
        let moveItem = NSMenuItem(title: String(localized: "Move to Group"), action: nil, keyEquivalent: "")
        let moveMenu = NSMenu()
        moveMenu.addItem(ActionMenuItem(String(localized: "No Group"), checked: profile.group == nil) { [weak self] in
            self?.move(profile, to: nil)
        })
        for group in model.groups {
            moveMenu.addItem(ActionMenuItem(group.name, checked: profile.group == group.id) { [weak self] in
                self?.move(profile, to: group.id)
            })
        }
        moveItem.submenu = moveMenu
        menu.addItem(moveItem)
        if model.supportsMcpAccess {
            let accessItem = NSMenuItem(title: String(localized: "MCP Access"), action: nil, keyEquivalent: "")
            let accessMenu = NSMenu()
            let current = model.mcpLevel(of: profile.id)
            // 关闭自动启用：否则 AppKit 验证菜单时会因 target/action 有效而把手动禁用的项重新启用。
            accessMenu.autoenablesItems = false
            addMcpReadErrorItem(to: accessMenu)
            for level in McpText.accessLevels {
                let item = ActionMenuItem(McpText.accessTitle(level), checked: model.mcpReadError == nil && level == current) { [weak self] in
                    self?.setMcpAccess([profile.id], to: level)
                }
                item.isEnabled = model.mcpReadError == nil
                accessMenu.addItem(item)
            }
            accessItem.submenu = accessMenu
            menu.addItem(accessItem)
        }
        menu.addItem(.separator())
        menu.addItem(ActionMenuItem(String(localized: "Delete…")) { [weak self] in self?.delete(profile) })
        return menu
    }

    private func showGroupMenu(_ group: FfiGroup, from anchor: NSView) {
        popUp(makeGroupMenu(group), from: anchor)
    }

    /// 分组标题行「…」菜单；同样单独成函数供测试使用。
    func makeGroupMenu(_ group: FfiGroup) -> NSMenu {
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "Rename…")) { [weak self] in self?.rename(group) })
        if model.supportsMcpAccess {
            // 没有「当前级别」可勾：组内会话级别可以各不相同，所以只提供「整组设为…」。
            let accessItem = NSMenuItem(title: String(localized: "MCP Access"), action: nil, keyEquivalent: "")
            let accessMenu = NSMenu()
            // 关闭自动启用：否则 AppKit 验证菜单时会因 target/action 有效而把手动禁用的项重新启用。
            accessMenu.autoenablesItems = false
            addMcpReadErrorItem(to: accessMenu)
            for level in McpText.accessLevels {
                let item = ActionMenuItem(String(localized: "Set Whole Group to \(McpText.accessTitle(level))")) { [weak self] in
                    guard let self else { return }
                    setMcpAccess(model.profileIDs(inGroup: group.id), to: level)
                }
                item.isEnabled = model.mcpReadError == nil
                accessMenu.addItem(item)
            }
            accessItem.submenu = accessMenu
            menu.addItem(accessItem)
        }
        // 破坏性的「删除…」放最后并用分隔线隔开，与会话菜单一致，避免误点。
        menu.addItem(.separator())
        menu.addItem(ActionMenuItem(String(localized: "Delete…")) { [weak self] in self?.delete(group) })
        return menu
    }

    /// 授权读取失败时，在「MCP 访问」子菜单顶部放一条禁用的说明项（后面的级别项也已禁用）；读取成功则什么也不加。
    private func addMcpReadErrorItem(to menu: NSMenu) {
        guard let reason = model.mcpReadError else { return }
        let item = NSMenuItem(title: String(localized: "Could not read the MCP access level: \(reason)"), action: nil, keyEquivalent: "")
        item.isEnabled = false
        menu.addItem(item)
        menu.addItem(.separator())
    }

    private func move(_ profile: FfiProfile, to groupID: String?) {
        do {
            try model.move(profileID: profile.id, toGroup: groupID)
            rebuild()
        } catch {
            showFailure(error)
        }
    }

    /// 设置一个或一组会话的 MCP 访问级别。从「关闭」调高时先确认（敏感内容提示，spec/mcp.md §2）；
    /// 确认框是 sheet，会让浮层失焦关闭，所以走 `withDialog`（先关浮层、做完再重开）。
    /// 不需要确认时直接写入并刷新列表，不关浮层；但写入失败要弹错误框，错误框同样是 sheet，
    /// 所以失败时也走 `withDialog`，与确认路径一致（否则浮层失焦关闭时错误框会连带消失或错位）。
    private func setMcpAccess(_ ids: [String], to level: FfiMcpAccess) {
        // 读授权失败时菜单项已禁用；这里再挡一次，防止状态未知时写入。
        guard !ids.isEmpty, model.mcpReadError == nil else { return }
        let model = model, dialogs = dialogs
        guard model.raisesMcpAccessFromOff(ids, to: level) else {
            let result = model.setMcpAccess(ids, to: level)
            // 部分成功时成功的会话已是新级别，列表必须刷新。
            rebuild()
            if result.firstError != nil {
                actions.withDialog { Self.reportMcpAccessFailure(result, dialogs: dialogs) }
            }
            return
        }
        actions.withDialog {
            guard await dialogs.confirm(
                title: McpText.raiseConfirmTitle(plural: ids.count > 1), message: McpText.sensitiveNotice,
                confirmTitle: String(localized: "Open"), destructive: false
            ) else { return }
            Self.applyMcpAccess(ids, to: level, model: model, dialogs: dialogs)
        }
    }

    /// 写入并在有失败时汇总报错；成功的会话保持新级别。
    private static func applyMcpAccess(_ ids: [String], to level: FfiMcpAccess, model: SessionLauncherModel, dialogs: SessionDialogs) {
        reportMcpAccessFailure(model.setMcpAccess(ids, to: level), dialogs: dialogs)
    }

    /// 有失败时汇总弹一次错误；没有失败什么也不做。
    private static func reportMcpAccessFailure(_ result: SessionLauncherModel.McpAccessChange, dialogs: SessionDialogs) {
        guard let error = result.firstError else { return }
        let reason = CoreMessageText.text(for: error)
        dialogs.showError(
            title: String(localized: "Could not change MCP access"),
            message: result.total > 1
                ? String(localized: "\(result.failed) of \(result.total) sessions could not be changed: \(reason)")
                : reason
        )
    }

    private func createGroup() {
        let model = model, dialogs = dialogs
        actions.withDialog {
            guard let name = await dialogs.promptText(
                title: String(localized: "New Group"), message: nil, initial: "", confirmTitle: String(localized: "Create")
            ) else { return }
            do { try model.createGroup(named: name) } catch { Self.showFailure(error, dialogs: dialogs) }
        }
    }

    private func rename(_ group: FfiGroup) {
        let model = model, dialogs = dialogs
        actions.withDialog {
            guard let name = await dialogs.promptText(
                title: String(localized: "Rename Group"), message: nil, initial: group.name, confirmTitle: String(localized: "Rename")
            ) else { return }
            do { try model.renameGroup(group, to: name) } catch { Self.showFailure(error, dialogs: dialogs) }
        }
    }

    private func delete(_ group: FfiGroup) {
        let model = model, dialogs = dialogs
        actions.withDialog {
            do {
                let message = try model.deleteGroupConfirmation(group)
                guard await dialogs.confirm(
                    title: String(localized: "Delete group “\(group.name)”?"), message: message,
                    confirmTitle: String(localized: "Delete"), destructive: true
                ) else { return }
                try await model.deleteGroup(group)
            } catch {
                Self.showFailure(error, dialogs: dialogs)
            }
        }
    }

    private func delete(_ profile: FfiProfile) {
        let model = model, dialogs = dialogs
        actions.withDialog {
            guard await dialogs.confirm(
                title: String(localized: "Delete session “\(profile.name)”?"),
                message: String(localized: "Only the session and credentials saved in Kanso are deleted; ~/.ssh/config and key files are not touched. This can’t be undone."),
                confirmTitle: String(localized: "Delete"), destructive: true
            ) else { return }
            do { try await model.deleteProfile(id: profile.id) } catch { Self.showFailure(error, dialogs: dialogs) }
        }
    }

    private func showFailure(_ error: Error) { Self.showFailure(error, dialogs: dialogs) }

    private static func showFailure(_ error: Error, dialogs: SessionDialogs) {
        dialogs.showError(title: String(localized: "The operation failed"), message: CoreMessageText.text(for: error))
    }

    // MARK: - 搜索框与键盘

    func controlTextDidChange(_ obj: Notification) {
        model.query = searchField.stringValue
        // 改搜索词会换一批结果。保持当前滚动位置，只有 ↑↓ 改选中时才滚到那一行。
        rebuild(revealSelection: false)
    }

    /// 上下键移动选中、回车连接、Esc 关闭。
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveDown(_:)):
            model.moveSelection(by: 1)
            refreshSelection()
        case #selector(NSResponder.moveUp(_:)):
            model.moveSelection(by: -1)
            refreshSelection()
        case #selector(NSResponder.insertNewline(_:)):
            if let id = model.selectedID { actions.connect(id) }
        case #selector(NSResponder.cancelOperation(_:)):
            if embedded, !searchField.stringValue.isEmpty {
                searchField.stringValue = ""
                model.query = ""
                rebuild(revealSelection: false)
            } else {
                actions.dismiss()
            }
        default:
            return false
        }
        return true
    }
}

/// 启动器根视图：flipped，布局交给控制器。
@MainActor
private final class LauncherRootView: NSView {
    var onLayout: (() -> Void)?
    override var isFlipped: Bool { true }

    override func layout() {
        super.layout()
        onLayout?()
    }
}

/// 高亮块只负责绘制，不参与命中测试，点击落到行上。
@MainActor
private final class HoverHighlightView: StyledBoxView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// 悬停时整行浅色高亮的行（无分隔线，visual-style §3.4）。高亮是行内缩进 6pt 的圆角块，
/// 不贴着面板边缘，和面板圆角协调；键盘选中与鼠标悬停用同一种高亮。
@MainActor
class LauncherHoverRow: NSView {
    private let highlight = HoverHighlightView()
    private(set) var isHovered = false
    let compact: Bool

    var isSelected = false {
        didSet { refreshHighlight() }
    }

    override var isFlipped: Bool { true }

    init(frame frameRect: NSRect = .zero, compact: Bool = false) {
        self.compact = compact
        super.init(frame: frameRect)
        highlight.cornerRadius = compact ? DesignTokens.Radius.sm : 8
        highlight.borderWidth = 0
        addSubview(highlight)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherHoverRow 只支持代码创建") }

    /// 高亮块之上的内容左右内边距（相对面板边缘）。
    static let contentInset: CGFloat = 18
    var horizontalInset: CGFloat { compact ? DesignTokens.Spacing.sm + 4 : Self.contentInset }

    override func layout() {
        super.layout()
        highlight.frame = bounds.insetBy(dx: compact ? DesignTokens.Spacing.sm : 6, dy: 1)
    }

    private func refreshHighlight() {
        highlight.fillToken = (isHovered || isSelected) ? DesignTokens.Color.bgRowHover : nil
    }

    func setHovered(_ hovered: Bool) {
        isHovered = hovered
        refreshHighlight()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(
            rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self, userInfo: nil
        ))
    }

    override func mouseEntered(with event: NSEvent) { setHovered(true) }
    override func mouseExited(with event: NSEvent) { setHovered(false) }
}

/// 列表式操作行：图标 + 文字（"本地 Shell"、"新建会话…"）。
@MainActor
final class LauncherActionRow: LauncherHoverRow {
    var onClick: (() -> Void)?
    private let iconView = NSImageView()
    private let label: PassthroughLabel

    init(title: String, symbol: String, compact: Bool = false) {
        label = PassthroughLabel(labelWithString: title)
        super.init(frame: .zero, compact: compact)
        iconView.image = .symbol(symbol)
        iconView.symbolConfiguration = .init(pointSize: compact ? 13 : 15, weight: .regular)
        iconView.contentTintColor = .token(DesignTokens.Color.textPrimary)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.textColor = .token(DesignTokens.Color.textPrimary)
        addSubview(iconView)
        addSubview(label)
        setAccessibilityRole(.button)
        setAccessibilityLabel(title)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherActionRow 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = horizontalInset
        let iconGap: CGFloat = compact ? 24 : 32
        iconView.frame = NSRect(x: inset, y: (bounds.height - 18) / 2, width: 20, height: 18)
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: inset + iconGap, y: (bounds.height - h) / 2, width: max(0, bounds.width - inset * 2 - iconGap), height: h)
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}

/// 小标题行（"已保存的会话"、分组名）：次要文字色 + 右侧"…"菜单按钮。两者样式相同（visual-style §3.4）。
@MainActor
final class LauncherHeaderView: NSView {
    var onMenu: ((NSView) -> Void)?
    var onToggle: (() -> Void)?
    private let disclosureButton: KButton?
    private let label: NSTextField
    private let menuButton = KButton(symbol: "ellipsis", style: .icon)
    private let compact: Bool

    override var isFlipped: Bool { true }

    init(title: String, collapsed: Bool? = nil, compact: Bool = false) {
        self.compact = compact
        disclosureButton = collapsed.map { KButton(symbol: $0 ? "chevron.right" : "chevron.down", style: .icon) }
        label = NSTextField(labelWithString: title)
        super.init(frame: .zero)
        label.font = .systemFont(ofSize: 12)
        label.textColor = .token(DesignTokens.Color.textSecondary)
        label.lineBreakMode = .byTruncatingTail
        menuButton.toolTip = String(localized: "More")
        menuButton.onPress = { [weak self] in
            guard let self else { return }
            onMenu?(menuButton)
        }
        if let disclosureButton, let collapsed {
            let action = collapsed ? String(localized: "Expand Group") : String(localized: "Collapse Group")
            disclosureButton.toolTip = action
            disclosureButton.setAccessibilityLabel("\(action) · \(title)")
            disclosureButton.onPress = { [weak self] in self?.onToggle?() }
            addSubview(disclosureButton)
        }
        addSubview(label)
        addSubview(menuButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherHeaderView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = compact ? DesignTokens.Spacing.sm : LauncherHoverRow.contentInset - 2
        menuButton.frame = NSRect(x: bounds.width - inset - 24, y: (bounds.height - 24) / 2, width: 24, height: 24)
        disclosureButton?.frame = NSRect(x: inset, y: (bounds.height - 24) / 2, width: 24, height: 24)
        let labelX = disclosureButton?.frame.maxX ?? inset
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: labelX, y: (bounds.height - h) / 2,
                             width: max(0, menuButton.frame.minX - labelX - DesignTokens.Spacing.xs), height: h)
    }
}

/// 行尾 MCP 标记：不拦截鼠标事件，点击标记等同点击整行（行的 `mouseDown` 负责连接）。
/// 不能直接用 `NSImageView`：它会吃掉 mouseDown，点在标记上就连不上会话。
/// hitTest 返回 nil 后视图自带的 toolTip 不再显示，提示由行视图用 `addToolTip` 给标记区域补上。
@MainActor
final class PassthroughImageView: NSImageView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// 会话行：小方块色点、名称、`user@host:port`。启动器右侧只留「…」；命令面板可关掉按钮。
/// 单击整行连接。右键与「…」打开同一份菜单。
@MainActor
final class LauncherProfileRowView: LauncherHoverRow {
    var onClick: (() -> Void)?
    var onMenu: ((NSView) -> Void)?
    var onContextMenu: ((NSView) -> Void)?

    private let swatch = ColorSquareView()
    private let titleLabel = PassthroughLabel(labelWithString: "")
    private let subtitleLabel = PassthroughLabel(labelWithString: "")
    private let menuButton = KButton(symbol: "ellipsis", style: .icon)
    private let mcpBadge = PassthroughImageView()
    /// 标记区域的悬停提示在行视图上的注册号；布局变化时要先移除再重新登记。
    private var mcpBadgeToolTipTag: NSView.ToolTipTag?
    private var mcpBadgeToolTipText: String?
    private let showsAccessoryButtons: Bool

    /// 会话对 AI 工具开放的级别；不是「关闭」时行尾显示 MCP 标记，悬停显示级别。
    var mcpLevel: FfiMcpAccess = .off {
        didSet {
            mcpBadge.isHidden = mcpLevel == .off
            mcpBadgeToolTipText = mcpLevel == .off ? nil : McpText.badgeTooltip(mcpLevel)
            needsLayout = true
        }
    }

    /// MCP 标记是否显示，供测试读取。
    var isMcpBadgeVisible: Bool { !mcpBadge.isHidden }
    var mcpBadgeToolTip: String? { mcpBadgeToolTipText }
    /// 标记视图本身，供测试做 hitTest。
    var mcpBadgeViewForTesting: NSView { mcpBadge }

    init(title: String, subtitle: String, color: FfiRgb?, showsAccessoryButtons: Bool = true, compact: Bool = false) {
        self.showsAccessoryButtons = showsAccessoryButtons
        super.init(frame: .zero, compact: compact)
        mcpBadge.image = .symbol("point.3.connected.trianglepath.dotted", description: String(localized: "MCP access"))
        mcpBadge.imageScaling = .scaleProportionallyDown
        mcpBadge.contentTintColor = .token(DesignTokens.Color.textTertiary)
        mcpBadge.isHidden = true
        swatch.rgb = color
        titleLabel.stringValue = title
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        titleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.stringValue = subtitle
        subtitleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        subtitleLabel.textColor = .token(DesignTokens.Color.textSecondary)
        subtitleLabel.lineBreakMode = .byTruncatingMiddle
        var views: [NSView] = [swatch, titleLabel, subtitleLabel, mcpBadge]
        if showsAccessoryButtons {
            menuButton.toolTip = String(localized: "More")
            menuButton.onPress = { [weak self] in
                guard let self else { return }
                onMenu?(menuButton)
            }
            views.append(menuButton)
        }
        for v in views { addSubview(v) }
        setAccessibilityRole(.button)
        setAccessibilityLabel(title)
    }

    convenience init(profile: FfiProfile, compact: Bool = false) {
        self.init(
            title: profile.name,
            subtitle: SessionLauncherModel.subtitle(for: profile),
            color: profile.color,
            showsAccessoryButtons: true,
            compact: compact
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherProfileRowView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = compact ? DesignTokens.Spacing.sm + 4 : Self.contentInset - 2
        let h = bounds.height
        var textTrailing: CGFloat
        if showsAccessoryButtons {
            menuButton.frame = NSRect(x: bounds.width - inset - 24, y: (h - 24) / 2, width: 24, height: 24)
            textTrailing = menuButton.frame.minX - DesignTokens.Spacing.xs
        } else {
            textTrailing = bounds.width - inset
        }
        if !mcpBadge.isHidden {
            // 标记贴在文字列右端、编辑按钮之前；文字让出它的位置。
            mcpBadge.frame = NSRect(x: textTrailing - 14, y: (h - 14) / 2, width: 14, height: 14)
            textTrailing = mcpBadge.frame.minX - DesignTokens.Spacing.xs
        }
        if let mcpBadgeToolTipTag { removeToolTip(mcpBadgeToolTipTag) }
        mcpBadgeToolTipTag = mcpBadge.isHidden ? nil : addToolTip(mcpBadge.frame, owner: self, userData: nil)
        let textX = inset + 22
        let textW = max(0, textTrailing - textX)
        let titleH = ceil(titleLabel.intrinsicContentSize.height)
        let subtitleH = ceil(subtitleLabel.intrinsicContentSize.height)
        let top = (h - titleH - subtitleH - 2) / 2
        titleLabel.frame = NSRect(x: textX, y: top, width: textW, height: titleH)
        subtitleLabel.frame = NSRect(x: textX, y: top + titleH + 2, width: textW, height: subtitleH)
        swatch.frame = NSRect(x: inset, y: titleLabel.frame.midY - 5, width: 10, height: 10)
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func rightMouseDown(with event: NSEvent) {
        if let onContextMenu {
            onContextMenu(self)
        } else {
            super.rightMouseDown(with: event)
        }
    }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}

extension LauncherProfileRowView: NSViewToolTipOwner {
    nonisolated func view(_ view: NSView, stringForToolTip tag: NSView.ToolTipTag, point: NSPoint, userData data: UnsafeMutableRawPointer?) -> String {
        MainActor.assumeIsolated { mcpBadgeToolTipText ?? "" }
    }
}

/// 会话颜色小方块（圆角 3）；没有颜色时不显示。颜色是固定 RGB，不随外观变化，所以只在设置时写一次图层。
@MainActor
final class ColorSquareView: NSView {
    var rgb: FfiRgb? {
        didSet { refresh() }
    }

    override var isFlipped: Bool { true }

    /// 只显示颜色。点击交给所在的行，否则色块会吃掉「单击连接」。
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = 3
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("ColorSquareView 只支持代码创建") }

    private func refresh() {
        isHidden = rgb == nil
        layer?.backgroundColor = rgb.map {
            NSColor(srgbRed: CGFloat($0.r) / 255, green: CGFloat($0.g) / 255, blue: CGFloat($0.b) / 255, alpha: 1).cgColor
        }
    }
}
