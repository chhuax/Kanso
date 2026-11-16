import AppKit

/// 对话框外壳（visual-style §3.3）：实色底、圆角 12、1px 边框；
/// 头部（图标徽章 + 标题 + 关闭 ×）、分隔线、可滚动主体、分隔线、底部（左侧提示文字 + 右对齐按钮组）。
///
/// 外壳自己不决定高度：主体内容超过 `maxHeight` 时主体变成滚动区，所以呈现方在内容变化后
/// 必须调 `fittingHeight(maxHeight:)` 取总高度并设置窗口尺寸。宽度固定。
@MainActor
final class DialogShellView: StyledBoxView {
    let titleLabel: NSTextField
    let closeButton = KButton(symbol: "xmark", style: .icon)
    /// 主体内容栈（垂直）：往里加卡片。
    let bodyStack = NSStackView()
    /// 头部、分隔线、主体、底部的纵向容器；量高度时以它为准（见 `fittingHeight`）。
    private let root = NSStackView()
    /// 底部左侧的提示 / 错误文字，空串时隐藏。
    let messageLabel = NSTextField(wrappingLabelWithString: "")

    var onClose: (() -> Void)? {
        didSet { closeButton.onPress = onClose }
    }

    private let badge = StyledBoxView()
    private let badgeIcon = NSImageView()
    private let scrollView = NSScrollView()
    private let documentView = FlippedView()
    private let footer = NSStackView()
    private var bodyHeight: NSLayoutConstraint!
    private let width: CGFloat

    static let bodyInset: CGFloat = 20

    init(title: String, symbol: String, width: CGFloat) {
        self.width = width
        titleLabel = NSTextField(labelWithString: title)
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgDialog
        borderToken = DesignTokens.Color.borderSubtle
        cornerRadius = DesignTokens.Radius.xl
        setAccessibilityRole(.group)
        setAccessibilityLabel(title)

        // 头部
        badge.fillToken = DesignTokens.Color.bgSegmentSelected
        badge.borderToken = DesignTokens.Color.borderSegmentSelected
        badge.cornerRadius = 8
        badgeIcon.image = .symbol(symbol)
        badgeIcon.symbolConfiguration = .init(pointSize: 14, weight: .regular)
        badgeIcon.contentTintColor = .token(DesignTokens.Color.accent)
        badgeIcon.translatesAutoresizingMaskIntoConstraints = false
        badge.addSubview(badgeIcon)
        badge.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiTitle, weight: .semibold)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        closeButton.toolTip = String(localized: "Close")
        let spacer = NSView()
        // 比其它视图略低：多出来的宽度先给空白，不拉伸标题和按钮。
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(249), for: .horizontal)
        let header = NSStackView(views: [badge, titleLabel, spacer, closeButton])
        header.orientation = .horizontal
        header.alignment = .centerY
        header.spacing = DesignTokens.Spacing.md
        header.edgeInsets = NSEdgeInsets(top: 14, left: Self.bodyInset, bottom: 14, right: 16)

        // 主体
        bodyStack.orientation = .vertical
        bodyStack.alignment = .leading
        bodyStack.spacing = DesignTokens.Spacing.lg
        bodyStack.translatesAutoresizingMaskIntoConstraints = false
        documentView.addSubview(bodyStack)
        let inset = Self.bodyInset
        NSLayoutConstraint.activate([
            bodyStack.topAnchor.constraint(equalTo: documentView.topAnchor, constant: inset),
            bodyStack.bottomAnchor.constraint(equalTo: documentView.bottomAnchor, constant: -inset),
            bodyStack.leadingAnchor.constraint(equalTo: documentView.leadingAnchor, constant: inset),
            bodyStack.trailingAnchor.constraint(equalTo: documentView.trailingAnchor, constant: -inset),
        ])
        scrollView.documentView = documentView
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.borderType = .noBorder
        documentView.translatesAutoresizingMaskIntoConstraints = false
        documentView.widthAnchor.constraint(equalTo: scrollView.contentView.widthAnchor).isActive = true

        // 底部
        messageLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        messageLabel.textColor = .token(DesignTokens.Color.statusError)
        messageLabel.maximumNumberOfLines = 4
        messageLabel.isSelectable = true
        messageLabel.isHidden = true
        messageLabel.preferredMaxLayoutWidth = width - 280
        messageLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        messageLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        footer.orientation = .horizontal
        footer.alignment = .centerY
        footer.spacing = DesignTokens.Spacing.sm
        footer.edgeInsets = NSEdgeInsets(top: 14, left: Self.bodyInset, bottom: 14, right: Self.bodyInset)
        footer.addArrangedSubview(messageLabel)
        let footerSpacer = NSView()
        footerSpacer.setContentHuggingPriority(NSLayoutConstraint.Priority(249), for: .horizontal)
        footer.addArrangedSubview(footerSpacer)

        let topDivider = DividerView(), bottomDivider = DividerView()
        root.setViews([header, topDivider, scrollView, bottomDivider, footer], in: .leading)
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 0
        root.translatesAutoresizingMaskIntoConstraints = false
        addSubview(root)
        bodyHeight = scrollView.heightAnchor.constraint(equalToConstant: 100)
        NSLayoutConstraint.activate([
            root.topAnchor.constraint(equalTo: topAnchor),
            root.bottomAnchor.constraint(equalTo: bottomAnchor),
            root.leadingAnchor.constraint(equalTo: leadingAnchor),
            root.trailingAnchor.constraint(equalTo: trailingAnchor),
            widthAnchor.constraint(equalToConstant: width),
            badge.widthAnchor.constraint(equalToConstant: 32),
            badge.heightAnchor.constraint(equalToConstant: 32),
            badgeIcon.centerXAnchor.constraint(equalTo: badge.centerXAnchor),
            badgeIcon.centerYAnchor.constraint(equalTo: badge.centerYAnchor),
            header.widthAnchor.constraint(equalTo: root.widthAnchor),
            topDivider.widthAnchor.constraint(equalTo: root.widthAnchor),
            scrollView.widthAnchor.constraint(equalTo: root.widthAnchor),
            bottomDivider.widthAnchor.constraint(equalTo: root.widthAnchor),
            footer.widthAnchor.constraint(equalTo: root.widthAnchor),
            bodyHeight,
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("DialogShellView 只支持代码创建") }

    /// 设置底部按钮（从左到右的显示顺序，整体右对齐）。
    func setFooterButtons(_ buttons: [KButton]) {
        for view in footer.arrangedSubviews where view is KButton { footer.removeArrangedSubview(view); view.removeFromSuperview() }
        for button in buttons { footer.addArrangedSubview(button) }
    }

    /// 提示 / 错误文字（`status_error` 色）；nil 或空串时隐藏。
    func setMessage(_ text: String?) {
        messageLabel.stringValue = text ?? ""
        messageLabel.isHidden = (text ?? "").isEmpty
    }

    /// 按内容计算外壳总高度，并把主体滚动区高度限制在 `maxHeight` 之内。
    /// 先让主体按当前宽度布局再量高度，所以显隐字段后要重新调用。
    func fittingHeight(maxHeight: CGFloat) -> CGFloat {
        bodyHeight.constant = 100
        layoutSubtreeIfNeeded()
        let content = bodyStack.fittingSize.height + Self.bodyInset * 2
        bodyHeight.constant = content
        layoutSubtreeIfNeeded()
        // 量 root 而不是 self：self 作为视图控制器的根视图往往带着 autoresizing 产生的
        // 固定高度约束（上一次的 sheet 高度），它的 fittingSize 会被钉在旧值上。
        let total = root.fittingSize.height
        let overflow = total - maxHeight
        if overflow > 0 {
            // 总高度超限：只压缩主体滚动区，头部和底部保持完整。
            bodyHeight.constant = max(120, content - overflow)
            layoutSubtreeIfNeeded()
        }
        return root.fittingSize.height
    }
}

