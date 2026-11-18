import AppKit
import KansoUI

/// 底部状态栏：左侧依次是"时间轴"开关、当前目录、活动会话状态文字；右侧依次是 Legacy SSH 标记（有旧算法时）与时钟。
///
/// 时钟只在整分钟刷新（Timer 对齐到下一分钟），不要改成每秒刷新：状态栏不显示秒，
/// 每秒唤醒只是白白耗电。
///
/// 布局优先级：右侧时钟与 Legacy 标记按自然宽度固定；左侧空间不足时，状态文字先被截断（直到 0 宽），
/// 然后才压缩当前目录（截断开头、保留末尾），"时间轴"条目始终完整显示。
@MainActor
final class StatusBarView: NSView {
    private let statusLabel = NSTextField(labelWithString: "")
    private let clockLabel = NSTextField(labelWithString: "")
    private let legacyBadge = LegacySshBadge()
    private let timelineItem = StatusBarItemView(
        symbol: "clock", title: String(localized: "Timeline"), isClickable: true
    )
    private let directoryItem = StatusBarItemView(symbol: "folder", title: "", isClickable: false)
    private let senderItem = StatusBarItemView(symbol: "repeat", title: String(localized: "Sender is Running"), isClickable: true)
    var onShowSender: (() -> Void)?
    private let mcpItem = StatusBarItemView(
        symbol: "point.3.connected.trianglepath.dotted", title: "", isClickable: true
    )
    var onOpenMcp: (() -> Void)?

    func setMcpClientCount(_ count: Int) {
        mcpItem.isHidden = count == 0
        mcpItem.title = String(localized: "MCP \(count)")
        needsLayout = true
    }

    var isMcpIndicatorVisible: Bool { !mcpItem.isHidden }
    func clickMcpIndicator() { mcpItem.onClick?() }

    func setSenderRunning(_ running: Bool) {
        senderItem.isHidden = !running
        needsLayout = true
    }
    /// "文件浏览器"开关（在"时间轴"左边，展开时高亮；filer.md §2）。
    private let filerItem = StatusBarItemView(
        symbol: "folder", title: String(localized: "File Browser"), isClickable: true
    )
    /// 临时提示（⌘J 定位结果等）：优先于状态文字显示，几秒后自动消失。
    private var noticeText: String?
    private var noticeIsError = false
    private var noticeTimer: Timer?
    private var baseStatus = ""
    private var baseStatusIsError = false
    private var clockTimer: Timer?

    /// 点击"时间轴"条目。由窗口控制器接到 `GutterModeController.toggleTimeline()`。
    var onToggleTimeline: (() -> Void)?
    /// 点击"文件浏览器"条目。
    var onToggleFiler: (() -> Void)?

    /// 状态栏条目之间的间距。
    static let itemSpacing: CGFloat = 16

