import AppKit

/// 查找栏的视图：只负责控件与外观，不含任何搜索逻辑（逻辑在 `FindBarController`）。
///
/// 外观取自设计 token：背景 `bg_popover`、圆角 `Radius.lg`、1px `border_strong` 描边、轻阴影；
/// 输入框与按钮用组件库（`KTextField`、`KButton`），与其它自绘界面同一套样式。
@MainActor
final class FindBarView: NSView {
    typealias Spacing = DesignTokens.Spacing

    /// 输入框容器；`searchField` 是它内部的文本框，控制器的 delegate / 取值 / 焦点都针对它。
    let searchBox = KTextField()
    var searchField: NSTextField { searchBox.textField }
    let caseSensitiveButton = KButton(title: "Aa", style: .icon)
    let regexButton = KButton(title: ".*", style: .icon)
    let statusLabel = NSTextField(labelWithString: "")
    let previousButton = KButton(symbol: "chevron.up", style: .icon)
    let nextButton = KButton(symbol: "chevron.down", style: .icon)
    let closeButton = KButton(symbol: "xmark", style: .icon)

    /// 浮层固定宽度。
    static let barWidth: CGFloat = 360

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setUp()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) 不支持") }

    private func setUp() {
        wantsLayer = true
        // 阴影画在图层外侧，所以不能裁剪子图层。
        layer?.masksToBounds = false
        layer?.cornerRadius = DesignTokens.Radius.lg
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.18
        layer?.shadowRadius = 6
        layer?.shadowOffset = CGSize(width: 0, height: -2)
        setAccessibilityRole(.group)

        searchBox.placeholder = String(localized: "Find")
        searchField.setAccessibilityLabel(String(localized: "Find"))
        searchField.usesSingleLineMode = true
        searchBox.setContentHuggingPriority(.defaultLow, for: .horizontal)
        searchBox.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        configureToggle(caseSensitiveButton, toolTip: String(localized: "Match Case"))
        configureToggle(regexButton, toolTip: String(localized: "Regular Expression"))

        statusLabel.font = .monospacedDigitSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
        statusLabel.alignment = .right
        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.setContentHuggingPriority(.required, for: .horizontal)
        statusLabel.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)

        configureIcon(previousButton, label: String(localized: "Find Previous"))
        configureIcon(nextButton, label: String(localized: "Find Next"))
        configureIcon(closeButton, label: String(localized: "Close Find Bar"))

        let stack = NSStackView(views: [
            searchBox, caseSensitiveButton, regexButton, statusLabel, previousButton, nextButton, closeButton,
        ])
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = Spacing.xs
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Spacing.sm),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Spacing.sm),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: Spacing.sm),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -Spacing.sm),
            // 状态文字（"–/1234"、"无效的正则表达式"）需要稳定的最小宽度，避免数字变化时搜索框抖动。
            statusLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 56),
            // 查找栏比对话框里的输入框矮一点，保持浮层紧凑。
            searchBox.heightAnchor.constraint(equalToConstant: 28),
        ])
        updateColors()
    }

    private func configureToggle(_ button: KButton, toolTip: String) {
        button.isToggle = true
        button.toolTip = toolTip
        button.setAccessibilityLabel(toolTip)
    }

    private func configureIcon(_ button: KButton, label: String) {
        button.toolTip = label
        button.setAccessibilityLabel(label)
    }

    /// 更新状态文字。`isError` 为真时用 `status_error` 颜色（非法正则），否则用次要文字色。
    func setStatus(_ text: String, isError: Bool) {
        statusLabel.stringValue = text
        statusLabel.textColor = .token(isError ? DesignTokens.Color.statusError : DesignTokens.Color.textSecondary)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateColors()
    }

    /// 图层颜色是 CGColor，不会随外观自动更新；必须在当前外观下解析一次，切换外观时重做。
    private func updateColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let background = NSColor(named: DesignTokens.Color.bgPopover) ?? .windowBackgroundColor
            let border = NSColor(named: DesignTokens.Color.borderStrong) ?? .separatorColor
            layer?.backgroundColor = background.cgColor
            layer?.borderColor = border.cgColor
            layer?.shadowColor = NSColor.black.cgColor
        }
    }
}
