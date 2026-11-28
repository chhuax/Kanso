import AppKit
import KansoBridge
import Metal
import QuartzCore
import os

/// 终端视图：`CAMetalLayer` 承载的 Metal 渲染，按 display link 节拍取帧。
///
/// 取帧模型（architecture §4）：
/// - core 在终端由干净变脏时通知一次，上层调用 `setNeedsFrame()`；
/// - 视图的 display link 在下一个 vsync 调用 `frameSource.takeFrame` 并渲染，
///   **没有待取帧也没有待重绘时立即暂停 display link**，空闲时不消耗 CPU / GPU / 电量；
/// - 窗口不可见（被遮挡、最小化）或视图隐藏时同样暂停，待取帧留到重新可见时再处理，
///   所以后台标签只解析不渲染；
/// - 洪水输出时一个 vsync 只取一次最新帧，中间状态自然被跳过。
///
/// 输入（键盘、IME、鼠标、滚轮）见 `TerminalView+Input.swift` 与 `TerminalView+TextInput.swift`，
/// 结果通过 `inputSink` 输出；选区与菜单不在本视图内。必须在主线程使用。
public final class TerminalView: NSView {
    // MARK: 公开属性

    /// 帧来源；赋值后会立即请求取一帧。
    public var frameSource: FrameSource? {
        didSet { setNeedsFrame() }
    }

    /// 网格尺寸或单元格像素尺寸变化时回调：`(cols, rows, cellWidthPx, cellHeightPx)`。
    /// 上层据此调用 core 的 resize。视图首次拿到有效尺寸后会回调一次。
    public var onGridSizeChange: ((Int, Int, Int, Int) -> Void)?

    /// 帧已在屏幕呈现时回调，供启动基准确认首帧；普通会话不设置。
    public var onFramePresented: (@MainActor @Sendable (FrameSnapshot) -> Void)?

    /// 终端字体（`pointSize` 即字号）。改变后清空图集、重排并回调 `onGridSizeChange`。
    public var font: NSFont {
        get { renderer?.font ?? .monospacedSystemFont(ofSize: 14, weight: .regular) }
        set {
            renderer?.font = newValue
            layoutDidChange()
        }
    }

    public var lineHeight: Double {
        get { renderer?.lineHeight ?? 1.2 }
        set {
            renderer?.lineHeight = newValue
            layoutDidChange()
        }
    }

    /// 时间戳与行号栏模式。栏宽变化会改变列数，因此会回调 `onGridSizeChange`。
    public var gutterMode: GutterMode {
        get { renderer?.gutterMode ?? .both }
        set {
            renderer?.gutterMode = newValue
            layoutDidChange()
        }
    }

    /// 最低对比度保护开关（默认开启）。
    public var isMinimumContrastEnabled: Bool {
        get { renderer?.isMinimumContrastEnabled ?? true }
        set {
            renderer?.isMinimumContrastEnabled = newValue
            requestRedraw()
        }
    }

    /// 最低对比度阈值（默认 4.5）。
    public var minimumContrastRatio: Double {
        get { renderer?.minimumContrastRatio ?? 4.5 }
        set {
            renderer?.minimumContrastRatio = newValue
            requestRedraw()
        }
    }

    /// 浅色外观使用的终端配色（默认 Kanso Light）。外观切换时实时生效，正在运行的会话不受影响
    /// （帧里的颜色按索引保存，architecture §11）。
    public var lightPalette: TerminalPalette {
        get { renderer?.lightPalette ?? .kansoLight }
        set {
            renderer?.lightPalette = newValue
            paletteDidChange()
        }
    }

    /// 深色外观使用的终端配色（默认 Kanso Dark）。
    public var darkPalette: TerminalPalette {
        get { renderer?.darkPalette ?? .kansoDark }
        set {
            renderer?.darkPalette = newValue
            paletteDidChange()
        }
    }

