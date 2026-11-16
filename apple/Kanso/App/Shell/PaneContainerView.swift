import AppKit
import KansoUI
import TerminalKit

/// 分屏区：按 `Workspace.layout` 摆放每个 pane 当前活动标签的终端视图，并提供分隔条拖动。
///
/// 终端视图实例由控制器持有并复用；这里只负责把它们加进 / 移出视图层级并设置 frame。
/// 只有活动标签的视图在层级里，其余标签的视图被摘下（对应 core 侧 `setVisible(false)`）。
@MainActor
final class PaneContainerView: NSView {
    /// 分隔条线宽（pt）。
    static let dividerThickness: CGFloat = 1
    /// 分隔条命中区域向两侧各扩的宽度：1pt 的线几乎点不中。
    static let dividerHitInset: CGFloat = 3
    /// 分屏的最小像素尺寸：宽 160pt、高 80pt。
    static let minPaneWidth: CGFloat = 160
    static let minPaneHeight: CGFloat = 80

    private let workspace: Workspace
    private let controller: WorkspaceController
    private var placedViews: [PaneID: NSView] = [:]
    private var handles: [DividerHandleView] = []
    private var dimViews: [PaneID: PaneDimView] = [:]
    private let emptyState = EmptyStateView()
    /// 固定的图层顺序（自下而上）：空状态页 < 终端 < 淡遮罩 < 分隔条。
    /// 用三个容器而不是靠 `positioned:` 排序：后加入的终端视图不会盖到遮罩上面。
    private let terminalLayer = PassthroughView()
    private let dimLayer = PassthroughView()
    /// 上一次已把焦点交给的会话；只在活动会话变化时抢焦点，避免标题刷新打断侧栏搜索框的输入。
    private var lastFocusedSession: SessionID?
    var sftpViewProvider: ((SessionID) -> SftpDualView?)?

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    init(controller: WorkspaceController) {
        self.controller = controller
        workspace = controller.workspace
        super.init(frame: .zero)
        wantsLayer = true
        emptyState.onNewLocalShell = { [weak controller] in
            controller?.perform { try controller?.openLocalShell() }
        }
        emptyState.onNewSession = { [weak controller] in controller?.sessions.presentEditor(editing: nil) }
        emptyState.onImportSshConfig = { [weak controller] in controller?.sessions.presentSshImport() }
        for layer in [emptyState, terminalLayer, dimLayer] { addSubview(layer) }
        controller.paneSizeProvider = { [weak self] pane in self?.paneFrames()[pane]?.size }
        controller.addObserver { [weak self] in self?.reload() }
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("PaneContainerView 只支持代码创建") }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgWindow))
    }

    override func layout() {
        super.layout()
        reload()
    }

    private func paneFrames() -> [PaneID: CGRect] {
        workspace.layout.frames(in: bounds, dividerThickness: Self.dividerThickness)
    }

    /// 按模型重新摆放。幂等；分隔条拖动期间每次移动都会调用。
    func reload() {
        let frames = paneFrames()
        let multiPane = workspace.allPanes.count > 1

        // 1. 终端视图：每个 pane 一个（活动标签）。
        var desired: [PaneID: NSView] = [:]
        for pane in workspace.allPanes {
            if let session = workspace.activeTab(in: pane), let view = workspaceView(for: session) {
                desired[pane] = view
            }
        }
        for (pane, view) in placedViews where desired[pane] !== view {
            // 视图可能已经被搬到别的 pane（跨 pane 移动），只在它还在本容器里时摘下。
            if !desired.values.contains(where: { $0 === view }), view.superview === terminalLayer { view.removeFromSuperview() }
        }
        for (pane, view) in desired {
            if view.superview !== terminalLayer { terminalLayer.addSubview(view) }
            if let frame = frames[pane], view.frame != frame { view.frame = frame }
        }
        placedViews = desired

        // 2. 空状态页：只有"唯一 pane 且没有标签"才会出现。
        emptyState.isHidden = !desired.isEmpty
        for layer in [emptyState, terminalLayer, dimLayer] { layer.frame = bounds }

        // 3. 非活动 pane 的淡遮罩（单 pane 时不需要）。
        for pane in workspace.allPanes {
            let needsDim = multiPane && pane != workspace.activePane && desired[pane] != nil
            if needsDim, let frame = frames[pane] {
                let dim = dimViews[pane] ?? makeDimView(pane)
                dim.frame = frame
                dim.isHidden = false
            } else {
                dimViews[pane]?.isHidden = true
            }
        }
        for pane in dimViews.keys where !workspace.allPanes.contains(pane) {
            dimViews.removeValue(forKey: pane)?.removeFromSuperview()
        }

        // 4. 分隔条：复用 handle 对象。拖动中 handle 收着鼠标事件，重建会丢事件流。
        let dividers = workspace.layout.dividers(in: bounds, dividerThickness: Self.dividerThickness)
        while handles.count < dividers.count {
            let handle = DividerHandleView()
            handle.container = self
            handles.append(handle)
            addSubview(handle)
        }
        while handles.count > dividers.count { handles.removeLast().removeFromSuperview() }
        for (handle, divider) in zip(handles, dividers) {
            handle.divider = divider
            handle.frame = Self.hitRect(for: divider)
            handle.needsDisplay = true
        }

        focusActiveTerminalIfChanged()
    }

    private func makeDimView(_ pane: PaneID) -> PaneDimView {
        let dim = PaneDimView()
        dimViews[pane] = dim
        dimLayer.addSubview(dim)
        return dim
    }

    static func hitRect(for divider: LayoutDivider) -> CGRect {
        switch divider.axis {
        case .horizontal: divider.rect.insetBy(dx: -dividerHitInset, dy: 0)
        case .vertical: divider.rect.insetBy(dx: 0, dy: -dividerHitInset)
        }
    }

    // MARK: - 焦点

    private func workspaceView(for session: SessionID) -> NSView? {
        if workspace.item(session)?.kind == .sftp { return sftpViewProvider?(session) }
        return controller.host(for: session)?.view
    }

    private func focusActiveTerminalIfChanged() {
        let active = workspace.activeSession
        guard active != lastFocusedSession else { return }
        if focusActiveTerminal() { lastFocusedSession = active }
    }

    /// 把键盘焦点交给活动终端；视图还不在窗口里时返回 false（下次刷新再试）。
    @discardableResult
    func focusActiveTerminal() -> Bool {
        guard let session = workspace.activeSession, let view = workspaceView(for: session),
              let window = view.window else { return false }
        let target: NSView = (view as? SftpDualView)?.remotePanel.table ?? view
        if window.firstResponder !== target { window.makeFirstResponder(target) }
        return true
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        // 首次进入窗口时视图才有 window，补一次焦点。
        if window != nil { lastFocusedSession = nil; focusActiveTerminalIfChanged() }
    }

    // MARK: - 分隔条拖动

    /// 把分隔条拖到指针位置（容器坐标）。`grabOffset` 是按下时指针相对分隔条中心的偏移，
    /// 用绝对位置而不是累计位移：夹到最小尺寸后指针再回来，分隔条不会滞后。
    func dragDivider(_ divider: LayoutDivider, pointer: CGPoint, grabOffset: CGFloat) {
        guard divider.parentLength > 0 else { return }
        let center: CGFloat
        let pointerValue: CGFloat
        let minLength: CGFloat
        switch divider.axis {
        case .horizontal:
            center = divider.rect.midX; pointerValue = pointer.x; minLength = Self.minPaneWidth
        case .vertical:
            center = divider.rect.midY; pointerValue = pointer.y; minLength = Self.minPaneHeight
        }
        let delta = (pointerValue - grabOffset - center) / divider.parentLength
        workspace.moveDivider(
            splitPath: divider.splitPath, dividerIndex: divider.index, delta: Double(delta),
            minFraction: Double(minLength / divider.parentLength)
        )
        // 拖动要跟手：不等 Observation 的异步刷新，立刻重排。
        reload()
    }

    /// 当前几何下，与 `index` 对应的分隔条（拖动时模型已变，需要取最新位置）。
    func currentDivider(matching old: LayoutDivider) -> LayoutDivider? {
        workspace.layout.dividers(in: bounds, dividerThickness: Self.dividerThickness)
            .first { $0.splitPath == old.splitPath && $0.index == old.index }
    }
}

