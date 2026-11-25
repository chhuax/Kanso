import AppKit
import KansoUI

/// 标签行的选中强度（§2.2）："选中"= 所在 pane 的活动标签。
enum TabRowSelection: Equatable {
    case none
    /// 非活动 pane 的活动标签：弱一些。
    case paneActive
    /// 活动 pane 的活动标签：最强调。
    case windowActive
}

/// 会话颜色：配置了 `colorHex` 用它，否则按稳定哈希取调色板。
/// 调色板用系统动态色，随外观自动变化（没有可用的品牌 / 自定义调色板素材）。
enum SessionColor {
    static let palette: [NSColor] = [
        .systemRed, .systemOrange, .systemYellow, .systemGreen, .systemTeal, .systemBlue, .systemPurple, .systemPink,
    ]

    static func color(for item: TabItem) -> NSColor {
        if let hex = item.colorHex, let color = parse(hex: hex) { return color }
        return palette[TabPresentation.colorIndex(for: item, paletteCount: palette.count)]
    }

    /// 解析 `#RRGGBB`；格式不对返回 nil（回退到调色板）。
    static func parse(hex: String) -> NSColor? {
        var s = Substring(hex)
        if s.hasPrefix("#") { s = s.dropFirst() }
        guard s.count == 6, let value = UInt32(s, radix: 16) else { return nil }
        return NSColor(
            srgbRed: CGFloat((value >> 16) & 0xFF) / 255,
            green: CGFloat((value >> 8) & 0xFF) / 255,
            blue: CGFloat(value & 0xFF) / 255,
            alpha: 1
        )
    }
}

/// 侧栏列表（flipped 文档视图）：分组标题 + 标签行，以及拖拽排序（§2.4）。
///
/// 拖拽状态全部在这里，并以 internal 方法暴露（`dragBegan` / `dragMoved` / `dragEnded` / `cancelDrag`），
/// 行视图的鼠标事件只是转发，测试可以直接调用。
@MainActor
final class SidebarListView: NSView, TabRowHost {
    static let rowSpacing: CGFloat = DesignTokens.Spacing.tabRowGap
    static let headerHeight: CGFloat = 24
    static let horizontalInset: CGFloat = 8
    static let verticalInset: CGFloat = DesignTokens.Spacing.tabRowGap / 2

    /// 一次完整重建所需的数据。
    struct Section {
        var pane: PaneID
        var titleIndex: Int
        var showsHeader: Bool
        var isActivePane: Bool
        var rows: [TabRowView.Model]
    }

    var onActivate: ((SessionID) -> Void)?
    var onActivatePane: ((PaneID) -> Void)?
    var onClose: (([SessionID]) -> Void)?
    var onRename: ((SessionID) -> Void)?
    /// 拖拽正常结束：`(会话, 同组内插入位置)`。
    var onMove: ((SessionID, Int) -> Void)?
    var onBlankDoubleClick: (() -> Void)?
    /// 空白处（含分屏标题）右键：参数为点击位置（本视图坐标）。行自己的右键走 `rowMenuProvider`，不会到这里。
    var onBlankRightClick: ((NSPoint) -> Void)?
    var rowMenuProvider: ((SessionID) -> NSMenu?)?
    /// 拖拽结束（提交或取消）后调用，侧栏据此执行被推迟的重建。
    var onDragFinished: (() -> Void)?
    /// 筛选中禁用拖拽：行只是子集，插入位置无法对应到完整列表。
    var allowsReorder = true

    private(set) var rowViews: [TabRowView] = []
    private var headerViews: [SidebarHeaderView] = []
    private var sections: [Section] = []
    /// 每个组内（含该组全部行）的 home 位置，布局时算出。
    private var homeY: [SessionID: CGFloat] = [:]
    /// 标签行之间的 1px 横向分界线纵坐标，布局时算出。
    private var dividerYs: [CGFloat] = []
    private let insertionLine = NSView()

