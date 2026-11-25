import AppKit
import KansoUI

/// 顶部标签条（ui-shell.md §2.5）：一个分屏一条，只显示本分屏的标签，末尾固定一个 ＋ 按钮。
///
/// 标签等宽：放得下时取最大宽度，放不下先一起变窄到最小宽度，再不够就横向滚动。
/// 等宽也让拖拽排序简单：让位时每个标签落到的槽位就是原来的槽位，不用按宽度重排。
///
/// 拖拽状态与 `SidebarListView` 一样放在容器里，并以 internal 方法暴露（`dragBegan` / `dragMoved` /
/// `dragEnded` / `cancelDrag`），测试可以直接调用。坐标一律用文档视图（`content`）的 x。
@MainActor
final class TabStripView: NSView, TabRowHost {
    static let height: CGFloat = 34
    static let chipHeight: CGFloat = 26
    static let chipMinWidth: CGFloat = 110
    static let chipMaxWidth: CGFloat = 200
    static let chipGap: CGFloat = DesignTokens.Spacing.xs
    static let horizontalInset: CGFloat = 6
    static let plusWidth: CGFloat = 28

    let pane: PaneID

    var onActivate: ((SessionID) -> Void)?
    var onActivatePane: ((PaneID) -> Void)?
    var onClose: (([SessionID]) -> Void)?
    var onRename: ((SessionID) -> Void)?
    /// 拖拽正常结束：`(会话, 本分屏内插入位置)`。
    var onMove: ((SessionID, Int) -> Void)?
    var onNewSession: ((PaneID) -> Void)?
    var onBlankDoubleClick: ((PaneID) -> Void)?
    /// 空白处右键：参数为点击位置（本视图坐标）。
    var onBlankRightClick: ((PaneID, NSPoint) -> Void)?
    var rowMenuProvider: ((SessionID) -> NSMenu?)?
    /// 拖拽结束（提交或取消）后调用，容器据此执行被推迟的重建。
    var onDragFinished: (() -> Void)?

    let plusButton = NSButton()
    private let scrollView = NSScrollView()
    private let content = TabStripContentView()
    private let insertionLine = NSView()
    private(set) var chips: [TabRowView] = []
    private var chipWidth: CGFloat = TabStripView.chipMaxWidth
    /// 每个标签的 home 位置（文档视图坐标），布局时算出。
    private var homeX: [SessionID: CGFloat] = [:]
    private var lastActiveTab: SessionID?

    // 拖拽状态
    private var drag: TabReorderDrag?
    private var dragSession: SessionID?
    private var dragStartX: CGFloat = 0
    private var dragPointerX: CGFloat = 0
    private var resignObserver: NSObjectProtocol?

    override var isFlipped: Bool { true }

    init(pane: PaneID) {
        self.pane = pane
        super.init(frame: .zero)

        scrollView.drawsBackground = false
        scrollView.hasHorizontalScroller = true
        scrollView.hasVerticalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.verticalScrollElasticity = .none
        scrollView.documentView = content
        content.strip = self

        plusButton.isBordered = false
        plusButton.image = .symbol("plus", description: String(localized: "New Session…"))
        plusButton.imagePosition = .imageOnly
        plusButton.contentTintColor = .token(DesignTokens.Color.textSecondary)
        plusButton.toolTip = String(localized: "New Session…")
        plusButton.target = self
        plusButton.action = #selector(newSession)

        insertionLine.wantsLayer = true
        insertionLine.isHidden = true
        content.addSubview(insertionLine)
        for v in [scrollView, plusButton] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("TabStripView 只支持代码创建") }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.token(DesignTokens.Color.bgSidebar).setFill()
        bounds.fill()
        // 底部一条细线，把标签条与终端分开。
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        NSRect(x: 0, y: bounds.height - 1, width: bounds.width, height: 1).fill()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: - 数据

    /// 是否正在跟踪拖拽（含尚未激活的按下状态）。
    var isDragTracking: Bool { drag != nil }
    var isDragActive: Bool { drag?.isActive ?? false }

    /// 更新内容。id 序列不变时只就地更新标签内容，不重建视图（保住悬停与拖拽状态）。
    /// 返回 false 表示结构变化但正在拖拽，调用方要在 `onDragFinished` 时重试。
    @discardableResult
    func update(_ models: [TabRowView.Model], activeTab: SessionID?) -> Bool {
        if models.map(\.item.id) == chips.map(\.model.item.id) {
            for (chip, model) in zip(chips, models) { chip.model = model }
        } else {
            // 重建会丢掉拖拽状态，推迟到拖拽结束。
            if isDragTracking { return false }
            rebuild(models)
        }
        // 活动标签变化（点击 / 快捷键 / 新开）时滚动到可见。
        if activeTab != lastActiveTab {
            lastActiveTab = activeTab
            if let activeTab {
                layoutSubtreeIfNeeded()
                scrollToVisible(session: activeTab)
            }
        }
        return true
    }