/// 分隔条：1pt 的线，命中区域上下 / 左右各扩 3pt。
@MainActor
final class DividerHandleView: NSView {
    weak var container: PaneContainerView?
    var divider: LayoutDivider?
    private var grabOffset: CGFloat = 0

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        let inset = PaneContainerView.dividerHitInset
        switch divider?.axis {
        case .horizontal: NSRect(x: inset, y: 0, width: PaneContainerView.dividerThickness, height: bounds.height).fill()
        case .vertical: NSRect(x: 0, y: inset, width: bounds.width, height: PaneContainerView.dividerThickness).fill()
        case nil: break
        }
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: divider?.axis == .vertical ? .resizeUpDown : .resizeLeftRight)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        guard let container, let divider else { return }
        let p = container.convert(event.locationInWindow, from: nil)
        grabOffset = divider.axis == .horizontal ? p.x - divider.rect.midX : p.y - divider.rect.midY
    }

    override func mouseDragged(with event: NSEvent) {
        guard let container, let divider else { return }
        container.dragDivider(divider, pointer: container.convert(event.locationInWindow, from: nil), grabOffset: grabOffset)
    }
}

/// 只负责分层、自己不接收鼠标的容器。
@MainActor
final class PassthroughView: NSView {
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit === self ? nil : hit
    }
}

