import AppKit
import KansoBridge
import KansoUI

/// 对话框里的提示条：图标 + 文字，实色底 + 细边框（用于文件级警告与扫描失败）。
/// 文字为空时由调用方隐藏整条。
@MainActor
final class NoticeBannerView: StyledBoxView {
    let label = NSTextField(wrappingLabelWithString: "")

    init(symbol: String, tintToken: String) {
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgField
        borderToken = DesignTokens.Color.borderField
        cornerRadius = DesignTokens.Radius.md

        let icon = NSImageView(image: .symbol(symbol))
        icon.symbolConfiguration = .init(pointSize: 13, weight: .regular)
        icon.contentTintColor = .token(tintToken)
        icon.setContentHuggingPriority(.required, for: .horizontal)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.textColor = .token(DesignTokens.Color.textPrimary)
        label.isSelectable = true
        label.maximumNumberOfLines = 0
        // 宽度 = 对话框主体内宽 - 横向内边距 - 图标 - 间距；折行高度由它决定。
        label.preferredMaxLayoutWidth = SshConfigImportViewController.width - DialogShellView.bodyInset * 2 - 24 - 13 - 8
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let row = NSStackView(views: [icon, label])
        row.orientation = .horizontal
        row.alignment = .top
        row.spacing = DesignTokens.Spacing.sm
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
        ])
        setAccessibilityRole(.staticText)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("NoticeBannerView 只支持代码创建") }

    func setText(_ text: String) {
        label.stringValue = text
        isHidden = text.isEmpty
        setAccessibilityLabel(text)
    }
}

/// 行内的小标签（"已保存，将更新"）：强调色低明度底 + 强调色边框，与分段卡片的选中态同一套 token。
@MainActor
final class TagPillView: StyledBoxView {
    private let label: PassthroughLabel

    init(text: String) {
        label = PassthroughLabel(labelWithString: text)
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgSegmentSelected
        borderToken = DesignTokens.Color.borderSegmentSelected
        cornerRadius = DesignTokens.Radius.sm
        label.font = .systemFont(ofSize: 10, weight: .medium)
        label.textColor = .token(DesignTokens.Color.accent)
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("TagPillView 只支持代码创建") }

    override var intrinsicContentSize: NSSize {
        NSSize(width: ceil(label.intrinsicContentSize.width) + 12, height: 18)
    }

    override func layout() {
        super.layout()
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: 6, y: (bounds.height - h) / 2, width: bounds.width - 12, height: h)
    }
}

/// 候选主机行：复选框 + 别名（主文字）+ `user@host:port`（次要文字）+ 私钥 / 跳板（第三行，有才显示）；
/// 右侧依次是"已保存，将更新"标签与警告图标（悬停显示警告内容）。单击整行 = 切换勾选。
@MainActor
final class CandidateRowView: LauncherHoverRow {
    static let compactHeight: CGFloat = 50
    static let extendedHeight: CGFloat = 66

    var onToggle: (() -> Void)?

    let checkbox = KCheckbox()
    let titleLabel = PassthroughLabel(labelWithString: "")
    let destinationLabel = PassthroughLabel(labelWithString: "")
    let extraLabel = PassthroughLabel(labelWithString: "")
    private(set) var existingTag: TagPillView?
    private(set) var warningIcon: NSImageView?

    private let hasExtra: Bool

    init(row: SshConfigImportModel.Row) {
        let extras = [row.identityText, row.jumpText].compactMap { $0 }
        hasExtra = !extras.isEmpty
        super.init(frame: .zero)
        checkbox.isChecked = row.isSelected
        titleLabel.stringValue = row.alias
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        titleLabel.lineBreakMode = .byTruncatingTail
        destinationLabel.stringValue = row.destination
        destinationLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        destinationLabel.textColor = .token(DesignTokens.Color.textSecondary)
        destinationLabel.lineBreakMode = .byTruncatingMiddle
        extraLabel.stringValue = extras.joined(separator: "  ·  ")
        extraLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        extraLabel.textColor = .token(DesignTokens.Color.textTertiary)
        extraLabel.lineBreakMode = .byTruncatingMiddle
        extraLabel.isHidden = !hasExtra
        for v in [checkbox, titleLabel, destinationLabel, extraLabel] { addSubview(v) }

        if row.isExisting {
            let existingTag = TagPillView(text: SshConfigImportModel.existingTagText)
            addSubview(existingTag)
            self.existingTag = existingTag
        }
        if !row.warnings.isEmpty {
            let icon = NSImageView(image: .symbol("exclamationmark.triangle"))
            icon.symbolConfiguration = .init(pointSize: 12, weight: .regular)
            icon.contentTintColor = .token(DesignTokens.Color.statusWarning)
            icon.toolTip = row.warnings.joined(separator: "\n")
            addSubview(icon)
            warningIcon = icon
        }
        setAccessibilityRole(.checkBox)
        setAccessibilityLabel(row.alias)
        setAccessibilityValue(row.isSelected)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("CandidateRowView 只支持代码创建") }

    var height: CGFloat { hasExtra ? Self.extendedHeight : Self.compactHeight }

    func setChecked(_ checked: Bool) {
        checkbox.isChecked = checked
        setAccessibilityValue(checked)
    }