    // 拖拽状态
    private var drag: TabReorderDrag?
    private var dragSession: SessionID?
    private var dragStartY: CGFloat = 0
    private var dragPointerY: CGFloat = 0
    private var dragGroup: [SessionID] = []
    private var resignObserver: NSObjectProtocol?

    override var isFlipped: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        insertionLine.wantsLayer = true
        insertionLine.isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SidebarListView 只支持代码创建") }

    // MARK: - 数据

    /// 结构签名：组与行的 id 序列。相同则只就地更新行内容，不重建视图（保住悬停与拖拽状态）。
    private static func signature(_ sections: [Section]) -> [String] {
        sections.map { s in "\(s.pane.raw)|\(s.showsHeader)|" + s.rows.map { String($0.item.id) }.joined(separator: ",") }
    }

    /// 是否正在跟踪拖拽（含尚未激活的按下状态）。
    var isDragTracking: Bool { drag != nil }
    var isDragActive: Bool { drag?.isActive ?? false }

    /// 更新内容。返回 false 表示结构变化但正在拖拽，调用方要在 `onDragFinished` 时重试。
    @discardableResult
    func update(_ newSections: [Section]) -> Bool {
        if Self.signature(newSections) == Self.signature(sections) {
            sections = newSections
            let models = newSections.flatMap(\.rows)
            for (row, model) in zip(rowViews, models) { row.model = model }
            for (header, section) in zip(headerViews, newSections.filter(\.showsHeader)) {
                header.configure(titleIndex: section.titleIndex, isActive: section.isActivePane)
            }
            return true
        }
        // 重建会丢掉拖拽状态，推迟到拖拽结束。
        if isDragTracking { return false }
        rebuild(newSections)
        return true
    }

    private func rebuild(_ newSections: [Section]) {
        var existingRows = Dictionary(uniqueKeysWithValues: rowViews.map { ($0.model.item.id, $0) })
        var existingHeaders = Dictionary(uniqueKeysWithValues: headerViews.map { ($0.pane, $0) })
        sections = newSections
        rowViews = []
        headerViews = []
        for section in newSections {
            if section.showsHeader {
                let header = existingHeaders.removeValue(forKey: section.pane) ?? SidebarHeaderView()
                header.pane = section.pane
                header.configure(titleIndex: section.titleIndex, isActive: section.isActivePane)
                header.onClick = { [weak self] pane in self?.onActivatePane?(pane) }
                if header.superview == nil { addSubview(header) }
                headerViews.append(header)
            }
            for model in section.rows {
                let row = existingRows.removeValue(forKey: model.item.id) ?? TabRowView(model: model)
                row.model = model
                row.host = self
                if row.superview == nil { addSubview(row) }
                rowViews.append(row)
            }
        }
        for row in existingRows.values { row.removeFromSuperview() }
        for header in existingHeaders.values { header.removeFromSuperview() }
        addSubview(insertionLine) // 始终在最上层
        needsLayout = true
        layoutSubtreeIfNeeded()
        refreshHover()
    }

    /// 重建后补算悬停：鼠标静止不动时不会再有 mouseEntered，行会一直处于"未悬停"。
    private func refreshHover() {
        guard let window else { return }
        let p = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        for row in rowViews { row.isHovered = row.frame.contains(p) && bounds.contains(p) }
    }

    func rowView(for session: SessionID) -> TabRowView? { rowViews.first { $0.model.item.id == session } }

    /// 把某个标签滚到可见区域（活动标签变化时调用）。
    func scrollToVisible(session: SessionID) {
        if let row = rowView(for: session) { scrollToVisible(row.frame.insetBy(dx: 0, dy: -Self.rowSpacing)) }
    }

    // MARK: - 布局

    /// 内容总高度。
    private(set) var contentHeight: CGFloat = 0

    override func layout() {
        super.layout()
        layoutRows()
    }