/// 非活动 pane 上的淡遮罩，提示当前焦点在哪个分屏。不拦截鼠标。
@MainActor
final class PaneDimView: NSView {
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        // 终端背景色由主题决定（可能深可能浅），用半透明黑在两种背景上都只是"略暗"，不会打扰阅读。
        NSColor.black.withAlphaComponent(0.14).setFill()
        bounds.fill()
    }
}

/// 空状态页（ui-shell §11）：居中的三个入口——"新建本地 Shell"（主按钮，带快捷键提示）、
/// "新建会话…"、"导入 OpenSSH 配置…"（次要按钮）。
@MainActor
final class EmptyStateView: NSView {
    var onNewLocalShell: (() -> Void)?
    var onNewSession: (() -> Void)?
    var onImportSshConfig: (() -> Void)?
    let newShellButton = KButton(title: String(localized: "New Local Shell"), symbol: "terminal", style: .primary)
    let newSessionButton = KButton(title: String(localized: "New Session…"), symbol: "plus", style: .secondary)
    let importButton = KButton(title: String(localized: "Import OpenSSH Config…"), symbol: "arrow.down.doc", style: .secondary)
    private let hint = NSTextField(labelWithString: "")

    /// 三个按钮统一宽度，竖直居中排列。
    private static let buttonWidth: CGFloat = 220

    override var isFlipped: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        newShellButton.onPress = { [weak self] in self?.onNewLocalShell?() }
        newSessionButton.onPress = { [weak self] in self?.onNewSession?() }
        importButton.onPress = { [weak self] in self?.onImportSshConfig?() }
        hint.stringValue = String(localized: "or press ⌘T")
        hint.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        hint.textColor = .token(DesignTokens.Color.textTertiary)
        hint.alignment = .center
        for v in [newShellButton, hint, newSessionButton, importButton] as [NSView] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("EmptyStateView 只支持代码创建") }

    override func layout() {
        super.layout()
        let height = DesignTokens.Spacing.fieldHeight
        let gap = DesignTokens.Spacing.sm
        let hintHeight: CGFloat = 16
        let total = height + gap + hintHeight + DesignTokens.Spacing.lg + height + gap + height
        var y = ((bounds.height - total) / 2).rounded()
        let x = ((bounds.width - Self.buttonWidth) / 2).rounded()
        newShellButton.frame = NSRect(x: x, y: y, width: Self.buttonWidth, height: height)
        y += height + gap
        hint.frame = NSRect(x: 0, y: y, width: bounds.width, height: hintHeight)
        y += hintHeight + DesignTokens.Spacing.lg
        newSessionButton.frame = NSRect(x: x, y: y, width: Self.buttonWidth, height: height)
        y += height + gap
        importButton.frame = NSRect(x: x, y: y, width: Self.buttonWidth, height: height)
    }
}
