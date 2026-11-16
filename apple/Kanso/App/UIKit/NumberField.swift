import AppKit

/// 数字输入（visual-style §3.1 的输入框 + 两个纯图标的 −/+ 按钮）：用于字号、回滚行数、对比度比例。
///
/// 值始终夹在 `range` 内：输入越界的数字在提交时被夹回边界（而不是拒绝），输入无法解析的文字在提交时还原为当前值。
/// 提交时机：回车、失去焦点、点 −/+。程序设置 `value` 不触发 `onChange`；用户提交且值真正变化才触发。
@MainActor
final class KNumberField: NSStackView, NSTextFieldDelegate {
    let field = KTextField()
    let minusButton = KButton(symbol: "minus", style: .icon)
    let plusButton = KButton(symbol: "plus", style: .icon)

    let range: ClosedRange<Double>
    let step: Double
    let fractionDigits: Int

    /// 用户提交了新值。
    var onChange: ((Double) -> Void)?

    private(set) var value: Double

    var isEnabled = true {
        didSet {
            field.isEnabled = isEnabled
            minusButton.isEnabled = isEnabled
            plusButton.isEnabled = isEnabled
        }
    }

    /// - Parameters:
    ///   - range: 允许的范围（含两端）。
    ///   - step: −/+ 的步进。
    ///   - fractionDigits: 显示的小数位数（0 表示整数）。
    init(range: ClosedRange<Double>, step: Double, fractionDigits: Int = 0, fieldWidth: CGFloat = 92) {
        self.range = range
        self.step = step
        self.fractionDigits = fractionDigits
        value = range.lowerBound
        super.init(frame: .zero)
        orientation = .horizontal
        alignment = .centerY
        spacing = DesignTokens.Spacing.xs

        field.textField.delegate = self
        field.textField.alignment = .right
        // 回车提交；Tab / 点击别处由 controlTextDidEndEditing 提交。
        field.textField.target = self
        field.textField.action = #selector(fieldCommitted(_:))
        field.translatesAutoresizingMaskIntoConstraints = false
        field.widthAnchor.constraint(equalToConstant: fieldWidth).isActive = true
        minusButton.onPress = { [weak self] in self?.nudge(-1) }
        plusButton.onPress = { [weak self] in self?.nudge(1) }
        minusButton.setAccessibilityLabel(String(localized: "Decrease"))
        plusButton.setAccessibilityLabel(String(localized: "Increase"))
        addArrangedSubview(field)
        addArrangedSubview(minusButton)
        addArrangedSubview(plusButton)
        setValue(range.lowerBound)
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("KNumberField 只支持代码创建") }

    /// 程序设置（同步外部状态）：夹进范围、刷新文字，不回调。
    func setValue(_ newValue: Double) {
        value = min(max(newValue, range.lowerBound), range.upperBound)
        field.stringValue = format(value)
        updateButtons()
    }

    /// 提交输入框里的文字。internal 供测试驱动。
    func commitText() {
        let text = field.stringValue.trimmingCharacters(in: .whitespaces)
        guard let parsed = Double(text), parsed.isFinite else {
            // 无法解析：还原为当前值，不改变设置。
            field.stringValue = format(value)
            return
        }
        commit(parsed)
    }

    /// 点 −/+（方向 -1 / +1）。internal 供测试驱动。
    func nudge(_ direction: Double) {
        commit(value + direction * step)
    }

    private func commit(_ proposed: Double) {
        let old = value
        setValue(proposed)
        if value != old { onChange?(value) }
    }

    private func updateButtons() {
        minusButton.isEnabled = isEnabled && value > range.lowerBound
        plusButton.isEnabled = isEnabled && value < range.upperBound
    }

    private func format(_ v: Double) -> String {
        String(format: "%.\(fractionDigits)f", v)
    }

    @objc private func fieldCommitted(_ sender: Any?) { commitText() }

    func controlTextDidEndEditing(_ obj: Notification) { commitText() }
}
