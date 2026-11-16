import AppKit

/// 分区卡片（visual-style §3.3）：实色底、1px 边框、圆角 10，左上角"图标 + 标题"，下面是内容栈。
///
/// 调用方往 `contentStack` 里加字段 / 行；整张卡片隐藏时用 `isHidden`，不要逐个摘子视图。
@MainActor
final class KCard: StyledBoxView {
    let contentStack = NSStackView()
    let titleLabel: NSTextField

    static let padding: CGFloat = 16

    init(title: String, symbol: String) {
        titleLabel = NSTextField(labelWithString: title)
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgCard
        borderToken = DesignTokens.Color.borderCard
        cornerRadius = DesignTokens.Radius.lg

        let icon = NSImageView(image: .symbol(symbol))
        icon.symbolConfiguration = .init(pointSize: 13, weight: .regular)
        icon.contentTintColor = .token(DesignTokens.Color.textSecondary)
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody, weight: .semibold)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        let header = NSStackView(views: [icon, titleLabel])
        header.orientation = .horizontal
        header.spacing = DesignTokens.Spacing.sm

        contentStack.orientation = .vertical
        contentStack.alignment = .leading
        contentStack.spacing = DesignTokens.Spacing.md

        let root = NSStackView(views: [header, contentStack])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = DesignTokens.Spacing.md
        root.translatesAutoresizingMaskIntoConstraints = false
        addSubview(root)
        let pad = Self.padding
        NSLayoutConstraint.activate([
            root.topAnchor.constraint(equalTo: topAnchor, constant: pad),
            root.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -pad),
            root.leadingAnchor.constraint(equalTo: leadingAnchor, constant: pad),
            root.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -pad),
            // 内容栈撑满卡片内宽，里面的行才能按列等分。
            contentStack.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            contentStack.trailingAnchor.constraint(equalTo: root.trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KCard 只支持代码创建") }

    /// 往内容栈追加一行，并让它撑满卡片内宽。
    func addRow(_ view: NSView) {
        contentStack.addArrangedSubview(view)
        view.translatesAutoresizingMaskIntoConstraints = false
        view.leadingAnchor.constraint(equalTo: contentStack.leadingAnchor).isActive = true
        view.trailingAnchor.constraint(equalTo: contentStack.trailingAnchor).isActive = true
    }

    /// 两列并排的一行：两个字段等宽。
    static func pair(_ left: NSView, _ right: NSView) -> NSStackView {
        let row = NSStackView(views: [left, right])
        row.orientation = .horizontal
        row.alignment = .top
        row.distribution = .fillEqually
        row.spacing = DesignTokens.Spacing.md
        return row
    }
}

/// 一个分段卡片：上图标下文字。
@MainActor
private final class SegmentCardView: StyledBoxView {
    var onClick: (() -> Void)?
    private let iconView = NSImageView()
    private let label: PassthroughLabel

    var isSelected = false {
        didSet { refresh() }
    }

    init(title: String, symbol: String) {
        label = PassthroughLabel(labelWithString: title)
        super.init(frame: .zero)
        cornerRadius = DesignTokens.Radius.lg
        iconView.image = .symbol(symbol)
        iconView.symbolConfiguration = .init(pointSize: 16, weight: .regular)
        label.font = .systemFont(ofSize: 12, weight: .semibold)
        label.alignment = .center
        addSubview(iconView)
        addSubview(label)
        setAccessibilityRole(.radioButton)
        setAccessibilityLabel(title)
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SegmentCardView 只支持代码创建") }

    override var intrinsicContentSize: NSSize { NSSize(width: 112, height: 52) }

    private func refresh() {
        // 选中态：强调色的低明度底 + 强调色边框（visual-style §3.3）。
        fillToken = isSelected ? DesignTokens.Color.bgSegmentSelected : DesignTokens.Color.bgField
        borderToken = isSelected ? DesignTokens.Color.borderSegmentSelected : DesignTokens.Color.borderSubtle
        borderWidth = isSelected ? 1.5 : 1
        let tint = NSColor.token(isSelected ? DesignTokens.Color.accent : DesignTokens.Color.textSecondary)
        iconView.contentTintColor = tint
        label.textColor = .token(isSelected ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary)
        setAccessibilityValue(isSelected)
    }

    override func layout() {
        super.layout()
        iconView.frame = NSRect(x: (bounds.width - 20) / 2, y: 8, width: 20, height: 20)
        label.frame = NSRect(x: 4, y: 30, width: bounds.width - 8, height: 16)
    }

    override func mouseDown(with event: NSEvent) { onClick?() }
}

/// 带图标的分段卡片（本地 / SSH / SFTP）：单选，选中的用强调色边框和低明度强调色底。
@MainActor
final class KSegmentedCards: NSStackView {
    struct Segment {
        let title: String
        let symbol: String
    }

    /// 用户点选某一项（`selectedIndex` 已更新）。程序设置 `selectedIndex` 不回调。
    var onSelect: ((Int) -> Void)?

    var selectedIndex = 0 {
        didSet { refresh() }
    }

    private var cards: [SegmentCardView] = []

    init(segments: [Segment]) {
        super.init(frame: .zero)
        orientation = .horizontal
        spacing = DesignTokens.Spacing.sm
        alignment = .top
        distribution = .fill
        cards = segments.enumerated().map { index, segment in
            let card = SegmentCardView(title: segment.title, symbol: segment.symbol)
            card.onClick = { [weak self] in self?.select(index) }
            card.translatesAutoresizingMaskIntoConstraints = false
            card.widthAnchor.constraint(equalToConstant: 112).isActive = true
            card.heightAnchor.constraint(equalToConstant: 52).isActive = true
            addArrangedSubview(card)
            return card
        }
        // 尾部弹性空白：卡片保持固定宽度、靠左，不被拉伸。
        let spacer = NSView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        addArrangedSubview(spacer)
        refresh()
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("KSegmentedCards 只支持代码创建") }

    /// 用户点选。internal 供测试直接调用。
    func select(_ index: Int) {
        guard cards.indices.contains(index) else { return }
        selectedIndex = index
        onSelect?(index)
    }

    private func refresh() {
        for (i, card) in cards.enumerated() { card.isSelected = i == selectedIndex }
    }
}