    /// 当前外观下实际生效的配色。
    public var currentPalette: TerminalPalette {
        currentAppearance == .light ? lightPalette : darkPalette
    }

    /// 按键拦截点：在任何按键处理（含输入法）之前调用，返回 true 表示宿主已处理 / 吞掉该按键，视图不再处理。
    ///
    /// 用途：用户把某个命令绑定到了菜单没有认领的组合上（例如菜单项因无选区而不可用时的复制键），
    /// 这些键不能漏给 shell（terminal.md §8.2）；以及普通 Ctrl+字母绑定为复制键时有选区就复制。
    /// 宿主只应对**用户绑定过的组合**返回 true：普通 Ctrl/Alt+字母永远不拦截（terminal.md §9）。
    public var keyInterceptor: (@MainActor (NSEvent) -> Bool)?
    /// 新帧就绪后通知宿主更新依赖光标的界面。
    public var onFrameChanged: (@MainActor () -> Void)?
    /// 输入或组字变化时使宿主的异步建议失效。
    public var onInputActivity: (@MainActor () -> Void)?
    /// 补全专用按键拦截；组字期间不会调用。
    public var completionKeyInterceptor: (@MainActor (NSEvent) -> Bool)?
    public var onDirectoryChipClick: (@MainActor (NSRect) -> Void)?
    /// 根据帧里的 `hyperlinkId`（OSC 8）查询目标地址。
    public var hyperlinkResolver: ((UInt32) -> String?)?
    /// 打开链接的执行点；生产路径走系统默认程序，测试可注入替身。
    public var openURL: (URL) -> Bool = { NSWorkspace.shared.open($0) }
    var onOpenLinkRejected: ((String) -> Void)?

    /// 时间戳显示用的时区，默认本地时区。
    public var timeZone: TimeZone {
        get { renderer?.timeZone ?? .current }
        set {
            renderer?.timeZone = newValue
            requestRedraw()
        }
    }

    /// 当前网格尺寸（视图还没有有效尺寸时为 nil）。
    public private(set) var gridSize: TerminalGridSize?

    /// 渲染器；没有可用的 Metal 设备时为 nil，视图只显示空白。
    public let renderer: TerminalRenderer?

    /// 输入出口（按键、IME 上屏文字、鼠标、滚轮、焦点）。弱引用：sink 通常是会话层对象，视图不该延长它的寿命。
    public weak var inputSink: TerminalInputSink?
    /// 复制、粘贴、全选的出口（菜单 / ⌘C ⌘V ⌘A 经 responder chain 调到本视图后转给它）。
    /// 弱引用：由持有 core 的上层强持有。
    public weak var editActions: TerminalEditActions?
    /// 选中即复制（terminal.md §8.2）：开启后松开鼠标完成选择时自动复制选区，默认关闭。
    public var copyOnSelect = false
    /// 中键是否粘贴（terminal.md §8.1）；false 表示无动作。程序开启鼠标上报且未按 Shift 时不生效。
    public var middleClickPastes = true
    /// 右键是否弹出菜单；false 表示"复制或粘贴"（有选区复制、没有就粘贴）。程序开启鼠标上报且未按 Shift 时不生效。
    public var rightClickShowsMenu = true
    /// 粘贴读取的剪贴板；仅测试替换成临时剪贴板，避免读写用户的系统剪贴板。
    var pasteboard: NSPasteboard = .general
    public var onCopied: (() -> Void)?
    public var writeCopiedText: (String) -> Bool = { text in
        NSPasteboard.general.clearContents()
        return NSPasteboard.general.setString(text, forType: .string)
    }

    /// 右键菜单提供者。返回 nil 表示不弹菜单（右键照常转给程序）。菜单由上层构造（复制 / 粘贴 / 定位目录等）。
    public var contextMenuProvider: (() -> NSMenu?)?

    /// 视图成为 first responder 后回调（用户点击终端、或被程序设为焦点）。
    /// 宿主据此把"用户点了哪个终端"同步给自己的活动会话状态；回调里应当幂等，因为程序设焦点也会触发它。
    public var onBecomeFirstResponder: (() -> Void)?

