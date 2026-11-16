import AppKit

/// 按钮样式（visual-style §3.2）。
enum KButtonStyle {
    /// 深灰底 + 细边框，主文字色。
    case secondary
    /// 强调色底，`text.onAccent` 文字，可带图标。
    case primary
    /// 无边框无底色的纯图标按钮（铅笔、`…`、× 、齿轮）：次要文字色，悬停变亮。
    case icon
}

/// 自绘按钮。
///
/// 不用 `NSButton`：它的 bezel / 底色由系统决定，且重写 `updateLayer` 会让单元格不再绘制标题。
/// 因此动作接口模仿 NSButton 的子集：`target` / `action`、`state`（开关型）、`performClick(_:)`，
/// 另有更方便的 `onPress` 闭包；两者都设置时都会触发。
///
/// `keyEquivalent`（"\r" / "\u{1b}"）由 `performKeyEquivalent` 实现，只在按钮可见且可用时响应，
/// 因此对话框的默认按钮要靠"哪个按钮设置了 `\r`"决定。
@MainActor
final class KButton: StyledBoxView {
    let style: KButtonStyle

    var onPress: (() -> Void)?
    var contextMenuProvider: (() -> NSMenu)?
    weak var target: AnyObject?
    var action: Selector?
    var keyEquivalent = ""

    /// 开关型按钮（查找栏的"区分大小写"）：每次点击翻转 `state`，选中时用强调色边框。
    var isToggle = false
    var state: NSControl.StateValue = .off {
        didSet { refreshStyle() }
    }

    var isEnabled = true {
        didSet { refreshStyle() }
    }

    var title: String {
        didSet { label.stringValue = title; invalidateIntrinsicContentSize(); needsLayout = true }
    }

    var symbol: String? {
        didSet { iconView.image = symbol.map { .symbol($0) }; invalidateIntrinsicContentSize(); needsLayout = true }
    }

    private let label = PassthroughLabel(labelWithString: "")
    private let iconView = NSImageView()
    private var isHovered = false { didSet { refreshStyle() } }
    private var isPressed = false { didSet { refreshStyle() } }

    init(title: String = "", symbol: String? = nil, style: KButtonStyle = .secondary) {
        self.title = title
        self.symbol = symbol
        self.style = style
        super.init(frame: .zero)
        label.stringValue = title
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody, weight: style == .primary ? .medium : .regular)
        label.lineBreakMode = .byClipping
        iconView.image = symbol.map { .symbol($0) }
        iconView.symbolConfiguration = .init(pointSize: style == .icon ? 13 : 12, weight: .regular)
        iconView.imageScaling = .scaleProportionallyDown
        addSubview(iconView)
        addSubview(label)
        cornerRadius = DesignTokens.Radius.md
        setAccessibilityRole(.button)
        refreshStyle()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KButton 只支持代码创建") }

    // MARK: - 尺寸与布局

    private static let horizontalPadding: CGFloat = 14
    private static let iconGap: CGFloat = 6

    override var intrinsicContentSize: NSSize {
        if style == .icon {
            // 纯图标 26x26；带文字的开关（"Aa"、".*"）按文字加宽。
            return NSSize(width: symbol == nil ? max(26, ceil(label.intrinsicContentSize.width) + 12) : 26, height: 26)
        }
        var width = Self.horizontalPadding * 2 + ceil(label.intrinsicContentSize.width)
        if symbol != nil { width += 14 + Self.iconGap }
        return NSSize(width: max(width, 64), height: DesignTokens.Spacing.fieldHeight)
    }

    override func layout() {
        super.layout()
        let iconWidth: CGFloat = symbol == nil ? 0 : 14
        iconView.isHidden = symbol == nil
        if style == .icon {
            if symbol != nil {
                iconView.frame = bounds
                label.frame = .zero
            } else {
                let h = ceil(label.intrinsicContentSize.height)
                label.frame = NSRect(x: 0, y: (bounds.height - h) / 2, width: bounds.width, height: h)
                label.alignment = .center
            }
            return
        }
        let textWidth = ceil(label.intrinsicContentSize.width)
        let textHeight = ceil(label.intrinsicContentSize.height)
        let gap = symbol == nil || title.isEmpty ? 0 : Self.iconGap
        let total = iconWidth + gap + textWidth
        var x = ((bounds.width - total) / 2).rounded()
        if symbol != nil {
            iconView.frame = NSRect(x: x, y: (bounds.height - 14) / 2, width: iconWidth, height: 14)
            x += iconWidth + gap
        }
        label.frame = NSRect(x: x, y: (bounds.height - textHeight) / 2, width: textWidth, height: textHeight)
    }

    // MARK: - 外观

    private func refreshStyle() {
        let active = state == .on
        switch style {
        case .secondary:
            fillToken = DesignTokens.Color.bgButtonSecondary
            borderToken = (active || isHovered) ? DesignTokens.Color.accent : DesignTokens.Color.borderStrong
            let text = NSColor.token(DesignTokens.Color.textPrimary)
            label.textColor = text
            iconView.contentTintColor = text
        case .primary:
            fillToken = DesignTokens.Color.accent
            borderToken = nil
            let text = NSColor.token(DesignTokens.Color.textOnAccent)
            label.textColor = text
            iconView.contentTintColor = text
        case .icon:
            fillToken = (active || isHovered) ? DesignTokens.Color.bgRowHover : nil
            // 开关型图标按钮选中时文字 / 图标用强调色，与"强调色只用于选中项"一致。
            borderToken = nil
            let tintToken = active ? DesignTokens.Color.accent
                : (isHovered ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary)
            let tint = NSColor.token(tintToken)
            label.textColor = tint
            iconView.contentTintColor = tint
        }
        alphaValue = !isEnabled ? 0.4 : (isPressed ? 0.75 : (style == .primary && isHovered ? 0.9 : 1))
    }

    // MARK: - 交互

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(
            rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self, userInfo: nil
        ))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        isPressed = true
    }

    override func menu(for event: NSEvent) -> NSMenu? { contextMenuProvider?() ?? super.menu(for: event) }

    override func mouseUp(with event: NSEvent) {
        guard isPressed else { return }
        isPressed = false
        // 在按钮外松开视为取消，和系统按钮一致。
        if bounds.contains(convert(event.locationInWindow, from: nil)) { performClick(nil) }
    }

    /// 触发一次点击（开关型先翻转状态）。可用性检查在这里做，测试与键盘等价键共用。
    func performClick(_ sender: Any?) {
        guard isEnabled else { return }
        if isToggle { state = state == .on ? .off : .on }
        onPress?()
        if let action { NSApp.sendAction(action, to: target, from: self) }
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard !keyEquivalent.isEmpty, isEnabled, !isHiddenOrHasHiddenAncestor,
              event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.numericPad, .function]).isEmpty,
              event.charactersIgnoringModifiers == keyEquivalent
        else { return super.performKeyEquivalent(with: event) }
        performClick(nil)
        return true
    }

    override func accessibilityPerformPress() -> Bool {
        performClick(nil)
        return isEnabled
    }

    override func accessibilityLabel() -> String? { super.accessibilityLabel() ?? toolTip ?? (title.isEmpty ? nil : title) }
}