/// 1px 分隔线（`border.subtle`）。
@MainActor
final class DividerView: StyledBoxView {
    init(token: String = DesignTokens.Color.borderSubtle) {
        super.init(frame: .zero)
        fillToken = token
        cornerRadius = 0
        translatesAutoresizingMaskIntoConstraints = false
        heightAnchor.constraint(equalToConstant: 1).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("DividerView 只支持代码创建") }
}

/// 滚动文档视图用的 flipped 视图：内容从顶部开始，高度不足时不会贴底。
@MainActor
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

/// 呈现对话框用的无边框透明 sheet 窗口：外观完全由内容里的 `DialogShellView` 提供。
///
/// 仍用 sheet（`beginSheet`）而不是自建遮罩：系统保证 sheet 期间主窗口不响应键鼠与菜单快捷键，
/// 提问对话框（主机密钥变更等）依赖这个模态性。无边框 + 透明背景之后，窗口阴影按内容的圆角轮廓绘制。
/// Esc 默认关闭等价于"取消"，由 `onCancel` 提供（普通 `NSWindow` 的 sheet 不会自己响应 Esc）。
@MainActor
final class DialogSheetWindow: NSWindow {
    var onCancel: (() -> Void)?

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    init(shell: NSView) {
        super.init(contentRect: NSRect(origin: .zero, size: shell.fittingSize),
                   styleMask: [.borderless], backing: .buffered, defer: false)
        isReleasedWhenClosed = false
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        contentView = shell
    }

    override func cancelOperation(_ sender: Any?) { onCancel?() }

    /// 内容高度变化后调用；`invalidateShadow` 让阴影跟上新轮廓。
    func resize(toContentSize size: NSSize) {
        setContentSize(size)
        invalidateShadow()
    }
}
