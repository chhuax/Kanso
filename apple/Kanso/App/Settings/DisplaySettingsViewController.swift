import AppKit

/// 设置页的公共骨架：窗口底色的根视图 + 固定宽度 + 卡片纵向排布（visual-style §3.3 的卡片风格）。
@MainActor
class SettingsPageViewController: NSViewController {
    static let pageWidth: CGFloat = 560
    static let inset: CGFloat = 20

    /// 把卡片放进窗口底色的根视图，宽度固定；卡片高度贴合内容、从顶部排布，不随窗口被拉高。
    func makePageView(_ cards: [NSView]) -> StyledBoxView {
        let root = StyledBoxView()
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0
        let stack = NSStackView(views: cards)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = DesignTokens.Spacing.lg
        stack.setHuggingPriority(.required, for: .vertical)
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        let inset = Self.inset
        // 底部用低优先级等式：fittingSize 时根视图高度贴合内容；父视图强行拉高时卡片不跟着被拉伸。
        let bottom = stack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -inset)
        bottom.priority = .defaultLow
        NSLayoutConstraint.activate([
            root.widthAnchor.constraint(equalToConstant: Self.pageWidth),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: inset),
            bottom,
            root.bottomAnchor.constraint(greaterThanOrEqualTo: stack.bottomAnchor, constant: inset),
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: inset),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -inset),
        ])
        for card in cards {
            card.translatesAutoresizingMaskIntoConstraints = false
            card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
            card.setContentHuggingPriority(.required, for: .vertical)
            card.setContentCompressionResistancePriority(.required, for: .vertical)
        }
        return root
    }

    /// 带滚动的设置页高度上限；内容更高时滚动。键盘快捷键页与 MCP 页共用。
    static let maxPageHeight: CGFloat = 620

    /// 把已经布好约束的文档视图放进纵向滚动区（overlay 滚动条、无边框、透明背景），文档宽度跟随可视区。
    func makeScrollView(document: NSView) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.documentView = document
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.borderType = .noBorder
        document.translatesAutoresizingMaskIntoConstraints = false
        document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor).isActive = true
        return scroll
    }

    /// 说明文字（11pt、次要色、可换行）。
    static func caption(_ text: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.textColor = .token(DesignTokens.Color.textTertiary)
        label.preferredMaxLayoutWidth = pageWidth - inset * 2 - KCard.padding * 2
        return label
    }
}

/// "显示"页（ui-shell.md §10、terminal.md §15）：终端字体与字号、光标、回滚行数、最低对比度保护。
///
/// 每个改动立即写入设置；设置变化再由 `WorkspaceController` 应用到所有终端视图与 core，所以这里只管读写设置，
/// 不直接碰终端视图。界面字体 / 界面字号本版不做。
@MainActor
final class DisplaySettingsViewController: SettingsPageViewController, NSTextFieldDelegate {
    private let settings: SettingsStore
    private let fontFamilies: @MainActor () -> [String]

    let fontField = KTextField()
    let fontChooseButton = KButton(title: String(localized: "Choose…"), style: .secondary)
    let fontWarning = SettingsPageViewController.caption("")
    let sizeField = KNumberField(range: DisplaySettings.fontSizeRange, step: 1)
    let lineHeightField = KNumberField(range: DisplaySettings.lineHeightRange, step: 0.05, fractionDigits: 2)
    let cursorShapeCards = KSegmentedCards(segments: [
        .init(title: CursorShapeSetting.block.displayName, symbol: "rectangle.fill"),
        .init(title: CursorShapeSetting.underline.displayName, symbol: "underline"),
        .init(title: CursorShapeSetting.bar.displayName, symbol: "text.cursor"),
    ])
    let blinkRow = KCheckRow(title: String(localized: "Blinking cursor"))
    let scrollbackField = KNumberField(
        range: Double(DisplaySettings.scrollbackRange.lowerBound)...Double(DisplaySettings.scrollbackRange.upperBound),
        step: 1_000, fieldWidth: 120
    )
    let contrastRow = KCheckRow(title: String(localized: "Minimum contrast protection"))
    let semanticRow = KCheckRow(title: String(localized: "Semantic coloring"))
    let contrastField = KNumberField(range: DisplaySettings.contrastRange, step: 0.5, fractionDigits: 1)

