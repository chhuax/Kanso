import AppKit

/// 下拉框（visual-style §3.1）：外观与 `KTextField` 一致，右侧一个小的上下箭头。
///
/// 自绘按钮 + `NSMenu`，而不是 `NSPopUpButton`：后者在新版 macOS 上是系统胶囊外观，无法调整。
/// 菜单本身仍是系统菜单（键盘导航、辅助功能都由系统提供）。
///
/// 选择有两条路径，别混用：`selectItem(at:)` 只改显示（程序设置初值 / 同步外部状态，不回调）；
/// `userSelect(at:)` 等同用户在菜单里点选，会回调 `onChange`。
@MainActor
final class KPopUpField: StyledBoxView {
    private let label = PassthroughLabel(labelWithString: "")
    private let chevron = NSImageView()
    private(set) var titles: [String] = []

    private(set) var selectedIndex = 0 {
        didSet { refreshTitle() }
    }

    /// 用户选了某一项（已更新 `selectedIndex`）。
    var onChange: ((Int) -> Void)?

    var isEnabled = true {
        didSet { alphaValue = isEnabled ? 1 : 0.5 }
    }

    /// 菜单展开期间边框用强调色，等价于输入框的焦点态。
    private var isMenuOpen = false {
        didSet { refreshBorder() }
    }

    private var isFocused = false {
        didSet { refreshBorder() }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        fillToken = DesignTokens.Color.bgField
        borderToken = DesignTokens.Color.borderField
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.textColor = .token(DesignTokens.Color.textPrimary)
        label.lineBreakMode = .byTruncatingTail
        chevron.image = .symbol("chevron.up.chevron.down")
        chevron.symbolConfiguration = .init(pointSize: 9, weight: .semibold)
        chevron.contentTintColor = .token(DesignTokens.Color.textSecondary)
        addSubview(label)
        addSubview(chevron)
        setAccessibilityRole(.popUpButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KPopUpField 只支持代码创建") }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: DesignTokens.Spacing.fieldHeight)
    }

    override var acceptsFirstResponder: Bool { isEnabled }

    var indexOfSelectedItem: Int { selectedIndex }
    var selectedTitle: String? { titles.indices.contains(selectedIndex) ? titles[selectedIndex] : nil }

    func setItems(_ titles: [String]) {
        self.titles = titles
        selectedIndex = min(selectedIndex, max(0, titles.count - 1))
        refreshTitle()
    }

    func selectItem(at index: Int) {
        guard titles.indices.contains(index) else { return }
        selectedIndex = index
    }

    func userSelect(at index: Int) {
        guard titles.indices.contains(index) else { return }
        selectedIndex = index
        onChange?(index)
    }

    /// 当前选项对应的菜单；internal 供测试检查菜单内容与勾选态。
    func makeMenu() -> NSMenu {
        let menu = NSMenu()
        for (index, title) in titles.enumerated() {
            menu.addItem(ActionMenuItem(title, checked: index == selectedIndex) { [weak self] in
                self?.userSelect(at: index)
            })
        }
        // 菜单至少与字段等宽，看起来像"从字段里展开"。
        menu.minimumWidth = bounds.width
        return menu
    }

    private func showMenu() {
        guard isEnabled, !titles.isEmpty else { return }
        isMenuOpen = true
        // popUp 会阻塞到菜单关闭（选中项的回调在返回前已执行）。
        makeMenu().popUp(positioning: nil, at: NSPoint(x: 0, y: bounds.height + 4), in: self)
        isMenuOpen = false
    }

    private func refreshTitle() {
        label.stringValue = selectedTitle ?? ""
        setAccessibilityValue(label.stringValue)
    }

    private func refreshBorder() {
        borderToken = (isMenuOpen || isFocused) ? DesignTokens.Color.accent : DesignTokens.Color.borderField
    }

    override func layout() {
        super.layout()
        let pad = DesignTokens.Spacing.fieldPaddingX
        let chevronSize = NSSize(width: 12, height: 14)
        chevron.frame = NSRect(x: bounds.width - pad - chevronSize.width, y: (bounds.height - chevronSize.height) / 2,
                               width: chevronSize.width, height: chevronSize.height)
        let textHeight = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: pad, y: (bounds.height - textHeight) / 2,
                             width: max(0, chevron.frame.minX - pad - DesignTokens.Spacing.xs), height: textHeight)
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        showMenu()
    }

    override func becomeFirstResponder() -> Bool {
        isFocused = true
        return true
    }

    override func resignFirstResponder() -> Bool {
        isFocused = false
        return true
    }

    /// 键盘：空格 / 回车 / 下箭头展开菜单；其余交给上层（Tab 切换焦点）。
    override func keyDown(with event: NSEvent) {
        switch event.charactersIgnoringModifiers {
        case " ", "\r", String(UnicodeScalar(NSDownArrowFunctionKey)!):
            showMenu()
        default:
            super.keyDown(with: event)
        }
    }
}
