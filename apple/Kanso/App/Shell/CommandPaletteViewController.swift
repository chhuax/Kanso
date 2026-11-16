import AppKit
import KansoUI

/// 命令面板浮层内容：搜索框 + 分组结果列表（视觉对齐会话启动器：§3.1 输入框、行悬停高亮）。
@MainActor
final class CommandPaletteViewController: NSViewController, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let width: CGFloat = 520
    static let maxHeight: CGFloat = 640
    static let hitRowHeight: CGFloat = 36
    static let sessionRowHeight: CGFloat = SessionLauncherViewController.profileRowHeight
    /// 分组标题行：上方留白与组间一致（含第一组，避免标题紧贴搜索框）。
    static let headerRowHeight: CGFloat = 28 + DesignTokens.Spacing.md
    private static let headerTopSpacing: CGFloat = DesignTokens.Spacing.md

    private static let topPadding: CGFloat = 10
    private static let searchHeight: CGFloat = DesignTokens.Spacing.fieldHeight
    private static let bottomPadding: CGFloat = 8
    private static let emptyListHeight: CGFloat = 56

    var onPreferredSizeChange: ((NSSize) -> Void)?

    let model: CommandPaletteModel
    let searchField = SidebarFilterField()
    private let tableView = NSTableView()
    private let scrollView = NSScrollView()
    private let emptyLabel = NSTextField(wrappingLabelWithString: "")

    init(model: CommandPaletteModel) {
        self.model = model
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("CommandPaletteViewController 只支持代码创建") }

    override func loadView() {
        let root = PaletteRootView(frame: NSRect(origin: .zero, size: NSSize(width: Self.width, height: Self.maxHeight)))
        root.onLayout = { [weak self] in self?.layoutContents() }
        view = root

        searchField.placeholder = String(localized: "Search tabs, sessions, commands, files")
        searchField.textField.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        searchField.textField.delegate = self
        searchField.onClear = { [weak self] in
            self?.model.query = ""
            self?.reload()
        }

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main"))
        tableView.addTableColumn(column)
        tableView.headerView = nil
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

        emptyLabel.stringValue = String(localized: "No matching results")
        emptyLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        emptyLabel.textColor = .token(DesignTokens.Color.textTertiary)
        emptyLabel.alignment = .center

        for v in [searchField, scrollView, emptyLabel] { root.addSubview(v) }
        reload()
    }

    func focusSearchField() {
        view.window?.makeFirstResponder(searchField.textField)
    }

    private func layoutContents() {
        let w = view.bounds.width
        let side = DesignTokens.Spacing.lg
        var y = Self.topPadding
        searchField.frame = NSRect(x: side, y: y, width: w - side * 2, height: Self.searchHeight)
        y += Self.searchHeight + DesignTokens.Spacing.md
        scrollView.frame = NSRect(x: 0, y: y, width: w, height: max(0, view.bounds.height - y - Self.bottomPadding))
        emptyLabel.frame = NSRect(x: side, y: y + DesignTokens.Spacing.md, width: w - side * 2, height: 40)
        tableView.tableColumns.first?.width = w
    }

    func reload() {
        tableView.reloadData()
        let hasHits = !model.rows.isEmpty
        emptyLabel.isHidden = hasHits
        updatePreferredSize()
        refreshSelection()
    }

    private func updatePreferredSize() {
        let head = Self.topPadding + Self.searchHeight + DesignTokens.Spacing.md
        var list: CGFloat = 0
        measuredRowCount = 0
        if model.rows.isEmpty {
            list = Self.emptyListHeight
        } else {
            for row in model.rows {
                list += height(of: row)
                measuredRowCount += 1
                if head + list + Self.bottomPadding >= Self.maxHeight { break }
            }
        }
        let height = min(Self.maxHeight, head + list + Self.bottomPadding)
        let size = NSSize(width: Self.width, height: height)
        guard size != preferredContentSize else { return }
        preferredContentSize = size
        view.setFrameSize(size)
        onPreferredSizeChange?(size)
    }

    private(set) var measuredRowCount = 0

    private func height(of row: CommandPaletteModel.Row) -> CGFloat {
        switch row {
        case .header:
            return Self.headerRowHeight
        case .hit(let hit):
            if case .session = hit.kind { return Self.sessionRowHeight }
            return Self.hitRowHeight
        }
    }

    private func refreshSelection() {
        for row in 0..<model.rows.count {
            guard case .hit(let hit) = model.rows[row] else { continue }
            let selected = row == model.selectedIndex
            if case .session = hit.kind {
                (tableView.view(atColumn: 0, row: row, makeIfNecessary: false) as? LauncherProfileRowView)?.isSelected = selected
            } else {
                (tableView.view(atColumn: 0, row: row, makeIfNecessary: false) as? PaletteHitRowView)?.isSelected = selected
            }
        }
        if let selected = model.selectedIndex {
            tableView.scrollRowToVisible(selected)
        }
    }

    // MARK: - Table

    func numberOfRows(in tableView: NSTableView) -> Int { model.rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        height(of: model.rows[row])
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch model.rows[row] {
        case .header(let section):
            return PaletteSectionHeaderView(title: section.title, topSpacing: Self.headerTopSpacing)
        case .hit(let hit):
            if case .session = hit.kind {
                let view = LauncherProfileRowView(
                    title: hit.title,
                    subtitle: hit.detail ?? "",
                    color: hit.color,
                    showsAccessoryButtons: false
                )
                view.isSelected = row == model.selectedIndex
                view.onClick = { [weak self] in
                    self?.model.selectHit(at: row)
                    self?.model.execute(hit)
                }
                return view
            }
            let view = PaletteHitRowView(hit: hit)
            view.isSelected = row == model.selectedIndex
            view.onClick = { [weak self] in
                self?.model.selectHit(at: row)
                self?.model.execute(hit)
            }
            return view
        }
    }

    // MARK: - Search / keys

    func controlTextDidChange(_ obj: Notification) {
        model.query = searchField.stringValue
        reload()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.moveUp(_:)):
            model.moveSelection(by: -1)
            refreshSelection()
            return true
        case #selector(NSResponder.moveDown(_:)):
            model.moveSelection(by: 1)
            refreshSelection()
            return true
        case #selector(NSResponder.insertNewline(_:)):
            return model.executeSelection()
        case #selector(NSResponder.cancelOperation(_:)):
            model.actions?.dismiss()
            return true
        default:
            return false
        }
    }
}