    private func rebuild(_ models: [TabRowView.Model]) {
        var existing = Dictionary(uniqueKeysWithValues: chips.map { ($0.model.item.id, $0) })
        chips = models.map { model in
            let chip = existing.removeValue(forKey: model.item.id) ?? TabRowView(model: model, style: .chip)
            chip.model = model
            chip.host = self
            if chip.superview == nil { content.addSubview(chip) }
            return chip
        }
        for chip in existing.values { chip.removeFromSuperview() }
        content.addSubview(insertionLine, positioned: .above, relativeTo: nil) // 始终在最上层
        needsLayout = true
        layoutSubtreeIfNeeded()
        refreshHover()
    }

    /// 重建后补算悬停：鼠标静止不动时不会再有 mouseEntered，标签会一直处于"未悬停"。
    private func refreshHover() {
        guard let window else { return }
        let p = content.convert(window.mouseLocationOutsideOfEventStream, from: nil)
        let visible = content.visibleRect
        for chip in chips { chip.isHovered = chip.frame.contains(p) && visible.contains(p) }
    }

    func chip(for session: SessionID) -> TabRowView? { chips.first { $0.model.item.id == session } }

    func scrollToVisible(session: SessionID) {
        if let chip = chip(for: session) { content.scrollToVisible(chip.frame.insetBy(dx: -Self.chipGap, dy: 0)) }
    }

    // MARK: - 布局

    override func layout() {
        super.layout()
        let h = bounds.height
        plusButton.frame = NSRect(x: bounds.width - Self.plusWidth - 2, y: (h - 24) / 2, width: Self.plusWidth, height: 24)
        scrollView.frame = NSRect(x: 0, y: 0, width: max(0, plusButton.frame.minX - 2), height: h - 1)
        layoutChips()
    }

    private func layoutChips() {
        let visibleWidth = scrollView.contentSize.width
        let n = CGFloat(chips.count)
        if n > 0 {
            let fit = (visibleWidth - Self.horizontalInset * 2 - Self.chipGap * (n - 1)) / n
            chipWidth = min(max(fit, Self.chipMinWidth), Self.chipMaxWidth).rounded(.down)
        }
        let contentWidth = Self.horizontalInset * 2 + n * chipWidth + max(0, n - 1) * Self.chipGap
        // 文档视图至少和可视区一样宽：空白处才能接收双击 / 右键。
        content.setFrameSize(NSSize(width: max(contentWidth, visibleWidth), height: scrollView.contentSize.height))
        let y = ((content.bounds.height - Self.chipHeight) / 2).rounded()
        homeX = [:]
        var x = Self.horizontalInset
        for chip in chips {
            chip.frame = NSRect(x: x, y: y, width: chipWidth, height: Self.chipHeight)
            homeX[chip.model.item.id] = x
            x += chipWidth + Self.chipGap
        }
        applyDragOffsets()
    }

    // MARK: - 拖拽排序

    /// 按下：先丢弃残留的旧拖拽状态，再新建。
    /// 旧状态可能因为丢失 mouseUp（窗口外松开、被系统打断）残留，不清掉会让后续点击都被当成拖拽的延续。
    func dragBegan(session: SessionID, pointerX: CGFloat) {
        discardDragState()
        // `TabReorderDrag` 只认一维坐标，参数名里的 Y 只是历史命名；横向传 x 即可。
        let rows = chips.compactMap { chip -> (id: SessionID, minY: CGFloat, maxY: CGFloat)? in
            let id = chip.model.item.id
            guard let x = homeX[id] else { return nil }
            return (id, x, x + chipWidth)
        }
        guard rows.contains(where: { $0.id == session }) else { return }
        drag = TabReorderDrag(dragged: session, startY: pointerX, rows: rows)
        dragSession = session
        dragStartX = pointerX
        dragPointerX = pointerX
    }

    func dragMoved(pointerX: CGFloat) {
        guard drag != nil else { return }
        dragPointerX = pointerX
        drag?.update(pointerY: pointerX)
        if drag?.isActive == true { applyDragOffsets() }
    }

    /// 正常的左键松开：激活过才提交排序；只是点击则什么都不改。
    func dragEnded() {
        guard let drag, let session = dragSession else { return }
        let index = drag.isActive ? drag.insertionIndex : nil
        finishDragVisuals()
        if let index { onMove?(session, index) }
        onDragFinished?()
    }