    private func layoutRows() {
        let width = bounds.width - Self.horizontalInset * 2
        var y = Self.verticalInset
        var rowIter = rowViews.makeIterator()
        var headerIter = headerViews.makeIterator()
        homeY = [:]
        dividerYs = rowViews.isEmpty ? [] : [0]
        for section in sections {
            if section.showsHeader, let header = headerIter.next() {
                header.frame = NSRect(x: Self.horizontalInset, y: y, width: width, height: Self.headerHeight)
                y += Self.headerHeight + Self.verticalInset
            }
            for _ in section.rows {
                guard let row = rowIter.next() else { break }
                row.frame = NSRect(x: Self.horizontalInset, y: y, width: width, height: DesignTokens.Spacing.tabRowHeight)
                homeY[row.model.item.id] = y
                dividerYs.append(y + DesignTokens.Spacing.tabRowHeight + Self.rowSpacing / 2)
                y += DesignTokens.Spacing.tabRowHeight + Self.rowSpacing
            }
        }
        contentHeight = y
        needsDisplay = true
        applyDragOffsets()
    }

    override func draw(_ dirtyRect: NSRect) {
        guard !dividerYs.isEmpty else { return }
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        for y in dividerYs {
            let line = NSRect(x: 0, y: y, width: bounds.width, height: 1)
            if line.intersects(dirtyRect) { line.fill() }
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: - 拖拽排序

    /// 按下：先丢弃残留的旧拖拽状态，再新建。
    /// 旧状态可能因为丢失 mouseUp（窗口外松开、被系统打断）残留，不清掉会让后续点击都被当成拖拽的延续。
    func dragBegan(session: SessionID, pointerY: CGFloat) {
        discardDragState()
        guard allowsReorder, let section = sections.first(where: { $0.rows.contains { $0.item.id == session } }) else { return }
        let group = section.rows.map(\.item.id)
        let rows = group.compactMap { id -> (id: SessionID, minY: CGFloat, maxY: CGFloat)? in
            guard let y = homeY[id] else { return nil }
            return (id, y, y + DesignTokens.Spacing.tabRowHeight)
        }
        drag = TabReorderDrag(dragged: session, startY: pointerY, rows: rows)
        dragSession = session
        dragStartY = pointerY
        dragPointerY = pointerY
        dragGroup = group
    }

    func dragMoved(pointerY: CGFloat) {
        guard drag != nil else { return }
        dragPointerY = pointerY
        drag?.update(pointerY: pointerY)
        if drag?.isActive == true {
            applyDragOffsets()
        }
    }

    /// 正常的左键松开：激活过才提交排序；只是点击则什么都不改。
    func dragEnded() {
        guard let drag, let session = dragSession else { return }
        let index = drag.isActive ? drag.insertionIndex : nil
        finishDragVisuals()
        if let index { onMove?(session, index) }
        onDragFinished?()
    }

    /// 取消：恢复界面，不改顺序。触控板滚动、窗口失去 key、视图离开窗口等打断时调用。
    func cancelDrag() {
        guard drag != nil else { return }
        finishDragVisuals()
        onDragFinished?()
    }

    /// 只丢弃状态与视觉，不触发 `onDragFinished`（新的 mouseDown 内部使用）。
    private func discardDragState() {
        guard drag != nil else { return }
        finishDragVisuals()
    }

    private func finishDragVisuals() {
        drag = nil
        dragSession = nil
        dragGroup = []
        insertionLine.isHidden = true
        needsLayout = true
        layoutSubtreeIfNeeded()
    }

    /// 被拖行跟随指针，其他行按 `insertionIndex` 让位；插入指示线画在落点。
    private func applyDragOffsets() {
        guard let drag, drag.isActive, let session = dragSession, let insertion = drag.insertionIndex else {
            insertionLine.isHidden = true
            return
        }
        let slots = dragGroup.compactMap { homeY[$0] }
        guard slots.count == dragGroup.count, let originalIndex = dragGroup.firstIndex(of: session) else { return }
        let others = dragGroup.filter { $0 != session }
        let rowH = DesignTokens.Spacing.tabRowHeight
        for (i, id) in others.enumerated() {
            let slot = i < insertion ? i : i + 1
            rowView(for: id)?.setFrameOrigin(NSPoint(x: Self.horizontalInset, y: slots[slot]))
        }
        if let dragged = rowView(for: session) {
            let raw = slots[originalIndex] + (dragPointerY - dragStartY)
            let y = min(max(raw, slots.first ?? raw), slots.last ?? raw)
            dragged.setFrameOrigin(NSPoint(x: Self.horizontalInset, y: y))
            // 被拖行盖在其他行上面。
            addSubview(dragged, positioned: .above, relativeTo: nil)
            addSubview(insertionLine, positioned: .above, relativeTo: nil)
        }
        let lineY = insertion <= originalIndex ? slots[insertion] - Self.rowSpacing / 2 : slots[insertion] + rowH + Self.rowSpacing / 2
        insertionLine.frame = NSRect(x: Self.horizontalInset, y: max(0, lineY - 1), width: bounds.width - Self.horizontalInset * 2, height: 2)
        insertionLine.layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.accent))
        insertionLine.isHidden = false
    }

