import AppKit

/// 实色浮动面板（替代 `NSPopover`，visual-style §3.4）：无边框子窗口，圆角 12、1px 边框、淡阴影，
/// 锚定在某个视图下方；点击面板外部、面板失去 key 状态、按 Esc 都会关闭。
///
/// 为什么不用 `NSPopover`：系统气泡带箭头和玻璃材质，样式无法调整。
///
/// 面板是单次使用的：关闭后丢弃，要再显示就新建一个。`dismiss()` 幂等，关闭的各条路径
/// （点外部、失焦、Esc、调用方主动关）可以随意叠加调用，`onDismiss` 只会触发一次。
/// 普通面板成为 key window；补全使用被动面板，键盘焦点留在终端。
@MainActor
final class FloatingPanel: NSPanel {
    /// 面板关闭（无论哪条路径）后调用一次。
    var onDismiss: (() -> Void)?

    private var mouseMonitor: Any?
    private var resignObserver: NSObjectProtocol?
    private weak var hostWindow: NSWindow?
    private var isDismissed = false
    /// 大于 0 时忽略「点外部 / 失焦」：面板里弹出系统菜单期间，焦点与鼠标事件的归属不稳定，不能因此关闭面板。
    private var suspendCount = 0
    private let passive: Bool

    override var canBecomeKey: Bool { !passive }
    override var canBecomeMain: Bool { false }

    /// `content` 填满面板（圆角底板内）；`size` 是面板的初始尺寸。
    init(content: NSView, size: NSSize, passive: Bool = false) {
        self.passive = passive
        super.init(contentRect: NSRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
        isReleasedWhenClosed = false
        isOpaque = false
        backgroundColor = .clear
        // 窗口阴影随不透明内容的圆角轮廓绘制，足够"淡"；不再自己叠一层图层阴影。
        hasShadow = true
        isMovable = false

        let chrome = StyledBoxView()
        chrome.fillToken = DesignTokens.Color.bgPopover
        chrome.borderToken = DesignTokens.Color.borderStrong
        chrome.cornerRadius = DesignTokens.Radius.xl
        content.frame = chrome.bounds
        content.autoresizingMask = [.width, .height]
        chrome.addSubview(content)
        contentView = chrome
    }

    /// 显示在 `anchorRect`（屏幕坐标）正下方，左边缘对齐，并限制在所在屏幕的可见区域内；
    /// 然后成为 key window。面板作为 `parent` 的子窗口，跟随主窗口移动 / 前置。
    func present(below anchorRect: NSRect, parent: NSWindow, within bounds: NSRect? = nil) {
        self.hostWindow = parent
        position(below: anchorRect, within: bounds ?? (parent.screen ?? NSScreen.main)?.visibleFrame)
        parent.addChildWindow(self, ordered: .above)
        if passive { orderFront(nil) } else { makeKeyAndOrderFront(nil) }

        // 本地监视器：点击其它窗口 / 主窗口内容都算"外部"。只观察、不吞掉事件，
        // 所以点到按钮时按钮照常响应。
        mouseMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]) { [weak self] event in
            self?.handleMouseDown(in: event.window)
            return event
        }
        resignObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didResignKeyNotification, object: passive ? parent : self, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.handleResignKey() }
        }
    }

    func position(below anchorRect: NSRect, within bounds: NSRect?) {
        let gap: CGFloat = 6
        var origin = NSPoint(x: anchorRect.minX, y: anchorRect.minY - gap - frame.height)
        if let visible = bounds {
            if passive, origin.y < visible.minY { origin.y = anchorRect.maxY + gap }
            origin.x = min(max(origin.x, visible.minX + 8), visible.maxX - frame.width - 8)
            origin.y = min(max(origin.y, visible.minY + 8), visible.maxY - frame.height - 8)
        }
        setFrameOrigin(origin)
    }

    /// 改变面板高度 / 宽度，保持左上角不动（内容变化后向下伸缩）。
    func resize(to size: NSSize) {
        var frame = self.frame
        frame.origin.y += frame.height - size.height
        frame.size = size
        setFrame(frame, display: true)
        invalidateShadow()
    }

    // MARK: - 关闭路径（internal：测试直接调用）

    /// 一次鼠标按下发生在 `window` 中：不是面板自己就关闭。
    func handleMouseDown(in window: NSWindow?) {
        if window !== self, suspendCount == 0 { dismiss() }
    }

    func handleResignKey() {
        if suspendCount == 0 { dismiss() }
    }

    /// 执行会阻塞的菜单弹出（`NSMenu.popUp`）期间暂停自动关闭。
    func withoutAutoDismiss(_ body: () -> Void) {
        suspendCount += 1
        defer { suspendCount -= 1 }
        body()
    }

    override func cancelOperation(_ sender: Any?) { dismiss() }

    func dismiss() {
        guard !isDismissed else { return }
        isDismissed = true
        if let mouseMonitor { NSEvent.removeMonitor(mouseMonitor) }
        mouseMonitor = nil
        if let resignObserver { NotificationCenter.default.removeObserver(resignObserver) }
        resignObserver = nil
        let wasKey = isKeyWindow
        hostWindow?.removeChildWindow(self)
        orderOut(nil)
        // 只在面板自己是 key 时把焦点还给主窗口；用户已切到别的应用 / 窗口时不要抢回来。
        if wasKey { hostWindow?.makeKey() }
        onDismiss?()
    }
}