// MARK: - 子视图

@MainActor
private final class PaletteRootView: NSView {
    var onLayout: (() -> Void)?
    override var isFlipped: Bool { true }
    override func layout() {
        super.layout()
        onLayout?()
    }
}

@MainActor
private final class PaletteSectionHeaderView: NSView {
    private let label: NSTextField
    private let topSpacing: CGFloat
    override var isFlipped: Bool { true }

    init(title: String, topSpacing: CGFloat) {
        self.topSpacing = topSpacing
        label = NSTextField(labelWithString: title)
        super.init(frame: .zero)
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .token(DesignTokens.Color.textSecondary)
        label.lineBreakMode = .byTruncatingTail
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("PaletteSectionHeaderView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = LauncherHoverRow.contentInset - 2
        let h = ceil(label.intrinsicContentSize.height)
        let contentH = max(0, bounds.height - topSpacing)
        label.frame = NSRect(
            x: inset, y: topSpacing + (contentH - h) / 2,
            width: max(0, bounds.width - inset * 2), height: h
        )
    }
}

@MainActor
private final class PaletteHitRowView: LauncherHoverRow {
    var onClick: (() -> Void)?
    private let titleLabel = PassthroughLabel(labelWithString: "")
    private let detailLabel = PassthroughLabel(labelWithString: "")

    init(hit: CommandPaletteHit) {
        super.init(frame: .zero)
        titleLabel.stringValue = hit.title
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        titleLabel.lineBreakMode = .byTruncatingTail
        detailLabel.stringValue = hit.detail ?? ""
        detailLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        detailLabel.textColor = .token(DesignTokens.Color.textTertiary)
        // 左对齐 + 右侧定位：避免右对齐在偏窄 frame 里把尾部画出边界。
        detailLabel.alignment = .left
        detailLabel.lineBreakMode = .byTruncatingMiddle
        detailLabel.cell?.truncatesLastVisibleLine = true
        detailLabel.isHidden = hit.detail == nil || hit.detail?.isEmpty == true
        addSubview(titleLabel)
        addSubview(detailLabel)
        setAccessibilityRole(.button)
        setAccessibilityLabel(hit.title)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("PaletteHitRowView 只支持代码创建") }

    override func layout() {
        super.layout()
        let inset = Self.contentInset
        let h = bounds.height
        let titleH = ceil(titleLabel.intrinsicContentSize.height)
        if detailLabel.isHidden {
            titleLabel.frame = NSRect(x: inset, y: (h - titleH) / 2, width: max(0, bounds.width - inset * 2), height: titleH)
            return
        }
        // sizeThatFits 比 intrinsic / size(withAttributes:) 更稳；再加一点余量抵消 cell 内边距。
        let fitting = detailLabel.sizeThatFits(NSSize(width: CGFloat.greatestFiniteMagnitude, height: h))
        let rawDetailW = ceil(fitting.width) + 6
        let maxDetailW = max(0, bounds.width - inset * 2 - 96)
        let detailW = min(rawDetailW, maxDetailW)
        detailLabel.lineBreakMode = rawDetailW > maxDetailW ? .byTruncatingMiddle : .byClipping
        let detailH = max(ceil(fitting.height), ceil(detailLabel.intrinsicContentSize.height))
        detailLabel.frame = NSRect(
            x: bounds.width - inset - detailW, y: (h - detailH) / 2,
            width: detailW, height: detailH
        )
        let titleW = max(0, detailLabel.frame.minX - DesignTokens.Spacing.sm - inset)
        titleLabel.frame = NSRect(x: inset, y: (h - titleH) / 2, width: titleW, height: titleH)
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}