    /// 触控板 / 滚轮滚动：拖拽期间立即取消。
    /// 行视图不重写 scrollWheel，事件沿响应链冒泡到这里；这里处理后继续交给 scroll view 正常滚动。
    override func scrollWheel(with event: NSEvent) {
        cancelDrag()
        super.scrollWheel(with: event)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let resignObserver { NotificationCenter.default.removeObserver(resignObserver) }
        resignObserver = nil
        guard let window else {
            cancelDrag()
            return
        }
        resignObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didResignKeyNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.cancelDrag() }
        }
    }

    // MARK: - 鼠标

    override func mouseDown(with event: NSEvent) {
        // 行与组标题自己处理点击；落到这里的是空白处。
        discardDragState()
        if event.clickCount == 2 { onBlankDoubleClick?() }
    }

    override func rightMouseDown(with event: NSEvent) {
        discardDragState()
        onBlankRightClick?(convert(event.locationInWindow, from: nil))
    }

    // MARK: - 行回调（由 TabRowView 转发）

    func rowMouseDown(_ row: TabRowView, event: NSEvent) {
        if event.clickCount == 2 {
            discardDragState()
            onRename?(row.model.item.id)
            return
        }
        let y = convert(event.locationInWindow, from: nil).y
        // 先建拖拽状态再激活：激活触发的刷新会走"结构不变就地更新"，不会重建视图。
        dragBegan(session: row.model.item.id, pointerY: y)
        onActivate?(row.model.item.id)
    }

    func rowMouseDragged(_ row: TabRowView, event: NSEvent) {
        dragMoved(pointerY: convert(event.locationInWindow, from: nil).y)
    }

    func rowMouseUp(_ row: TabRowView, event: NSEvent) {
        dragEnded()
    }

    func rowRightMouseDown() {
        cancelDrag()
    }

    func rowCloseClicked(_ row: TabRowView) {
        onClose?([row.model.item.id])
    }

    func rowMenu(for session: SessionID) -> NSMenu? { rowMenuProvider?(session) }
}

/// 分组标题："分屏 N"，点击切换到该 pane。
@MainActor
final class SidebarHeaderView: NSView {
    var pane: PaneID?
    var onClick: ((PaneID) -> Void)?
    private let label = NSTextField(labelWithString: "")

    override var isFlipped: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: .semibold)
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SidebarHeaderView 只支持代码创建") }

    func configure(titleIndex: Int, isActive: Bool) {
        label.stringValue = String(localized: "Split \(titleIndex)")
        label.textColor = .token(isActive ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary)
    }

    override func layout() {
        super.layout()
        label.sizeToFit()
        label.frame = NSRect(
            x: DesignTokens.Spacing.tabRowPaddingX, y: (bounds.height - label.frame.height) / 2 + 2,
            width: bounds.width - DesignTokens.Spacing.tabRowPaddingX * 2, height: label.frame.height
        )
    }

    /// 标题文字不应吞掉点击，整块区域都由自己响应。
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }

    override func mouseDown(with event: NSEvent) {
        if let pane { onClick?(pane) }
    }
}
