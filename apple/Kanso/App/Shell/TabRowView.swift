import AppKit
import KansoUI

/// 承载标签行的容器：竖向侧栏列表（`SidebarListView`）或顶部标签条（`TabStripView`）。
///
/// 行只把鼠标事件原样转发过来，拖拽状态由容器持有：容器刷新会重建行，状态放在行里会跟着丢。
@MainActor
protocol TabRowHost: AnyObject {
    func rowMouseDown(_ row: TabRowView, event: NSEvent)
    func rowMouseDragged(_ row: TabRowView, event: NSEvent)
    func rowMouseUp(_ row: TabRowView, event: NSEvent)
    /// 右键按下：容器先取消进行中的拖拽，菜单由 AppKit 随后通过 `rowMenu(for:)` 弹出。
    func rowRightMouseDown()
    func rowCloseClicked(_ row: TabRowView)
    func rowMenu(for session: SessionID) -> NSMenu?
}

/// 标签行的外观。
enum TabRowStyle {
    /// 竖向侧栏的一行：主标题下可以有副标题。
    case row
    /// 顶部标签条里的一枚标签：单行，副标题（Git 分支）只进悬停提示，横向空间放不下第二行（ui-shell.md §2.5）。
    case chip
}

/// 一个标签的视图（自绘，不用 SwiftUI），竖向侧栏与顶部标签条共用，保证两种位置的内容与状态样式一致。
@MainActor
final class TabRowView: NSView {
    /// 行的展示数据。
    struct Model: Equatable {
        var item: TabItem
        /// 在完整侧栏（未筛选）中的序号，决定快捷键提示。
        var sidebarIndex: Int
        var selection: TabRowSelection
        /// MCP 打开标记与正在进行的操作。nil 表示都没有。
        var mcp: McpTabMark? = nil
    }

    weak var host: TabRowHost?
    let style: TabRowStyle
    var model: Model { didSet { if model != oldValue { applyModel() } } }
    var isHovered = false { didSet { if isHovered != oldValue { updateHoverState() } } }

    private let iconView = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let subtitleLabel = NSTextField(labelWithString: "")
    private let shortcutLabel = NSTextField(labelWithString: "")
    private let closeButton = NSButton()
    private let mcpOpenedMark = NSImageView()
    private var trackingArea: NSTrackingArea?

    override var isFlipped: Bool { true }