    /// 右侧时钟的固定格式 `YYYY/MM/DD HH:MM`。
    ///
    /// 刻意不随系统语言 / 区域变化：状态栏空间固定，宽度必须可预期；也与终端时间戳栏的固定格式风格一致。
    /// locale 固定为 `en_US_POSIX`，否则用户的 12/24 小时制设置会改写 `HH`。
    static func clockString(from date: Date, timeZone: TimeZone = .current) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = timeZone
        f.dateFormat = "yyyy/MM/dd HH:mm"
        return f.string(from: date)
    }

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        for label in [statusLabel, clockLabel] {
            label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            label.textColor = .token(DesignTokens.Color.textSecondary)
            label.lineBreakMode = .byTruncatingTail
            addSubview(label)
        }
        legacyBadge.isHidden = true
        addSubview(legacyBadge)
        timelineItem.toolTip = String(
            localized: "Show or hide timestamps and line numbers. Choose a mode in Session > Timestamps and Line Numbers."
        )
        timelineItem.onClick = { [weak self] in self?.onToggleTimeline?() }
        addSubview(timelineItem)
        filerItem.onClick = { [weak self] in self?.onToggleFiler?() }
        addSubview(filerItem)
        directoryItem.isHidden = true
        senderItem.isHidden = true
        mcpItem.isHidden = true
        mcpItem.onClick = { [weak self] in self?.onOpenMcp?() }
        addSubview(mcpItem)
        senderItem.onClick = { [weak self] in self?.onShowSender?() }
        addSubview(senderItem)
        directoryItem.lineBreakMode = .byTruncatingHead
        addSubview(directoryItem)
        updateClock()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("StatusBarView 只支持代码创建") }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: DesignTokens.Spacing.statusBarHeight)
    }

    /// 更新左侧文字。`isError` 时用警示色。
    func setStatus(_ text: String, isError: Bool = false) {
        baseStatus = text
        baseStatusIsError = isError
        renderStatus()
    }

    private func renderStatus() {
        let text = noticeText ?? baseStatus
        let isError = noticeText != nil ? noticeIsError : baseStatusIsError
        statusLabel.stringValue = text
        statusLabel.textColor = .token(isError ? DesignTokens.Color.statusError : DesignTokens.Color.textSecondary)
        needsLayout = true
    }

    /// 显示临时提示（覆盖状态文字约 6 秒；之后恢复）。
    func showNotice(_ text: String, isError: Bool) {
        noticeText = text
        noticeIsError = isError
        noticeTimer?.invalidate()
        noticeTimer = Timer.scheduledTimer(withTimeInterval: 6, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.noticeText = nil
                self?.renderStatus()
            }
        }
        renderStatus()
    }

    var noticeMessage: String? { noticeText }

    /// 文件浏览器条目：`expanded` 时高亮；`unavailableReason` 非 nil 时置灰并把原因放进悬停说明。
    func setFiler(expanded: Bool, unavailableReason: String?) {
        filerItem.isHighlighted = expanded && unavailableReason == nil
        filerItem.isEnabled = unavailableReason == nil
        filerItem.toolTip = unavailableReason ?? String(localized: "Show or hide the file browser (⌘⌥→). Right-click the terminal and choose “Show Working Directory in File Browser” to locate its folder.")
    }

    var isFilerHighlighted: Bool { filerItem.isHighlighted }
    var isFilerEnabled: Bool { filerItem.isEnabled }
    func clickFilerItem() { if filerItem.isEnabled { filerItem.onClick?() } }

    /// 设置"时间轴"条目的打开状态（打开时有强调色圆角底）。
    func setTimelineOn(_ isOn: Bool) {
        timelineItem.isHighlighted = isOn
    }

    /// 设置当前目录条目：传 nil 隐藏。本 WP 只显示，不可点击。
    func setDirectory(_ text: String?) {
        directoryItem.isHidden = text == nil
        directoryItem.title = text ?? ""
        directoryItem.toolTip = text
        needsLayout = true
    }

    var isTimelineOn: Bool { timelineItem.isHighlighted }
    var isDirectoryVisible: Bool { !directoryItem.isHidden }
    var directoryText: String { directoryItem.title }
    /// 测试用：模拟点击"时间轴"条目。
    func clickTimelineItem() { timelineItem.onClick?() }

    /// 设置 Legacy SSH 标记：传 nil 隐藏；否则显示并把 `toolTip` 作为悬停文字（每台服务器一行）。
    func setLegacySshToolTip(_ toolTip: String?) {
        legacyBadge.isHidden = toolTip == nil
        legacyBadge.setToolTip(toolTip)
        needsLayout = true
    }

    var isLegacySshVisible: Bool { !legacyBadge.isHidden }
    var legacySshToolTip: String? { legacyBadge.toolTip }

    var statusText: String { statusLabel.stringValue }
    var clockText: String { clockLabel.stringValue }

    override func layout() {
        super.layout()
        let pad = DesignTokens.Spacing.md
        let gap = Self.itemSpacing
        clockLabel.sizeToFit()
        let clockW = clockLabel.frame.width
        let h = clockLabel.frame.height
        let y = (bounds.height - h) / 2
        clockLabel.frame = NSRect(x: bounds.width - pad - clockW, y: y, width: clockW, height: h)
        var rightEdge = clockLabel.frame.minX
        if !mcpItem.isHidden {
            let itemH = mcpItem.fittingHeight
            let width = mcpItem.fittingWidth
            mcpItem.frame = NSRect(
                x: rightEdge - pad - width, y: (bounds.height - itemH) / 2, width: width, height: itemH
            )
            rightEdge = mcpItem.frame.minX
        }
        if !legacyBadge.isHidden {
            let size = legacyBadge.badgeSize
            legacyBadge.frame = NSRect(
                x: rightEdge - pad - size.width, y: (bounds.height - size.height) / 2,
                width: size.width, height: size.height
            )
            rightEdge = legacyBadge.frame.minX
        }
        // 左侧可用区间 [pad, rightEdge - gap]；与右侧元素之间至少留一个条目间距。
        let leftLimit = rightEdge - gap
        var x = pad
        let itemH = timelineItem.fittingHeight
        let itemY = (bounds.height - itemH) / 2
        if !senderItem.isHidden {
            senderItem.frame = NSRect(x: x, y: itemY, width: min(senderItem.fittingWidth, max(0, leftLimit - x)), height: itemH)
            x = senderItem.frame.maxX + gap
        }
        // 极窄时条目也要让位给右侧的时钟 / Legacy 标记，不能重叠。
        filerItem.frame = NSRect(x: x, y: itemY, width: min(filerItem.fittingWidth, max(0, leftLimit - x)), height: itemH)
        x = filerItem.frame.maxX + gap
        let timelineW = timelineItem.fittingWidth
        timelineItem.frame = NSRect(x: x, y: itemY, width: min(timelineW, max(0, leftLimit - x)), height: itemH)
        x = timelineItem.frame.maxX + gap
        if !directoryItem.isHidden {
            let w = min(directoryItem.fittingWidth, max(0, leftLimit - x))
            directoryItem.frame = NSRect(x: x, y: itemY, width: w, height: itemH)
            x = directoryItem.frame.maxX + gap
        }
        // 状态文字拿剩余空间，不够时宽度为 0（等价于被截断到消失），绝不与其他元素重叠。
        statusLabel.frame = NSRect(x: x, y: y, width: max(0, leftLimit - x), height: h)
    }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgStatusBar))
    }

    override func draw(_ dirtyRect: NSRect) {
        // 顶部 1px 分隔线。
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).fill()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: - 时钟

    private func updateClock() {
        clockLabel.stringValue = Self.clockString(from: Date())
        needsLayout = true
    }

    /// 在下一个整分钟触发，之后每分钟一次（每次重新对齐，避免 Timer 漂移累积）。
    private func scheduleClock() {
        clockTimer?.invalidate()
        let calendar = Calendar.current
        let now = Date()
        let next = calendar.nextDate(after: now, matching: DateComponents(second: 0), matchingPolicy: .nextTime)
            ?? now.addingTimeInterval(60)
        let timer = Timer(fire: next, interval: 0, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.updateClock()
                self?.scheduleClock()
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        clockTimer = timer
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil {
            updateClock()
            scheduleClock()
        } else {
            clockTimer?.invalidate()
            clockTimer = nil
        }
    }
}