    /// 输入法接入点。nil 时使用系统的 `inputContext`；测试注入假输入法，按真实输入法的回调顺序驱动
    /// `NSTextInputClient` 方法（`inputContext.handleEvent` 在测试里不可控）。
    public var inputMethod: TerminalInputMethod?

    // MARK: 内部状态

    var currentFrame: FrameSnapshot? {
        didSet {
            if let frame = currentFrame, let id = selectedCommandBlock,
               frame.flags.contains(.altScreen) || frame.flags.contains(.mouseReporting)
                || !frame.commandBlocks.contains(where: { $0.id == id && $0.canCopyCommand }) {
                selectedCommandBlock = nil
            }
            refreshHoveredLink()
        }
    }
    var selectedCommandBlock: UInt64? {
        didSet {
            renderer?.selectedCommandBlock = selectedCommandBlock
            requestRedraw()
        }
    }
    var hoveredLink: TerminalLinkHit? {
        didSet {
            guard hoveredLink != oldValue else { return }
            renderer?.hoveredLink = hoveredLink
            requestRedraw()
            window?.invalidateCursorRects(for: self)
        }
    }
    var commandKeyDown = false
    var hoveredLocation: NSPoint?
    var clickedLink: TerminalLinkHit?
    var clickedCommandBlock: UInt64?
    var clickedDirectoryChip: NSRect?
    private var needsFrame = false
    private var needsRedraw = false
    private var displayLink: CADisplayLink?
    private let linkTarget = DisplayLinkTarget()
    private var observers: [NSObjectProtocol] = []
    /// 光标闪烁相位。是否闪烁由帧里的 `cursorBlink` 标志决定（默认样式或程序的 DECSCUSR），这里只负责计时。
    let cursorBlinker: CursorBlinker

    // 输入状态。存储属性不能放进 extension，所以放在这里；语义见 TerminalView+Input / +TextInput。
    /// 组字文本（marked text）。仅作覆盖层显示，从不写入终端。
    var markedString = ""
    /// 组字文本内的选中段（输入法正在转换的那一段），UTF-16 范围。
    var markedSelection = NSRange(location: 0, length: 0)
    /// 正在被 `handleEvent` 处理的按键；回调（insertText / doCommand）借此与原始 NSEvent 关联。
    var currentDispatch: KeyDispatch?
    /// 已作为按键事件发出 press / repeat 的按键码；keyUp 只为这些键发 release。
    var pressedKeyCodes: Set<UInt16> = []
    /// 尚未收到 Up 的鼠标按键及其最近一次事件，用于拖动防粘。
    var activeButtons: [TerminalMouseButton: TerminalMouseEvent] = [:]
    /// 当前这次左键交互是否由终端自己处理选区（未交给程序鼠标上报）。
    var leftDragIsSelection = false
    var scrollAccumulator = ScrollAccumulator()
    var reportedFocus = false
    var mouseTrackingArea: NSTrackingArea?
    var menuCommandBlock: UInt64?
    let markedOverlay = MarkedTextOverlayView()
    private var reportedGrid: (Int, Int, Int, Int)?
    private let signposter = OSSignposter(
        subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "terminal-render"
    )

    // MARK: 初始化

    public override convenience init(frame frameRect: NSRect) {
        self.init(frame: frameRect, blinkScheduler: CursorBlinker.systemScheduler)
    }

    /// 指定初始化器。`blinkScheduler` 供测试注入手动时钟，生产路径用系统计时器。
    init(frame frameRect: NSRect, blinkScheduler: @escaping CursorBlinker.Scheduler) {
        renderer = TerminalRenderer()
        cursorBlinker = CursorBlinker(scheduler: blinkScheduler)
        super.init(frame: frameRect)
        configureBlinker()
        wantsLayer = true
        // 内容完全由 Metal 提供，不要让 AppKit 在 resize 时用 draw(_:) 重绘图层。
        layerContentsRedrawPolicy = .never
        linkTarget.owner = self
        // 本地 / 外部文件拖入：只接受文件 URL。远端条目的 file promise 不带文件 URL，所以天然被拒绝。
        registerForDraggedTypes([.fileURL])
        markedOverlay.isHidden = true
        addSubview(markedOverlay)
    }