    override func layout() {
        super.layout()
        let inset: CGFloat = 14
        let textX = inset + KCheckbox.size + 10
        var right = bounds.width - inset
        let titleH = ceil(titleLabel.intrinsicContentSize.height)
        let subH = ceil(destinationLabel.intrinsicContentSize.height)
        let lines: CGFloat = hasExtra ? 3 : 2
        let total = titleH + subH * (lines - 1) + 2 * (lines - 1)
        let top = (bounds.height - total) / 2
        if let icon = warningIcon {
            icon.frame = NSRect(x: right - 16, y: top + (titleH - 16) / 2, width: 16, height: 16)
            right = icon.frame.minX - 6
        }
        if let existingTag {
            let size = existingTag.intrinsicContentSize
            existingTag.frame = NSRect(x: right - size.width, y: top + (titleH - size.height) / 2, width: size.width, height: size.height)
            right = existingTag.frame.minX - 8
        }
        titleLabel.frame = NSRect(x: textX, y: top, width: max(0, right - textX), height: titleH)
        let fullWidth = max(0, bounds.width - inset - textX)
        destinationLabel.frame = NSRect(x: textX, y: top + titleH + 2, width: fullWidth, height: subH)
        extraLabel.frame = NSRect(x: textX, y: destinationLabel.frame.maxY + 2, width: fullWidth, height: subH)
        checkbox.frame = NSRect(x: inset, y: top + (titleH - KCheckbox.size) / 2, width: KCheckbox.size, height: KCheckbox.size)
    }

    override func mouseDown(with event: NSEvent) { onToggle?() }

    override func accessibilityPerformPress() -> Bool {
        onToggle?()
        return true
    }
}

/// 候选主机列表：卡片内可滚动，最高 `maxListHeight`，内容更少时按内容收缩。
/// 自己按行高排版（行数不多、不需要复用），不用 `NSTableView`（系统表格样式与风格不一致）。
@MainActor
final class CandidateListView: NSScrollView {
    static let maxListHeight: CGFloat = 264

    private let documentContainer = FlippedView()
    private(set) var rowViews: [CandidateRowView] = []

    init() {
        super.init(frame: .zero)
        drawsBackground = false
        hasVerticalScroller = true
        autohidesScrollers = true
        scrollerStyle = .overlay
        borderType = .noBorder
        documentView = documentContainer
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("CandidateListView 只支持代码创建") }

    private var contentHeight: CGFloat { rowViews.reduce(0) { $0 + $1.height } }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: min(Self.maxListHeight, contentHeight))
    }

    func setRows(_ views: [CandidateRowView]) {
        for v in rowViews { v.removeFromSuperview() }
        rowViews = views
        for v in views { documentContainer.addSubview(v) }
        invalidateIntrinsicContentSize()
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let width = contentSize.width
        documentContainer.frame = NSRect(x: 0, y: 0, width: width, height: contentHeight)
        var y: CGFloat = 0
        for v in rowViews {
            v.frame = NSRect(x: 0, y: y, width: width, height: v.height)
            y += v.height
        }
    }
}

extension KCard {
    /// 结果卡片：一句汇总 + 可选的明细列表（每条一行，次要文字色）。导入完成后替换对话框主体，
    /// 数据导入结果对话框也用它。`width` 是对话框宽度，用于给折行文字定最大宽度。
    /// 用工厂方法而不是子类：`KCard` 是 final，且结果卡片没有自己的状态。
    static func result(_ content: ImportResultContent, width: CGFloat) -> KCard {
        let card = KCard(title: String(localized: "Result"), symbol: "checkmark.circle")
        let innerWidth = width - DialogShellView.bodyInset * 2 - padding * 2
        let summary = NSTextField(wrappingLabelWithString: content.summary)
        summary.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        summary.textColor = .token(DesignTokens.Color.textPrimary)
        summary.isSelectable = true
        summary.maximumNumberOfLines = 0
        summary.preferredMaxLayoutWidth = innerWidth
        card.addRow(summary)

        guard !content.details.isEmpty else { return card }
        let title = NSTextField(labelWithString: content.detailsTitle ?? "")
        title.font = .systemFont(ofSize: 12)
        title.textColor = .token(DesignTokens.Color.textSecondary)
        card.addRow(title)
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = DesignTokens.Spacing.sm
        for detail in content.details {
            let icon = NSImageView(image: .symbol("exclamationmark.triangle"))
            icon.symbolConfiguration = .init(pointSize: 11, weight: .regular)
            icon.contentTintColor = .token(DesignTokens.Color.statusWarning)
            icon.setContentHuggingPriority(.required, for: .horizontal)
            let text = NSTextField(wrappingLabelWithString: detail)
            text.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            text.textColor = .token(DesignTokens.Color.textSecondary)
            text.isSelectable = true
            text.maximumNumberOfLines = 0
            text.preferredMaxLayoutWidth = innerWidth - 11 - DesignTokens.Spacing.sm
            let line = NSStackView(views: [icon, text])
            line.orientation = .horizontal
            line.alignment = .top
            line.spacing = DesignTokens.Spacing.sm
            stack.addArrangedSubview(line)
        }
        card.addRow(stack)
        return card
    }
}