/// 状态栏左侧的"图标 + 文字"条目。
///
/// 可点击的条目悬停时文字与图标变亮（`textPrimary`），`isHighlighted` 时画强调色低明度的圆角底。
/// 子视图不接收鼠标事件（`hitTest` 返回自身），点击与 tooltip 统一由条目本身处理。
@MainActor
final class StatusBarItemView: NSView {
    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private let isClickable: Bool
    private var isHovering = false { didSet { updateColors() } }

    var onClick: (() -> Void)?

    /// 不可用时置灰且不响应点击。
    var isEnabled = true {
        didSet { alphaValue = isEnabled ? 1 : 0.5 }
    }

    /// 是否画强调色圆角底（"时间轴"打开状态）。
    var isHighlighted = false {
        didSet {
            guard isHighlighted != oldValue else { return }
            needsDisplay = true
            updateColors()
        }
    }

    var title: String {
        get { label.stringValue }
        set {
            label.stringValue = newValue
            superview?.needsLayout = true
        }
    }

    var lineBreakMode: NSLineBreakMode {
        get { label.lineBreakMode }
        set { label.lineBreakMode = newValue }
    }

    private static let hPad: CGFloat = 6
    private static let vPad: CGFloat = 2
    private static let iconSize: CGFloat = 12
    private static let gap: CGFloat = 4

