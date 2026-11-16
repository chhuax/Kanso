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

    private let newShellRow = LauncherActionRow(title: String(localized: "Local Shell"), symbol: "terminal")
    private let newSessionRow = LauncherActionRow(title: String(localized: "New Session…"), symbol: "plus")
    private let settingsButton = KButton(symbol: "gearshape", style: .icon)
    let searchField = SidebarFilterField()
    private let savedHeader = LauncherHeaderView(title: String(localized: "Saved Sessions"))
    private let tableView = NSTableView()
    private let scrollView = NSScrollView()
    private let emptyLabel = NSTextField(wrappingLabelWithString: "")
    private var items: [Item] = []

    init(model: SessionLauncherModel, dialogs: SessionDialogs, actions: SessionLauncherActions) {
        self.model = model
        self.dialogs = dialogs
        self.actions = actions
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
        settingsButton.toolTip = String(localized: "Settings…")
        settingsButton.onPress = { [weak self] in self?.actions.openSettings() }

        searchField.placeholder = String(localized: "Search saved sessions")
        searchField.textField.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        searchField.textField.delegate = self
        searchField.onClear = { [weak self] in
            self?.model.query = ""
            self?.rebuild()
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

        for v in [newShellRow, newSessionRow, settingsButton, searchField, savedHeader, scrollView, emptyLabel] {
            root.addSubview(v)
        }
        rebuild()
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
    private func rebuild() {
        items = model.sections.flatMap { section -> [Item] in
            (section.group.map { [Item.header($0)] } ?? []) + section.profiles.map { Item.profile($0) }
        }
        tableView.reloadData()
        emptyLabel.isHidden = !items.isEmpty
        emptyLabel.stringValue = model.hasAnyProfile
            ? String(localized: "No matching sessions")
            : String(localized: "No saved sessions yet. Choose “New Session…” to add one.")
        updatePreferredSize()
        scrollToSelection()
    }

    /// 面板高度 = 固定头部 + 列表内容（有上限，超出滚动）+ 底部留白。
    private func updatePreferredSize() {
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
            let view = LauncherHeaderView(title: group.name)
            view.onMenu = { [weak self] anchor in self?.showGroupMenu(group, from: anchor) }
            return view
        case .profile(let profile):
            let view = LauncherProfileRowView(profile: profile)
            view.isSelected = profile.id == model.selectedID
            view.onClick = { [weak self] in self?.actions.connect(profile.id) }
            view.onEdit = { [weak self] in self?.actions.edit(profile) }
            view.onMenu = { [weak self] anchor in self?.showProfileMenu(profile, from: anchor) }
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
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "Connect")) { [weak self] in self?.actions.connect(profile.id) })
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
        menu.addItem(.separator())
        menu.addItem(ActionMenuItem(String(localized: "Delete…")) { [weak self] in self?.delete(profile) })
        popUp(menu, from: anchor)
    }

    private func showGroupMenu(_ group: FfiGroup, from anchor: NSView) {
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "Rename…")) { [weak self] in self?.rename(group) })
        menu.addItem(ActionMenuItem(String(localized: "Delete…")) { [weak self] in self?.delete(group) })
        popUp(menu, from: anchor)
    }

    private func move(_ profile: FfiProfile, to groupID: String?) {
        do {
            try model.move(profileID: profile.id, toGroup: groupID)
            rebuild()
        } catch {
            showFailure(error)
        }
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
        rebuild()
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
            actions.dismiss()
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

/// 悬停时整行浅色高亮的行（无分隔线，visual-style §3.4）。高亮是行内缩进 6pt 的圆角块，
/// 不贴着面板边缘，和面板圆角协调；键盘选中与鼠标悬停用同一种高亮。
@MainActor
class LauncherHoverRow: NSView {
    private let highlight = StyledBoxView()
    private(set) var isHovered = false

    var isSelected = false {
        didSet { refreshHighlight() }
    }

    override var isFlipped: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        highlight.cornerRadius = 8
        highlight.borderWidth = 0
        addSubview(highlight)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherHoverRow 只支持代码创建") }

    /// 高亮块之上的内容左右内边距（相对面板边缘）。
    static let contentInset: CGFloat = 18

    override func layout() {
        super.layout()
        highlight.frame = bounds.insetBy(dx: 6, dy: 1)
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

    init(title: String, symbol: String) {
        label = PassthroughLabel(labelWithString: title)
        super.init(frame: .zero)
        iconView.image = .symbol(symbol)
        iconView.symbolConfiguration = .init(pointSize: 15, weight: .regular)
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
        let inset = Self.contentInset
        iconView.frame = NSRect(x: inset, y: (bounds.height - 18) / 2, width: 20, height: 18)
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: inset + 32, y: (bounds.height - h) / 2, width: max(0, bounds.width - inset * 2 - 32), height: h)
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
    private let label: NSTextField
    private let menuButton = KButton(symbol: "ellipsis", style: .icon)

    override var isFlipped: Bool { true }

    init(title: String) {
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
        addSubview(label)
        addSubview(menuButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherHeaderView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = LauncherHoverRow.contentInset - 2
        menuButton.frame = NSRect(x: bounds.width - inset - 24, y: (bounds.height - 24) / 2, width: 24, height: 24)
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: inset, y: (bounds.height - h) / 2,
                             width: max(0, menuButton.frame.minX - inset - DesignTokens.Spacing.xs), height: h)
    }
}

/// 会话行：小方块色点、名称、`user@host:port`；启动器里右侧铅笔与"…"一直显示，命令面板可关掉按钮。
/// 单击整行 = 连接 / 打开。
@MainActor
final class LauncherProfileRowView: LauncherHoverRow {
    var onClick: (() -> Void)?
    var onEdit: (() -> Void)?
    var onMenu: ((NSView) -> Void)?

    private let swatch = ColorSquareView()
    private let titleLabel = PassthroughLabel(labelWithString: "")
    private let subtitleLabel = PassthroughLabel(labelWithString: "")
    private let editButton = KButton(symbol: "pencil", style: .icon)
    private let menuButton = KButton(symbol: "ellipsis", style: .icon)
    private let showsAccessoryButtons: Bool

    init(title: String, subtitle: String, color: FfiRgb?, showsAccessoryButtons: Bool = true) {
        self.showsAccessoryButtons = showsAccessoryButtons
        super.init(frame: .zero)
        swatch.rgb = color
        titleLabel.stringValue = title
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        titleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.stringValue = subtitle
        subtitleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        subtitleLabel.textColor = .token(DesignTokens.Color.textSecondary)
        subtitleLabel.lineBreakMode = .byTruncatingMiddle
        var views: [NSView] = [swatch, titleLabel, subtitleLabel]
        if showsAccessoryButtons {
            editButton.toolTip = String(localized: "Edit")
            editButton.onPress = { [weak self] in self?.onEdit?() }
            menuButton.toolTip = String(localized: "More")
            menuButton.onPress = { [weak self] in
                guard let self else { return }
                onMenu?(menuButton)
            }
            views.append(contentsOf: [editButton, menuButton])
        }
        for v in views { addSubview(v) }
        setAccessibilityRole(.button)
        setAccessibilityLabel(title)
    }

    convenience init(profile: FfiProfile) {
        self.init(
            title: profile.name,
            subtitle: SessionLauncherModel.subtitle(for: profile),
            color: profile.color,
            showsAccessoryButtons: true
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LauncherProfileRowView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = Self.contentInset - 2
        let h = bounds.height
        let textTrailing: CGFloat
        if showsAccessoryButtons {
            menuButton.frame = NSRect(x: bounds.width - inset - 24, y: (h - 24) / 2, width: 24, height: 24)
            editButton.frame = NSRect(x: menuButton.frame.minX - 26, y: (h - 24) / 2, width: 24, height: 24)
            textTrailing = editButton.frame.minX - DesignTokens.Spacing.xs
        } else {
            textTrailing = bounds.width - inset
        }
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

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}

/// 会话颜色小方块（圆角 3）；没有颜色时不显示。颜色是固定 RGB，不随外观变化，所以只在设置时写一次图层。
@MainActor
final class ColorSquareView: NSView {
    var rgb: FfiRgb? {
        didSet { refresh() }
    }

    override var isFlipped: Bool { true }

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