    init(model: Model, style: TabRowStyle = .row) {
        self.model = model
        self.style = style
        super.init(frame: .zero)
        iconView.imageScaling = .scaleProportionallyDown
        titleLabel.lineBreakMode = .byTruncatingHead // 路径最有辨识度的是末尾几段，截断要保留末尾。
        titleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiTabTitle)
        titleLabel.textColor = .token(DesignTokens.Color.textPrimary)
        subtitleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.font = .systemFont(ofSize: DesignTokens.Font.uiTabSubtitle)
        subtitleLabel.textColor = .token(DesignTokens.Color.textSecondary)
        shortcutLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        shortcutLabel.textColor = .token(DesignTokens.Color.textTertiary)
        closeButton.isBordered = false
        closeButton.image = .symbol("xmark", description: String(localized: "Close"))
        closeButton.imagePosition = .imageOnly
        closeButton.contentTintColor = .token(DesignTokens.Color.textSecondary)
        closeButton.target = self
        closeButton.action = #selector(closeClicked)
        mcpOpenedMark.image = .symbol("point.3.connected.trianglepath.dotted", description: String(localized: "Opened by MCP"))
        mcpOpenedMark.imageScaling = .scaleProportionallyDown
        mcpOpenedMark.contentTintColor = .token(DesignTokens.Color.accent)
        mcpOpenedMark.isHidden = true
        for v in [iconView, titleLabel, subtitleLabel, shortcutLabel, closeButton, mcpOpenedMark] { addSubview(v) }
        applyModel()
        updateHoverState()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("TabRowView 只支持代码创建") }

    // MARK: - 内容

    private var item: TabItem { model.item }

    private var isDisconnected: Bool {
        if case .closed = item.connection { return true }
        return false
    }

    private var isConnecting: Bool { item.connection == .connecting }

    private func applyModel() {
        let home = NSHomeDirectory()
        let hasCustomTitle = item.customTitle?.isEmpty == false
        titleLabel.lineBreakMode = hasCustomTitle ? .byTruncatingTail : .byTruncatingHead
        titleLabel.stringValue = TabPresentation.primaryTitle(item, homeDirectory: home)
        let subtitle = isDisconnected ? String(localized: "Disconnected") : TabPresentation.subtitle(item, homeDirectory: home)
        subtitleLabel.lineBreakMode = (hasCustomTitle && item.kind == .local && !isDisconnected) ? .byTruncatingHead : .byTruncatingTail
        subtitleLabel.stringValue = subtitle ?? ""
        subtitleLabel.isHidden = subtitle == nil || style == .chip
        shortcutLabel.stringValue = TabPresentation.shortcutLabel(sidebarIndex: model.sidebarIndex)
        var tip = TabDescriptions.tooltip(for: item)
        if let extra = model.mcp?.tooltipLines, !extra.isEmpty {
            tip += "\n" + extra.joined(separator: "\n")
        }
        toolTip = tip
        mcpOpenedMark.isHidden = model.mcp?.openedByClient == nil
        updateIcon()
        setAccessibilityLabel(titleLabel.stringValue)
        needsLayout = true
        needsDisplay = true
    }

    private func updateIcon() {
        var symbol: String
        var tint = NSColor.token(DesignTokens.Color.textSecondary)
        switch item.kind {
        case .local: symbol = "terminal"
        case .ssh: symbol = "server.rack"
        case .sftp: symbol = "folder"
        }
        // AI 活动时换成该工具的图标、用工具品牌色（无素材的类型仍用 sparkles）。
        switch item.activity {
        case .agentWorking(_, let iconAsset), .agentIdle(_, let iconAsset):
            iconView.image = AgentIcon.image(assetName: iconAsset)
            iconView.contentTintColor = AgentIcon.tint(assetName: iconAsset)
            return
        case .idle, .commandRunning: break
        }
        iconView.image = .symbol(symbol)
        iconView.contentTintColor = tint
    }

    private func updateHoverState() {
        shortcutLabel.isHidden = !isHovered
        closeButton.isHidden = !isHovered
        needsLayout = true
        needsDisplay = true
    }

    /// MCP 打开标记当前是否显示。
    var showsMcpOpenedMark: Bool { !mcpOpenedMark.isHidden }

    /// 右侧状态标记（圆点）：命令 / AI 运行中、未读完成、连接中。已断开改由副标题与左侧暗灰条表达；悬停时让位给快捷键与关闭按钮。
    private var statusMarker: (color: NSColor, hollow: Bool)? {
        if model.mcp?.activityTool != nil { return (.token(DesignTokens.Color.accent), false) }
        if isDisconnected { return nil }
        if isConnecting { return (.token(DesignTokens.Color.statusRunning), true) }
        if item.unreadDone { return (.token(DesignTokens.Color.statusUnreadDone), false) }
        switch item.activity {
        case .commandRunning: return (.token(DesignTokens.Color.statusRunning), false)
        case .agentWorking: return (.token(DesignTokens.Color.statusAgentRunning), false)
        case .idle, .agentIdle: return nil
        }
    }

    // MARK: - 布局与绘制

    private static let iconSize: CGFloat = 16
    private static let markerSize: CGFloat = 7
    private static let closeSize: CGFloat = 18

    override func layout() {
        super.layout()
        let pad = DesignTokens.Spacing.tabRowPaddingX
        let h = bounds.height
        iconView.frame = NSRect(x: pad, y: (h - Self.iconSize) / 2, width: Self.iconSize, height: Self.iconSize)
        mcpOpenedMark.frame = NSRect(x: iconView.frame.maxX - 8, y: iconView.frame.maxY - 8, width: 10, height: 10)
        let textX = iconView.frame.maxX + DesignTokens.Spacing.sm
        var trailing = bounds.width - pad
        if isHovered {
            closeButton.frame = NSRect(x: trailing - Self.closeSize, y: (h - Self.closeSize) / 2, width: Self.closeSize, height: Self.closeSize)
            shortcutLabel.sizeToFit()
            let sw = shortcutLabel.frame.width
            shortcutLabel.frame = NSRect(
                x: closeButton.frame.minX - DesignTokens.Spacing.xs - sw, y: (h - shortcutLabel.frame.height) / 2,
                width: sw, height: shortcutLabel.frame.height
            )
            trailing = shortcutLabel.frame.minX - DesignTokens.Spacing.xs
        } else if statusMarker != nil {
            trailing -= Self.markerSize + DesignTokens.Spacing.xs
        }
        let textW = max(0, trailing - textX)
        let titleH = ceil(titleLabel.intrinsicContentSize.height)
        if subtitleLabel.isHidden {
            titleLabel.frame = NSRect(x: textX, y: (h - titleH) / 2, width: textW, height: titleH)
        } else {
            let subH = ceil(subtitleLabel.intrinsicContentSize.height)
            let top = (h - titleH - subH) / 2
            titleLabel.frame = NSRect(x: textX, y: top, width: textW, height: titleH)
            subtitleLabel.frame = NSRect(x: textX, y: top + titleH, width: textW, height: subH)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        let radius = DesignTokens.Radius.tabRow
        // 选中靠 1px 描边才看得出来：底色和侧栏太接近，描边画在路径中线，内缩半像素才不会被裁掉。
        let outlined = model.selection != .none
        let rect = outlined ? bounds.insetBy(dx: 0.5, dy: 0.5) : bounds
        let corner = max(0, radius - (outlined ? 0.5 : 0))
        let path = NSBezierPath(roundedRect: rect, xRadius: corner, yRadius: corner)
        switch model.selection {
        case .windowActive:
            NSColor.token(DesignTokens.Color.bgTabSelected).setFill()
            path.fill()
            NSColor.token(DesignTokens.Color.borderStrong).setStroke()
            path.lineWidth = 1
            path.stroke()
        case .paneActive:
            NSColor.token(DesignTokens.Color.bgTabSelected).withAlphaComponent(0.5).setFill()
            path.fill()
            NSColor.token(DesignTokens.Color.borderStrong).withAlphaComponent(0.5).setStroke()
            path.lineWidth = 1
            path.stroke()
        case .none:
            if isHovered {
                NSColor.token(DesignTokens.Color.bgTabHover).setFill()
                path.fill()
            }
        }
        // 左侧会话颜色细条；已断开时熄灭为暗灰条。标签条里的标签比侧栏行矮，上下留白相应收窄，否则色条只剩一个点。
        let barInset: CGFloat = style == .chip ? 6 : 9
        let barColor = isDisconnected ? NSColor.token(DesignTokens.Color.borderStrong) : SessionColor.color(for: item)
        barColor.setFill()
        NSBezierPath(
            roundedRect: NSRect(x: 4, y: barInset, width: 3, height: max(0, bounds.height - barInset * 2)), xRadius: 1.5, yRadius: 1.5
        ).fill()
        // 状态标记。
        if !isHovered, let marker = statusMarker {
            let rect = NSRect(
                x: bounds.width - DesignTokens.Spacing.tabRowPaddingX - Self.markerSize,
                y: (bounds.height - Self.markerSize) / 2, width: Self.markerSize, height: Self.markerSize
            )
            marker.color.setFill()
            marker.color.setStroke()
            let dot = NSBezierPath(ovalIn: marker.hollow ? rect.insetBy(dx: 0.75, dy: 0.75) : rect)
            if marker.hollow {
                dot.lineWidth = 1.5
                dot.stroke()
            } else {
                dot.fill()
            }
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
        updateIcon()
    }

    // MARK: - 鼠标

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(area)
        trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    /// 子视图（文字、图标）不接收鼠标，整行统一由自己响应；只有关闭按钮例外。
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard frame.contains(point) else { return nil }
        let local = convert(point, from: superview)
        if !closeButton.isHidden, closeButton.frame.contains(local) { return closeButton }
        return self
    }

    // 按下即切换标签（不是松开）。
    override func mouseDown(with event: NSEvent) { host?.rowMouseDown(self, event: event) }
    override func mouseDragged(with event: NSEvent) { host?.rowMouseDragged(self, event: event) }
    override func mouseUp(with event: NSEvent) { host?.rowMouseUp(self, event: event) }

    override func rightMouseDown(with event: NSEvent) {
        host?.rowRightMouseDown()
        super.rightMouseDown(with: event) // 交给 AppKit 弹出 menu(for:)
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        host?.rowMenu(for: item.id)
    }

    @objc private func closeClicked() { host?.rowCloseClicked(self) }
}
