import AppKit
import KansoUI

/// 标题栏右侧的 Git 按钮：分支图标 + 改动统计（绿色 +N、红色 −M）。
///
/// 平时无边框无底色，悬停时整块出现圆角底色；没有改动或不在仓库里时只显示图标。
/// 尺寸随统计文字变化，宿主在 `onSizeChange` 里调整标题栏附件的宽度。
@MainActor
final class GitTitlebarButton: NSView {
    var onPress: (() -> Void)?
    /// 内容宽度变化时回调（参数为新的总宽度）。
    var onSizeChange: ((CGFloat) -> Void)?

    private let icon = NSImageView()
    private let stats = NSTextField(labelWithString: "")
    private var isHovered = false { didSet { needsDisplay = true } }
    private static let pad: CGFloat = 6
    private static let iconSize: CGFloat = 16
    private static let statsFont = NSFont.monospacedDigitSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .medium)

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = DesignTokens.Radius.md
        icon.image = .symbol("arrow.triangle.branch")
        icon.symbolConfiguration = .init(pointSize: 13, weight: .regular)
        stats.font = Self.statsFont
        // 单行不换行：宽度不够时宁可裁掉，也不能在空格处折行把"−M"挤到看不见的第二行。
        stats.cell?.wraps = false
        stats.cell?.usesSingleLineMode = true
        stats.lineBreakMode = .byClipping
        for v in [icon, stats] { addSubview(v) }
        setAccessibilityRole(.button)
        setAccessibilityLabel(String(localized: "Changes"))
        toolTip = String(localized: "Changes")
        update(added: nil, deleted: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("GitTitlebarButton 只支持代码创建") }

    /// 更新统计；`added` 为 nil 表示不在仓库里或还没有结果，只显示图标。
    func update(added: UInt64?, deleted: UInt64?) {
        if let added, let deleted, added + deleted > 0 {
            let text = NSMutableAttributedString()
            // 字体必须写进属性：属性串整体替换标签内容，缺字体会退回默认字体，测出的宽度与实际绘制不一致。
            text.append(NSAttributedString(string: "+\(added)", attributes: [.foregroundColor: NSColor.systemGreen, .font: Self.statsFont]))
            text.append(NSAttributedString(string: " −\(deleted)", attributes: [.foregroundColor: NSColor.systemRed, .font: Self.statsFont]))
            stats.attributedStringValue = text
            stats.isHidden = false
        } else {
            stats.attributedStringValue = NSAttributedString()
            stats.isHidden = true
        }
        onSizeChange?(fittingWidth)
        needsLayout = true
    }

    /// 内容需要的总宽度。
    var fittingWidth: CGFloat {
        // 直接量属性串：标签的 intrinsicContentSize 在内容刚替换时可能还是旧值。
        let textWidth = stats.isHidden ? 0 : ceil(stats.attributedStringValue.size().width) + 4 + 6
        return Self.pad * 2 + Self.iconSize + textWidth
    }

    override func layout() {
        super.layout()
        icon.frame = NSRect(x: Self.pad, y: (bounds.height - Self.iconSize) / 2, width: Self.iconSize, height: Self.iconSize)
        let h = ceil(stats.attributedStringValue.size().height)
        stats.frame = NSRect(x: icon.frame.maxX + 6, y: (bounds.height - h) / 2, width: max(0, bounds.width - icon.frame.maxX - 6 - Self.pad), height: h)
    }

    override func updateLayer() {
        layer?.backgroundColor = isHovered ? resolvedCGColor(.token(DesignTokens.Color.bgRowHover)) : nil
        icon.contentTintColor = .token(isHovered ? DesignTokens.Color.textPrimary : DesignTokens.Color.textSecondary)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }
    override func mouseDown(with event: NSEvent) {}
    override func mouseUp(with event: NSEvent) {
        if bounds.contains(convert(event.locationInWindow, from: nil)) { onPress?() }
    }
    override func accessibilityPerformPress() -> Bool {
        onPress?()
        return true
    }
    /// 标题栏里的控件要吃掉拖动，否则按下后会变成拖动窗口。
    override var mouseDownCanMoveWindow: Bool { false }
}