    /// - Parameter fontFamilies: 供"选择…"菜单列出的字体家族；默认枚举本机已安装的等宽字体，测试注入固定列表。
    init(settings: SettingsStore, fontFamilies: @escaping @MainActor () -> [String] = TerminalFontResolver.installedMonospacedFamilies) {
        self.settings = settings
        self.fontFamilies = fontFamilies
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("DisplaySettingsViewController 只支持代码创建") }

    override func loadView() {
        // 字体
        // 留空即用系统等宽字体（SF Mono），占位提示写明实际字体，避免误以为默认是别的字体。
        fontField.placeholder = String(localized: "System Monospaced (SF Mono)")
        fontField.textField.delegate = self
        fontField.textField.target = self
        fontField.textField.action = #selector(fontCommitted(_:))
        fontChooseButton.setContentHuggingPriority(.required, for: .horizontal)
        fontChooseButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        fontChooseButton.onPress = { [weak self] in self?.showFontMenu() }
        let fontRow = NSStackView(views: [fontField, fontChooseButton])
        fontRow.orientation = .horizontal
        fontRow.alignment = .centerY
        fontRow.spacing = DesignTokens.Spacing.sm
        let fontLabeled = LabeledField(title: String(localized: "Terminal Font"), control: fontRow)
        let sizeLabeled = LabeledField(title: String(localized: "Font Size"), control: sizeField)
        // 字号固定宽度靠右，字体名输入框吃掉剩余宽度；不设低 hugging 的话输入框会被压成一条缝。
        sizeLabeled.setContentHuggingPriority(.required, for: .horizontal)
        fontLabeled.setContentHuggingPriority(.defaultLow, for: .horizontal)
        fontField.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let topRow = NSStackView(views: [fontLabeled, sizeLabeled])
        topRow.orientation = .horizontal
        topRow.alignment = .top
        topRow.distribution = .fill
        topRow.spacing = DesignTokens.Spacing.md
        sizeField.onChange = { [weak self] value in self?.settings.updateDisplay { $0.fontSize = value } }
        lineHeightField.onChange = { [weak self] value in self?.settings.updateDisplay { $0.lineHeight = value } }

        let fontHint = Self.caption(String(
            localized: "Leave empty to use the system monospaced font. Icons missing from the font are taken from an installed Nerd Font automatically."
        ))
        fontWarning.textColor = .token(DesignTokens.Color.statusWarning)
        fontWarning.isHidden = true
        let fontCard = KCard(title: String(localized: "Font"), symbol: "textformat")
        fontCard.addRow(topRow)
        fontCard.addRow(LabeledField(title: String(localized: "Line Height"), control: lineHeightField))
        fontCard.addRow(fontHint)
        fontCard.addRow(fontWarning)

        // 光标
        cursorShapeCards.onSelect = { [weak self] index in
            guard CursorShapeSetting.allCases.indices.contains(index) else { return }
            self?.settings.updateDisplay { $0.cursorShape = CursorShapeSetting.allCases[index] }
        }
        blinkRow.onToggle = { [weak self] on in self?.settings.updateDisplay { $0.cursorBlink = on } }
        let cursorCard = KCard(title: String(localized: "Cursor"), symbol: "text.cursor")
        cursorCard.addRow(LabeledField(title: String(localized: "Shape"), control: cursorShapeCards))
        cursorCard.addRow(blinkRow)

        // 回滚
        scrollbackField.onChange = { [weak self] value in self?.settings.updateDisplay { $0.scrollbackLines = Int(value) } }
        let bufferCard = KCard(title: String(localized: "Scrollback"), symbol: "list.bullet.rectangle")
        bufferCard.addRow(LabeledField(title: String(localized: "Scrollback Lines"), control: scrollbackField))
        bufferCard.addRow(Self.caption(String(localized: "Lowering this discards the oldest lines immediately.")))

        // 最低对比度
        contrastRow.onToggle = { [weak self] on in self?.settings.updateDisplay { $0.minimumContrastEnabled = on } }
        contrastField.onChange = { [weak self] value in self?.settings.updateDisplay { $0.minimumContrastRatio = value } }
        let contrastCard = KCard(title: String(localized: "Contrast"), symbol: "circle.lefthalf.filled")
        contrastCard.addRow(contrastRow)
        semanticRow.onToggle = { [weak self] on in self?.settings.updateDisplay { $0.semanticColoring = on } }
        contrastCard.addRow(semanticRow)
        contrastCard.addRow(LabeledField(title: String(localized: "Contrast Ratio"), control: contrastField))
        contrastCard.addRow(Self.caption(String(
            localized: "Text whose contrast with its background is below this ratio (1–21) is lightened or darkened automatically."
        )))

        view = makePageView([fontCard, cursorCard, bufferCard, contrastCard])
        reload()
        settings.observe(self) { [weak self] section in
            if section == .display { self?.reload() }
        }
    }

