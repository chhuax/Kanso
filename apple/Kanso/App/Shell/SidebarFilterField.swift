import AppKit

/// 标签栏顶部的路径筛选框：小圆角、细边框的平直输入框（与 `KTextField` 同一套 token：
/// `bg.field` 底、`border.field` 边框、焦点时强调色边框）。
///
/// 不用 `NSSearchField`：新版 macOS 把它画成大圆角胶囊，和终端界面的直角、细线风格不协调，
/// 而它的形状无法通过公开 API 调整。这里自己组合放大镜图标、无边框文本框和清除按钮。
///
/// 调用方通过 `textField` 设置 delegate / 读写文字；清除按钮会清空文字并调用 `onClear`
/// （程序修改 `stringValue` 不会触发 `controlTextDidChange`，所以清除要单独通知）。
@MainActor
final class SidebarFilterField: NSView {
    let textField = FocusTextField()
    /// 点击清除按钮后调用（文字已清空）。
    var onClear: (() -> Void)?

    private let icon = NSImageView()
    private let clearButton = NSButton()
    private var isEditing = false

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = DesignTokens.Radius.md
        layer?.borderWidth = 1

        icon.image = .symbol("magnifyingglass")
        icon.symbolConfiguration = .init(pointSize: DesignTokens.Font.uiCaption, weight: .regular)
        icon.contentTintColor = .token(DesignTokens.Color.textTertiary)

        textField.isBezeled = false
        textField.isBordered = false
        textField.drawsBackground = false
        textField.focusRingType = .none
        textField.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        textField.textColor = .token(DesignTokens.Color.textPrimary)
        textField.cell?.isScrollable = true
        textField.cell?.wraps = false
        textField.cell?.lineBreakMode = .byClipping

        clearButton.isBordered = false
        clearButton.image = .symbol("xmark.circle.fill", description: String(localized: "Clear"))
        clearButton.symbolConfiguration = .init(pointSize: DesignTokens.Font.uiCaption, weight: .regular)
        clearButton.imagePosition = .imageOnly
        clearButton.contentTintColor = .token(DesignTokens.Color.textTertiary)
        clearButton.toolTip = String(localized: "Clear")
        clearButton.target = self
        clearButton.action = #selector(clearClicked)
        clearButton.isHidden = true

        for v in [icon, textField, clearButton] { addSubview(v) }

        // 焦点进出只用于切换边框颜色，不占用 delegate（delegate 归调用方）。
        // 用 FocusTextField 的焦点回调而不是 textDidBeginEditing：后者要等用户输入第一个字符才发出。
        textField.onFocusChange = { [weak self] focused in
            self?.isEditing = focused
            self?.needsDisplay = true
        }
        NotificationCenter.default.addObserver(self, selector: #selector(textChanged), name: NSControl.textDidChangeNotification, object: textField)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SidebarFilterField 只支持代码创建") }

    /// 占位文字固定用 `text.tertiary`，与 `KTextField` 一致；字体取设置占位文字时的文本框字体。
    var placeholder: String {
        get { textField.placeholderAttributedString?.string ?? textField.placeholderString ?? "" }
        set {
            textField.placeholderAttributedString = NSAttributedString(string: newValue, attributes: [
                .foregroundColor: NSColor.token(DesignTokens.Color.textTertiary),
                .font: textField.font ?? NSFont.systemFont(ofSize: DesignTokens.Font.uiCaption),
            ])
        }
    }

    var stringValue: String {
        get { textField.stringValue }
        set {
            textField.stringValue = newValue
            syncClearButton()
        }
    }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgField))
        let border = isEditing ? DesignTokens.Color.accent : DesignTokens.Color.borderField
        layer?.borderColor = resolvedCGColor(.token(border))
    }

    override func layout() {
        super.layout()
        let pad = DesignTokens.Spacing.xs + DesignTokens.Spacing.xxs
        let iconSize: CGFloat = 14
        icon.frame = NSRect(x: pad, y: (bounds.height - iconSize) / 2, width: iconSize, height: iconSize)
        let clearSize: CGFloat = 16
        clearButton.frame = NSRect(x: bounds.width - pad - clearSize, y: (bounds.height - clearSize) / 2,
                                   width: clearSize, height: clearSize)
        // 单行文本框按字体行高垂直居中；右侧始终给清除按钮留位，出现 / 消失时文字不跳动。
        let textHeight = ceil(textField.intrinsicContentSize.height)
        let textX = icon.frame.maxX + DesignTokens.Spacing.xs
        textField.frame = NSRect(x: textX, y: (bounds.height - textHeight) / 2,
                                 width: max(0, clearButton.frame.minX - DesignTokens.Spacing.xxs - textX), height: textHeight)
    }

    /// 点击框内空白处（图标、内边距）也进入编辑。
    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(textField)
    }

    @objc private func textChanged() { syncClearButton() }

    @objc private func clearClicked() {
        stringValue = ""
        onClear?()
    }

    private func syncClearButton() {
        clearButton.isHidden = textField.stringValue.isEmpty
    }
}