    @available(*, unavailable)
    public required init?(coder: NSCoder) {
        fatalError("TerminalView 只支持代码创建")
    }

    deinit {
        // CADisplayLink 对 target 是强引用，且在 run loop 里被持有：必须显式 invalidate 才会释放。
        // target 是只持有 weak owner 的代理，所以即使这里漏了，也只是多留一个暂停的 link，不会泄漏视图。
        linkTarget.invalidate()
    }

    // MARK: 图层

    public override func makeBackingLayer() -> CALayer {
        let layer = CAMetalLayer()
        layer.device = renderer?.device
        layer.pixelFormat = MetalResources.colorPixelFormat
        // 只作为渲染目标，不被读回或采样：framebufferOnly 让系统可以用更省的显示路径。
        layer.framebufferOnly = true
        // 渲染目标是非 sRGB 格式、颜色按 sRGB 编码值写入，图层要声明为 sRGB，
        // 否则在广色域屏上会被当作显示器原生色彩空间而偏色（见 TerminalRenderer 的颜色空间说明）。
        layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        layer.isOpaque = true
        layer.contentsScale = window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2
        return layer
    }

    private var metalLayer: CAMetalLayer? { layer as? CAMetalLayer }

    // MARK: 响应链

    public override var acceptsFirstResponder: Bool { true }

    // MARK: 取帧通知

    /// 通知视图 core 有新帧可取（对应 `TerminalDirty` 事件）。
    ///
    /// 只需在终端由干净变脏时调用一次，重复调用无害但没有意义。必须在主线程调用：
    /// core 的回调线程需要先切到主线程（architecture §4）。
    public func setNeedsFrame() {
        needsFrame = true
        updateDisplayLink()
    }

    private func requestRedraw() {
        needsRedraw = true
        updateDisplayLink()
    }

    // MARK: display link

    /// display link 应该运行：有活要干，且视图可见。
    private var isVisible: Bool {
        guard let window, !isHiddenOrHasHiddenAncestor else { return false }
        return window.occlusionState.contains(.visible) && !window.isMiniaturized
    }

    private func updateDisplayLink() {
        let shouldRun = (needsFrame || needsRedraw) && isVisible && renderer != nil
        if shouldRun {
            if displayLink == nil { createDisplayLink() }
            displayLink?.isPaused = false
        } else {
            displayLink?.isPaused = true
        }
    }