    /// 把设置同步到控件（不触发回写）。
    private func reload() {
        let display = settings.display
        // 正在输入字体名时不要覆盖用户的输入：设置变化可能来自别处（菜单、恢复默认），
        // 但编辑中的文字以用户为准，提交后会再同步一次。
        if fontField.textField.currentEditor() == nil { fontField.stringValue = display.fontFamily }
        sizeField.setValue(display.fontSize)
        lineHeightField.setValue(display.lineHeight)
        cursorShapeCards.selectedIndex = CursorShapeSetting.allCases.firstIndex(of: display.cursorShape) ?? 0
        blinkRow.isChecked = display.cursorBlink
        scrollbackField.setValue(Double(display.scrollbackLines))
        contrastRow.isChecked = display.minimumContrastEnabled
        semanticRow.isChecked = display.semanticColoring
        contrastField.setValue(display.minimumContrastRatio)
        contrastField.isEnabled = display.minimumContrastEnabled
        refreshFontWarning()
    }

    private func refreshFontWarning() {
        let family = settings.display.fontFamily.trimmingCharacters(in: .whitespaces)
        let missing = !family.isEmpty && TerminalFontResolver.font(family: family, size: 13).isFallback
        fontWarning.stringValue = missing
            ? String(localized: "“\(family)” isn’t installed. The system monospaced font is used instead.") : ""
        fontWarning.isHidden = !missing
    }

    // MARK: 字体

    /// "选择…"弹出的菜单：系统等宽字体（清空）+ 本机已安装的等宽字体家族，当前选择打勾。internal 供测试检查。
    func makeFontMenu() -> NSMenu {
        let current = settings.display.fontFamily.trimmingCharacters(in: .whitespaces)
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "System Monospaced"), checked: current.isEmpty) { [weak self] in
            self?.setFontFamily("")
        })
        menu.addItem(.separator())
        for family in fontFamilies() {
            menu.addItem(ActionMenuItem(family, checked: family == current) { [weak self] in
                self?.setFontFamily(family)
            })
        }
        return menu
    }

    private func showFontMenu() {
        makeFontMenu().popUp(positioning: nil, at: NSPoint(x: 0, y: fontChooseButton.bounds.height + 4), in: fontChooseButton)
    }

    /// 设置字体家族（菜单选择、手填提交共用）。
    func setFontFamily(_ family: String) {
        let trimmed = family.trimmingCharacters(in: .whitespaces)
        fontField.stringValue = trimmed
        settings.updateDisplay { $0.fontFamily = trimmed }
        refreshFontWarning()
    }

    @objc private func fontCommitted(_ sender: Any?) { setFontFamily(fontField.stringValue) }

    func controlTextDidEndEditing(_ obj: Notification) { setFontFamily(fontField.stringValue) }
}
