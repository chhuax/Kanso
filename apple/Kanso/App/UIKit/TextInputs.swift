import AppKit

/// 能把"获得 / 失去键盘焦点"上报给外层自绘容器的文本框。
///
/// 为什么不用 `textDidBeginEditing` 通知：它在用户第一次修改文字时才发出，点进输入框但还没输入时
/// 边框不会变色。`becomeFirstResponder` 是焦点进入的第一时间；失去焦点则只有 `textDidEndEditing`
/// 可靠（焦点交给字段编辑器后，文本框本身的 `resignFirstResponder` 不能当作"离开"）。
@MainActor
final class FocusTextField: NSTextField {
    var onFocusChange: ((Bool) -> Void)?

    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted {
            applyCaretColor(of: self)
            onFocusChange?(true)
        }
        return accepted
    }

    override func textDidEndEditing(_ notification: Notification) {
        super.textDidEndEditing(notification)
        onFocusChange?(false)
    }
}

/// 密码框：必须是 `NSSecureTextField`——不回显由系统保证并启用 secure event input，不能自己遮字。
/// 焦点上报与 `FocusTextField` 相同，因两者没有共同的可用父类，逻辑重复一份。
@MainActor
final class FocusSecureTextField: NSSecureTextField {
    var onFocusChange: ((Bool) -> Void)?

    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted {
            applyCaretColor(of: self)
            onFocusChange?(true)
        }
        return accepted
    }

    override func textDidEndEditing(_ notification: Notification) {
        super.textDidEndEditing(notification)
        onFocusChange?(false)
    }
}

/// 深色底上系统默认的插入点颜色不够醒目，改成强调色（截图里是青色光标）。
/// 字段编辑器是窗口共享的，每次获得焦点都要重设。
@MainActor
private func applyCaretColor(of field: NSTextField) {
    (field.currentEditor() as? NSTextView)?.insertionPointColor = .token(DesignTokens.Color.accent)
}

/// 输入框（visual-style §3.1）：实色底、1px 边框、6pt 圆角、左右内边距 10pt、高 30pt；
/// 获得焦点时边框换成强调色。
///
/// 不用系统 bezel（新版 macOS 的圆角胶囊与光晕），由容器画底和边框，内部是去掉 bezel 的文本框。
/// 调用方通过 `textField` 设置 delegate / 读写文字（delegate 归调用方，焦点上报不占用它）。
@MainActor
final class KTextField: StyledBoxView {
    let textField: NSTextField

    /// 获得 / 失去焦点时回调（边框颜色已更新）。
    var onFocusChange: ((Bool) -> Void)?

    private(set) var isFocused = false

    override init(frame frameRect: NSRect) {
        textField = FocusTextField()
        super.init(frame: frameRect)
        setUp()
    }

    /// `secure` 为真时内部用 `NSSecureTextField`（密码框，不回显）。
    init(secure: Bool) {
        textField = secure ? FocusSecureTextField() : FocusTextField()
        super.init(frame: .zero)
        setUp()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KTextField 只支持代码创建") }

    private func setUp() {
        fillToken = DesignTokens.Color.bgField
        borderToken = DesignTokens.Color.borderField
        cornerRadius = DesignTokens.Radius.md

        textField.isBezeled = false
        textField.isBordered = false
        textField.drawsBackground = false
        textField.focusRingType = .none
        textField.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        textField.textColor = .token(DesignTokens.Color.textPrimary)
        textField.cell?.isScrollable = true
        textField.cell?.wraps = false
        textField.cell?.lineBreakMode = .byClipping
        addSubview(textField)

        if let field = textField as? FocusTextField {
            field.onFocusChange = { [weak self] in self?.setFocused($0) }
        } else if let field = textField as? FocusSecureTextField {
            field.onFocusChange = { [weak self] in self?.setFocused($0) }
        }
    }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: DesignTokens.Spacing.fieldHeight)
    }

    var stringValue: String {
        get { textField.stringValue }
        set { textField.stringValue = newValue }
    }

    /// 占位文字：用示例值（`example.com`），颜色固定为 `text.tertiary`，不用系统占位色。
    var placeholder: String {
        get { textField.placeholderAttributedString?.string ?? textField.placeholderString ?? "" }
        set {
            textField.placeholderAttributedString = NSAttributedString(string: newValue, attributes: [
                .foregroundColor: NSColor.token(DesignTokens.Color.textTertiary),
                .font: NSFont.systemFont(ofSize: DesignTokens.Font.uiBody),
            ])
        }
    }

    var isEnabled: Bool {
        get { textField.isEnabled }
        set {
            textField.isEnabled = newValue
            alphaValue = newValue ? 1 : 0.5
        }
    }

    /// 焦点状态切换边框色。internal 是为了测试可以不经过真实的响应链直接驱动。
    func setFocused(_ focused: Bool) {
        isFocused = focused
        borderToken = focused ? DesignTokens.Color.accent : DesignTokens.Color.borderField
        onFocusChange?(focused)
    }

    override func layout() {
        super.layout()
        let pad = DesignTokens.Spacing.fieldPaddingX
        let textHeight = ceil(textField.intrinsicContentSize.height)
        textField.frame = NSRect(x: pad, y: (bounds.height - textHeight) / 2,
                                 width: max(0, bounds.width - pad * 2), height: textHeight)
    }

    /// 点击框内空白处（内边距）也进入编辑。
    override func mouseDown(with event: NSEvent) {
        guard textField.isEnabled else { return }
        window?.makeFirstResponder(textField)
    }
}

/// 带上方标签的字段：标签（12pt，次要文字色）在控件上方，visual-style §3.1。
@MainActor
final class LabeledField: NSStackView {
    let titleLabel: NSTextField
    let control: NSView

    init(title: String, control: NSView) {
        titleLabel = NSTextField(labelWithString: title)
        self.control = control
        super.init(frame: .zero)
        orientation = .vertical
        alignment = .leading
        spacing = 6
        titleLabel.font = .systemFont(ofSize: 12)
        titleLabel.textColor = .token(DesignTokens.Color.textSecondary)
        addArrangedSubview(titleLabel)
        addArrangedSubview(control)
        // 控件撑满字段宽度；NSStackView 的 .leading 对齐默认只给控件固有宽度。
        control.translatesAutoresizingMaskIntoConstraints = false
        control.leadingAnchor.constraint(equalTo: leadingAnchor).isActive = true
        control.trailingAnchor.constraint(equalTo: trailingAnchor).isActive = true
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("LabeledField 只支持代码创建") }
}
