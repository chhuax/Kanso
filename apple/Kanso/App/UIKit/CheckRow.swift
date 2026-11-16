import AppKit

/// 复选行（visual-style §3）：自绘复选框 + 文字，整行可点。
///
/// `KCheckbox` 只负责显示、不接收鼠标，点击由所在的行处理，这个类就是那一行。
/// 程序设置 `isChecked` 不触发 `onToggle`；用户点击（或键盘空格、辅助功能按下）才触发。
@MainActor
final class KCheckRow: NSStackView {
    let checkbox = KCheckbox()
    let label: NSTextField

    /// 用户切换后回调（`isChecked` 已更新）。
    var onToggle: ((Bool) -> Void)?

    var isChecked: Bool {
        get { checkbox.isChecked }
        set {
            checkbox.isChecked = newValue
            setAccessibilityValue(newValue)
        }
    }

    var isEnabled = true {
        didSet { alphaValue = isEnabled ? 1 : 0.5 }
    }

    init(title: String) {
        label = PassthroughLabel(labelWithString: title)
        super.init(frame: .zero)
        orientation = .horizontal
        alignment = .centerY
        spacing = DesignTokens.Spacing.sm
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.textColor = .token(DesignTokens.Color.textPrimary)
        addArrangedSubview(checkbox)
        addArrangedSubview(label)
        setAccessibilityRole(.checkBox)
        setAccessibilityLabel(title)
        setAccessibilityValue(false)
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("KCheckRow 只支持代码创建") }

    override func mouseDown(with event: NSEvent) { toggle() }

    /// 用户切换一次。internal 供测试直接调用。
    func toggle() {
        guard isEnabled else { return }
        isChecked.toggle()
        onToggle?(isChecked)
    }

    override func accessibilityPerformPress() -> Bool {
        toggle()
        return isEnabled
    }
}