    init(symbol: String, title: String, isClickable: Bool) {
        self.isClickable = isClickable
        super.init(frame: .zero)
        wantsLayer = true
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.stringValue = title
        label.lineBreakMode = .byTruncatingTail
        icon.image = .symbol(symbol)
        icon.symbolConfiguration = .init(pointSize: Self.iconSize, weight: .regular)
        addSubview(icon)
        addSubview(label)
        updateColors()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("StatusBarItemView 只支持代码创建") }

    override var isFlipped: Bool { true }

    /// 自然宽度（不受截断影响）。
    var fittingWidth: CGFloat {
        label.sizeToFit()
        return Self.hPad * 2 + Self.iconSize + Self.gap + label.frame.width
    }

    var fittingHeight: CGFloat {
        label.sizeToFit()
        return label.frame.height + Self.vPad * 2
    }

    override func layout() {
        super.layout()
        let labelH = label.intrinsicContentSize.height
        icon.frame = NSRect(
            x: Self.hPad, y: (bounds.height - Self.iconSize) / 2, width: Self.iconSize, height: Self.iconSize
        )
        let labelX = icon.frame.maxX + Self.gap
        label.frame = NSRect(
            x: labelX, y: (bounds.height - labelH) / 2,
            width: max(0, bounds.width - labelX - Self.hPad), height: labelH
        )
    }

    private func updateColors() {
        let name = isClickable && isHovering ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary
        let color = NSColor.token(name)
        label.textColor = color
        icon.contentTintColor = color
    }

    override func draw(_ dirtyRect: NSRect) {
        guard isHighlighted else { return }
        // 选中态底色用 bg.segmentSelected（强调色的低明度版本，随外观自动切换）。
        NSColor.token(DesignTokens.Color.bgSegmentSelected).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: DesignTokens.Radius.md, yRadius: DesignTokens.Radius.md).fill()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: 鼠标

    /// `point` 在父视图坐标系。
    override func hitTest(_ point: NSPoint) -> NSView? {
        frame.contains(point) ? self : nil
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        guard isClickable else { return }
        addTrackingArea(NSTrackingArea(
            rect: bounds, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self
        ))
    }

    override func mouseEntered(with event: NSEvent) { isHovering = true }
    override func mouseExited(with event: NSEvent) { isHovering = false }

    override func mouseDown(with event: NSEvent) {
        // 点击发生在 mouseUp 且仍在条目内，与系统按钮的"按下后移出可取消"一致。
        guard isClickable else { return super.mouseDown(with: event) }
    }

    override func mouseUp(with event: NSEvent) {
        guard isClickable, isEnabled, bounds.contains(convert(event.locationInWindow, from: nil)) else { return }
        onClick?()
    }
}

/// Legacy SSH 标记：图标 + 文字的小圆角描边胶囊，颜色取 `status_legacySsh`。克制样式，不抢状态栏的视觉。
@MainActor
final class LegacySshBadge: NSView {
    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: String(localized: "Legacy SSH"))

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.borderWidth = 1
        layer?.cornerRadius = 8
        let color = NSColor.token(DesignTokens.Color.statusLegacySsh)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.textColor = color
        icon.image = .symbol("exclamationmark.shield")
        icon.contentTintColor = color
        addSubview(icon)
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("LegacySshBadge 只支持代码创建") }

    private static let hPad: CGFloat = 6
    private static let vPad: CGFloat = 1
    private static let iconSize: CGFloat = 11
    private static let gap: CGFloat = 3

    var badgeSize: NSSize {
        label.sizeToFit()
        return NSSize(
            width: Self.hPad * 2 + Self.iconSize + Self.gap + label.frame.width,
            height: label.frame.height + Self.vPad * 2
        )
    }

    /// 悬停文字同时设给子视图，避免子视图遮住外层的 tooltip 区域。
    func setToolTip(_ text: String?) {
        toolTip = text
        label.toolTip = text
        icon.toolTip = text
    }

    override func layout() {
        super.layout()
        label.sizeToFit()
        icon.frame = NSRect(x: Self.hPad, y: (bounds.height - Self.iconSize) / 2, width: Self.iconSize, height: Self.iconSize)
        label.frame.origin = NSPoint(x: icon.frame.maxX + Self.gap, y: (bounds.height - label.frame.height) / 2)
    }

    override func updateLayer() {
        layer?.borderColor = resolvedCGColor(.token(DesignTokens.Color.statusLegacySsh))
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}