    /// 取消：恢复界面，不改顺序。滚动、窗口失去 key、视图离开窗口等打断时调用。
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
        insertionLine.isHidden = true
        needsLayout = true
        layoutSubtreeIfNeeded()
    }

    /// 被拖标签跟随指针，其他标签按 `insertionIndex` 让位；插入指示线画在落点。
    private func applyDragOffsets() {
        guard let drag, drag.isActive, let session = dragSession, let insertion = drag.insertionIndex else {
            insertionLine.isHidden = true
            return
        }
        let order = chips.map(\.model.item.id)
        let slots = order.compactMap { homeX[$0] }
        guard slots.count == order.count, let originalIndex = order.firstIndex(of: session) else { return }
        let y = ((content.bounds.height - Self.chipHeight) / 2).rounded()
        for (i, id) in order.filter({ $0 != session }).enumerated() {
            let slot = i < insertion ? i : i + 1
            chip(for: id)?.setFrameOrigin(NSPoint(x: slots[slot], y: y))
        }
        if let dragged = chip(for: session) {
            let raw = slots[originalIndex] + (dragPointerX - dragStartX)
            dragged.setFrameOrigin(NSPoint(x: min(max(raw, slots.first ?? raw), slots.last ?? raw), y: y))
            // 被拖标签盖在其他标签上面。
            content.addSubview(dragged, positioned: .above, relativeTo: nil)
            content.addSubview(insertionLine, positioned: .above, relativeTo: nil)
        }
        let lineX = insertion <= originalIndex
            ? slots[insertion] - Self.chipGap / 2
            : slots[insertion] + chipWidth + Self.chipGap / 2
        insertionLine.frame = NSRect(x: lineX - 1, y: y, width: 2, height: Self.chipHeight)
        insertionLine.layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.accent))
        insertionLine.isHidden = false
    }

    /// 滚轮 / 触控板：拖拽期间先取消。只有竖向分量的滚轮（普通鼠标）换成横向滚动，
    /// 否则标签条放不下时用鼠标滚轮翻不动。
    func handleScrollWheel(_ event: NSEvent) -> Bool {
        cancelDrag()
        guard abs(event.scrollingDeltaY) > abs(event.scrollingDeltaX) else { return false }
        let clip = scrollView.contentView
        let maxX = max(0, content.frame.width - clip.bounds.width)
        let delta = event.hasPreciseScrollingDeltas ? event.scrollingDeltaY : event.scrollingDeltaY * 10
        let x = min(max(clip.bounds.origin.x - delta, 0), maxX)
        clip.scroll(to: NSPoint(x: x, y: clip.bounds.origin.y))
        scrollView.reflectScrolledClipView(clip)
        return true
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

    @objc private func newSession() { onNewSession?(pane) }

    /// 空白处（文档视图或标签条边缘）按下：单击切到本分屏，双击复制当前标签。
    func blankMouseDown(_ event: NSEvent) {
        discardDragState()
        if event.clickCount == 2 {
            onBlankDoubleClick?(pane)
        } else {
            onActivatePane?(pane)
        }
    }

    func blankRightMouseDown(_ event: NSEvent) {
        discardDragState()
        onBlankRightClick?(pane, convert(event.locationInWindow, from: nil))
    }

    override func mouseDown(with event: NSEvent) { blankMouseDown(event) }
    override func rightMouseDown(with event: NSEvent) { blankRightMouseDown(event) }

    // MARK: - TabRowHost

    func rowMouseDown(_ row: TabRowView, event: NSEvent) {
        if event.clickCount == 2 {
            discardDragState()
            onRename?(row.model.item.id)
            return
        }
        // 先建拖拽状态再激活：激活触发的刷新会走"结构不变就地更新"，不会重建视图。
        dragBegan(session: row.model.item.id, pointerX: content.convert(event.locationInWindow, from: nil).x)
        onActivate?(row.model.item.id)
    }

    func rowMouseDragged(_ row: TabRowView, event: NSEvent) {
        dragMoved(pointerX: content.convert(event.locationInWindow, from: nil).x)
    }

    func rowMouseUp(_ row: TabRowView, event: NSEvent) { dragEnded() }

    func rowRightMouseDown() { cancelDrag() }

    func rowCloseClicked(_ row: TabRowView) { onClose?([row.model.item.id]) }

    func rowMenu(for session: SessionID) -> NSMenu? { rowMenuProvider?(session) }
}

/// 标签条的文档视图：把空白处的鼠标与滚轮交回 `TabStripView`。
@MainActor
final class TabStripContentView: NSView {
    weak var strip: TabStripView?

    override var isFlipped: Bool { true }

    override func mouseDown(with event: NSEvent) { strip?.blankMouseDown(event) }
    override func rightMouseDown(with event: NSEvent) { strip?.blankRightMouseDown(event) }

    /// 标签视图不重写 scrollWheel，事件沿响应链冒泡到这里。
    override func scrollWheel(with event: NSEvent) {
        if strip?.handleScrollWheel(event) != true { super.scrollWheel(with: event) }
    }
}