    private func createDisplayLink() {
        guard window != nil else { return }
        // NSView.displayLink 绑定视图所在的屏幕，窗口在屏幕间移动时系统自动切换，刷新率随屏幕走。
        let link = displayLink(target: linkTarget, selector: #selector(DisplayLinkTarget.fire(_:)))
        // 允许 ProMotion 的 120 Hz；用不到时系统会自动降频。
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 60, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        linkTarget.link = link
        displayLink = link
    }

    private func destroyDisplayLink() {
        linkTarget.invalidate()
        displayLink = nil
    }

    fileprivate func displayLinkFired() {
        guard isVisible else {
            updateDisplayLink()
            return
        }
        pullFrameIfNeeded()
        if needsRedraw {
            needsRedraw = false
            renderCurrent()
        }
        // 没有后续工作就暂停：空闲时不能让 display link 空转（architecture §4）。
        if !needsFrame && !needsRedraw { displayLink?.isPaused = true }
    }

    /// 有待取帧时向 `frameSource` 取最新一帧。display link 调用；单独成方法是为了测试不必等 vsync。
    func pullFrameIfNeeded() {
        guard needsFrame else { return }
        needsFrame = false
        let state = signposter.beginInterval("TakeFrame")
        // 视图同时持有 currentFrame 和传给 takeFrame 的引用，实现想复用缓冲区必须自己保证旧帧不再被读：
        // 这里随后就会用新帧替换它，所以只在取到新帧时才丢弃旧帧。
        let frame = frameSource?.takeFrame(reusing: currentFrame)
        signposter.endInterval("TakeFrame", state)
        if let frame {
            currentFrame = frame
            needsRedraw = true
            // 新输出到来：光标回到可见并重新计时（输出期间光标不闪），然后按新帧的标志决定是否继续闪烁。
            cursorBlinker.reset()
            updateBlinking()
            // 光标可能移动了：组字覆盖层与候选框都跟随光标（terminal.md §16 场景 7）。
            updateMarkedOverlay()
            window?.invalidateCursorRects(for: self)
            onFrameChanged?()
        }
    }

    /// 当前帧里的光标位置；帧里光标不可见时仍返回其位置，因为输入法候选框与组字显示依赖位置而非可见性。
    var cursorCell: (col: Int, row: Int)? {
        guard let currentFrame else { return nil }
        let cursor = currentFrame.cursor
        return (cursor.col, cursor.row)
    }

    /// 输入起点列与当前光标行的单元格，复用终端实际渲染几何。
    public func completionAnchor(startColumn: UInt16) -> NSRect? {
        guard let geometry = hitGeometry(), let cursor = cursorCell else { return nil }
        let origin = geometry.cellOrigin(col: Int(startColumn), row: cursor.row)
        return NSRect(x: origin.x, y: Double(bounds.height) - origin.y - geometry.cellHeight,
                      width: geometry.cellWidth, height: geometry.cellHeight)
    }

    // MARK: 渲染

    private var currentAppearance: TerminalAppearance { TerminalAppearance(effectiveAppearance) }

    private var currentScale: CGFloat {
        window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2
    }

    private func renderCurrent() {
        guard let renderer, let layer = metalLayer, layer.drawableSize.width > 0, layer.drawableSize.height > 0,
              let drawable = layer.nextDrawable()
        else { return }
        renderer.topInset = safeAreaInsets.top
        if let callback = onFramePresented, let frame = currentFrame {
            let sequence = frame.seq
            drawable.addPresentedHandler { [weak self] _ in
                Task { @MainActor in
                    if let frame = self?.currentFrame, frame.seq == sequence { callback(frame) }
                }
            }
        }
        renderer.render(frame: currentFrame, to: drawable, scale: currentScale, appearance: currentAppearance)
    }

    /// 字体、栏模式、尺寸、倍率变化之后：重算网格、通知上层并重绘。
    private func layoutDidChange() {
        guard let renderer, let layer = metalLayer else { return }
        let scale = currentScale
        layer.contentsScale = scale
        let size = CGSize(width: max(1, (bounds.width * scale).rounded()), height: max(1, (bounds.height * scale).rounded()))
        if layer.drawableSize != size { layer.drawableSize = size }

        renderer.topInset = safeAreaInsets.top
        let grid = renderer.gridSize(for: bounds.size, scale: scale)
        gridSize = grid
        let report = (grid.cols, grid.rows, grid.cellWidthPx, grid.cellHeightPx)
        if reportedGrid == nil || reportedGrid! != report {
            reportedGrid = report
            onGridSizeChange?(report.0, report.1, report.2, report.3)
        }
        requestRedraw()
        updateMarkedOverlay()
    }

    // MARK: 视图生命周期

    public override func layout() {
        super.layout()
        layoutDidChange()
        // 拖拽窗口边缘时 display link 的下一拍可能晚于系统合成新尺寸的时刻，图层会把旧内容拉伸一瞬；
        // 尺寸变化时同步画一帧可以避免这种闪烁。
        if isVisible {
            needsRedraw = false
            renderCurrent()
        }
    }

    public override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        for token in observers { NotificationCenter.default.removeObserver(token) }
        observers.removeAll()
        if let window {
            // 遮挡状态变化（被其他窗口盖住、切换空间）与最小化都会影响可见性。
            let center = NotificationCenter.default
            for name in [NSWindow.didChangeOcclusionStateNotification, NSWindow.didMiniaturizeNotification,
                         NSWindow.didDeminiaturizeNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.visibilityChanged() }
                })
            }
            // 窗口 key 状态影响焦点上报，并要求结束进行中的鼠标拖动（terminal.md §8.1）。
            for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.windowKeyStateChanged() }
                })
            }
            layoutDidChange()
            // 窗口里的第一次显示：即使没有新帧也要把背景画出来。
            requestRedraw()
            updateBlinking()
        } else {
            destroyDisplayLink()
            endAllDrags()
            clearLinkHover()
            clickedLink = nil
            // Timer 由 run loop 持有，离开窗口后必须显式停掉，否则会话关闭后计时器仍在空转。
            cursorBlinker.setActive(false)
        }
    }

    public override func viewDidHide() {
        super.viewDidHide()
        visibilityChanged()
    }

    public override func viewDidUnhide() {
        super.viewDidUnhide()
        visibilityChanged()
    }

    private func visibilityChanged() {
        // 重新可见时内容可能已经过期（隐藏期间尺寸或外观变过），保守地重绘一次。
        if isVisible { needsRedraw = true }
        updateDisplayLink()
        updateBlinking()
    }

    /// 窗口在不同 DPI 的屏幕之间移动、或屏幕缩放改变时调用：图层倍率、drawable 尺寸与图集都要随之更新。
    public override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        layoutDidChange()
    }

    /// 外观切换：只换调色板重画，不需要新帧——帧里的颜色按索引保存（architecture §11）。
    public override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        paletteDidChange()
    }

    /// 配色或外观变化：重画，并让组字覆盖层换上当前配色。
    private func paletteDidChange() {
        markedOverlay.palette = currentPalette
        markedOverlay.needsDisplay = true
        requestRedraw()
    }

    // MARK: 光标闪烁

    private func configureBlinker() {
        cursorBlinker.onChange = { [weak self] visible in
            guard let self else { return }
            renderer?.isCursorPhaseVisible = visible
            requestRedraw()
        }
    }

    /// 是否应该闪烁：帧里光标可见且要求闪烁，窗口是 key（失焦时光标画成空心块，不闪），视图可见。
    /// 在新帧、窗口 key 状态、可见性变化时调用；`setActive` 对重复状态无副作用，所以可以随意调用。
    func updateBlinking() {
        let flags = currentFrame?.flags ?? []
        let wants = flags.contains(.cursorVisible) && flags.contains(.cursorBlink)
        cursorBlinker.setActive(wants && (blinkGateOverride?() ?? (isVisible && window?.isKeyWindow == true)))
    }

    /// 测试注入：覆盖"视图可见且窗口是 key"的判断（离屏测试窗口没有真实的 key / 遮挡状态）。生产代码不设置。
    var blinkGateOverride: (() -> Bool)?

    /// 有按键：光标回到可见并重新计时，免得打字时光标恰好处于隐藏相位。
    func noteUserActivityForBlink() {
        cursorBlinker.reset()
    }
}

/// CADisplayLink 强引用它的 target，所以不能直接把视图当 target，否则视图永远不会释放。
/// 这个代理只弱引用视图。
@MainActor
private final class DisplayLinkTarget: NSObject {
    weak var owner: TerminalView?
    var link: CADisplayLink?

    @objc func fire(_ link: CADisplayLink) {
        guard let owner else {
            link.invalidate()
            return
        }
        owner.displayLinkFired()
    }

    nonisolated func invalidate() {
        MainActor.assumeIsolated {
            link?.invalidate()
            link = nil
        }
    }
}
