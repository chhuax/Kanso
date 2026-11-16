import AppKit

/// 自绘复选框（visual-style §1：不用系统 bezel 控件）：16pt 圆角方块，选中时强调色底 + 对勾。
///
/// 只负责显示，不接收鼠标：点击由所在的行处理（整行可点），所以 `hitTest` 返回 nil。
@MainActor
final class KCheckbox: StyledBoxView {
    private let checkmark = NSImageView()

    var isChecked = false {
        didSet { refresh() }
    }

    static let size: CGFloat = 16

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        cornerRadius = 4
        checkmark.image = .symbol("checkmark")
        checkmark.symbolConfiguration = .init(pointSize: 9, weight: .bold)
        checkmark.contentTintColor = .token(DesignTokens.Color.textOnAccent)
        addSubview(checkmark)
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KCheckbox 只支持代码创建") }

    override var intrinsicContentSize: NSSize { NSSize(width: Self.size, height: Self.size) }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        checkmark.frame = bounds
    }

    private func refresh() {
        fillToken = isChecked ? DesignTokens.Color.accent : DesignTokens.Color.bgField
        borderToken = isChecked ? DesignTokens.Color.accent : DesignTokens.Color.borderStrong
        checkmark.isHidden = !isChecked
    }
}
