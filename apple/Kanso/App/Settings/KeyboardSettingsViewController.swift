import AppKit

/// 改绑冲突时询问用户：替换（清空原命令的绑定）还是取消。
@MainActor
protocol KeybindingConflictPrompter: AnyObject {
    /// 返回 true 表示用户选择替换。
    func confirmReplace(chord: KeyChord, existing: KeyCommand, new: KeyCommand, window: NSWindow?) async -> Bool
}

/// 用 `NSAlert` 询问（sheet 附着在设置窗口上）。测试不要用它——会真的弹窗，注入自己的实现。
@MainActor
final class AlertKeybindingConflictPrompter: KeybindingConflictPrompter {
    func confirmReplace(chord: KeyChord, existing: KeyCommand, new: KeyCommand, window: NSWindow?) async -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "\(chord.display) is already in use")
        alert.informativeText = String(
            localized: "It is assigned to “\(existing.title)”. Replace it to use \(chord.display) for “\(new.title)” instead."
        )
        alert.addButton(withTitle: String(localized: "Replace"))
        alert.addButton(withTitle: String(localized: "Cancel"))
        let response: NSApplication.ModalResponse
        if let window {
            response = await alert.beginSheetModal(for: window)
        } else {
            response = alert.runModal()
        }
        return response == .alertFirstButtonReturn
    }
}

/// "键盘与鼠标"页（ui-shell.md §7、§7.1）：顶部是鼠标与选择设置，下面列出全部可改绑的命令。
///
/// 点击某行的按键框进入录制，按下新组合即绑定；Esc 取消，Delete 清空；与别的命令冲突时询问替换或取消；
/// "恢复默认快捷键"只恢复快捷键表，不动鼠标设置。⌘1–⌘9 显示但不可改。命令的绑定读写与生效都在 `KeybindingController`，这里只管界面。
@MainActor
final class KeyboardSettingsViewController: SettingsPageViewController {
    private let keybindings: KeybindingController
    private let prompter: KeybindingConflictPrompter

    private(set) var fields: [KeyCommand: KeyChordField] = [:]
    /// 固定的 ⌘1–⌘9 行（不可录制）。
    let fixedTabsField = KeyChordField()
    let restoreButton = KButton(title: String(localized: "Restore Default Shortcuts"), style: .secondary)
    let middleClickPopUp = KPopUpField()
    let rightClickPopUp = KPopUpField()
    let copyOnSelectRow = KCheckRow(title: String(localized: "Copy on Select"))
    private let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// 最近一次录制事件的处理任务；只为让测试能等待结果（冲突询问是异步的）。
    private(set) var pendingTask: Task<Void, Never>?

    init(keybindings: KeybindingController, prompter: KeybindingConflictPrompter = AlertKeybindingConflictPrompter()) {
        self.keybindings = keybindings
        self.prompter = prompter
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KeyboardSettingsViewController 只支持代码创建") }

    override func loadView() {
        let root = StyledBoxView()
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0

        // 滚动区：每个分组一张卡片。
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = DesignTokens.Spacing.lg
        for card in makeMouseCards() {
            stack.addArrangedSubview(card)
            card.translatesAutoresizingMaskIntoConstraints = false
            card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        for section in KeyCommandSection.allCases {
            let card = KCard(title: section.title, symbol: section.symbol)
            for command in KeyCommand.allCases where command.section == section {
                card.addRow(makeRow(for: command))
            }
            if section == .session { card.addRow(makeFixedTabsRow()) }
            stack.addArrangedSubview(card)
            card.translatesAutoresizingMaskIntoConstraints = false
            card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        let document = FlippedView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(stack)
        let inset = Self.inset
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: document.topAnchor, constant: inset),
            stack.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -DesignTokens.Spacing.md),
            stack.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: inset),
            stack.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -inset),
        ])
        let scroll = makeScrollView(document: document)

        // 底部：提示 / 错误 + 恢复默认。
        messageLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        messageLabel.maximumNumberOfLines = 3
        messageLabel.preferredMaxLayoutWidth = Self.pageWidth - inset * 2 - 160
        messageLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        messageLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        restoreButton.setContentHuggingPriority(.required, for: .horizontal)
        restoreButton.onPress = { [weak self] in self?.restoreDefaults() }
        let footer = NSStackView(views: [messageLabel, restoreButton])
        footer.orientation = .horizontal
        footer.alignment = .centerY
        footer.spacing = DesignTokens.Spacing.md
        footer.edgeInsets = NSEdgeInsets(top: 12, left: inset, bottom: 12, right: inset)
        let divider = DividerView()

        let column = NSStackView(views: [scroll, divider, footer])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(column)
        NSLayoutConstraint.activate([
            root.widthAnchor.constraint(equalToConstant: Self.pageWidth),
            root.heightAnchor.constraint(equalToConstant: Self.maxPageHeight),
            column.topAnchor.constraint(equalTo: root.topAnchor),
            column.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            column.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.widthAnchor.constraint(equalTo: column.widthAnchor),
            divider.widthAnchor.constraint(equalTo: column.widthAnchor),
            footer.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        view = root
        showHint()
        reload()
        // 编辑菜单的"选中即复制"、"右键弹出菜单"改的是同一份 mouse 设置，这里要同步控件。
        keybindings.settings.observe(self) { [weak self] section in
            if section == .keybindings { self?.reload() }
            if section == .mouse { self?.reloadMouse() }
        }
    }

    // MARK: 构建

    private func makeMouseCards() -> [KCard] {
        let settings = keybindings.settings
        middleClickPopUp.setItems([String(localized: "Paste Clipboard Contents"), String(localized: "No Action")])
        middleClickPopUp.setAccessibilityLabel(String(localized: "Middle Button"))
        middleClickPopUp.onChange = { index in
            settings.updateMouse { $0.middleClick = MiddleClickAction.allCases[index] }
        }
        rightClickPopUp.setItems([String(localized: "Show Menu"), String(localized: "Copy or Paste")])
        rightClickPopUp.setAccessibilityLabel(String(localized: "Right Button"))
        rightClickPopUp.onChange = { index in
            settings.updateMouse { $0.rightClick = RightClickAction.allCases[index] }
        }
        let mouseCard = KCard(title: String(localized: "Mouse"), symbol: "computermouse")
        mouseCard.addRow(LabeledField(title: String(localized: "Middle Button"), control: middleClickPopUp))
        mouseCard.addRow(LabeledField(title: String(localized: "Right Button"), control: rightClickPopUp))
        mouseCard.addRow(Self.caption(String(
            localized: "When a program enables mouse reporting (vim, tmux), hold Shift to let the terminal handle clicks."
        )))

        copyOnSelectRow.onToggle = { on in settings.updateMouse { $0.copyOnSelect = on } }
        let selectionCard = KCard(title: String(localized: "Selection"), symbol: "selection.pin.in.out")
        selectionCard.addRow(copyOnSelectRow)
        selectionCard.addRow(Self.caption(String(
            localized: "Copies the selection to the clipboard when you release the mouse."
        )))
        return [mouseCard, selectionCard]
    }

    private func makeRow(for command: KeyCommand) -> NSView {
        let field = KeyChordField()
        field.onBeginRecording = { [weak self] in
            self?.cancelOtherRecordings(except: command)
            self?.showHint()
        }
        field.onEvent = { [weak self] event in
            guard let self else { return }
            pendingTask = Task { @MainActor [weak self] in await self?.handle(event, for: command) }
        }
        fields[command] = field
        return Self.row(title: command.title, field: field)
    }

    private func makeFixedTabsRow() -> NSView {
        fixedTabsField.isEnabled = false
        fixedTabsField.fixedText = "⌘1–⌘9"
        return Self.row(title: String(localized: "Go to Tab 1–9"), field: fixedTabsField)
    }

    private static func row(title: String, field: KeyChordField) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.textColor = .token(DesignTokens.Color.textPrimary)
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(249), for: .horizontal)
        field.translatesAutoresizingMaskIntoConstraints = false
        field.widthAnchor.constraint(equalToConstant: 148).isActive = true
        let row = NSStackView(views: [label, spacer, field])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = DesignTokens.Spacing.sm
        return row
    }

    /// 弹出菜单项顺序与 `MiddleClickAction` / `RightClickAction` 的 `allCases` 顺序一致。
    private func reloadMouse() {
        let mouse = keybindings.settings.mouse
        middleClickPopUp.selectItem(at: MiddleClickAction.allCases.firstIndex(of: mouse.middleClick) ?? 0)
        rightClickPopUp.selectItem(at: RightClickAction.allCases.firstIndex(of: mouse.rightClick) ?? 0)
        copyOnSelectRow.isChecked = mouse.copyOnSelect
    }

    private func reload() {
        reloadMouse()
        for (command, field) in fields { field.chord = keybindings.chord(for: command) }
    }

    private func cancelOtherRecordings(except command: KeyCommand) {
        for (other, field) in fields where other != command { field.cancelRecording() }
    }

    // MARK: 动作

    /// 处理一次录制结果。冲突时先询问用户；返回后界面与设置已更新。
    func handle(_ event: KeyChordField.Event, for command: KeyCommand) async {
        switch event {
        case .cancel:
            showHint()
        case .clear:
            keybindings.clear(command)
            showHint()
        case .chord(let chord):
            switch keybindings.assign(chord, to: command) {
            case .assigned:
                showHint()
            case .conflict(let other):
                let replace = await prompter.confirmReplace(chord: chord, existing: other, new: command, window: view.window)
                if replace { _ = keybindings.assignReplacingConflict(chord, to: command) }
                showHint()
            case .reserved:
                showError(String(localized: "\(chord.display) is reserved by the system or fixed, and can’t be assigned."))
            case .invalid:
                showError(String(localized: "A shortcut needs ⌘, ⌃ or ⌥ — or a modifier together with a function key."))
            }
        }
        // 绑定未变化（取消、被拒、放弃替换）时设置不会通知观察者，这里显式同步一次，
        // 让录制框从"按下快捷键…"回到当前值。
        reload()
    }

    /// "恢复默认快捷键"：恢复整张快捷键表（ui-shell.md §7），不动鼠标设置。
    func restoreDefaults() {
        keybindings.restoreDefaults()
        showHint()
    }

    private func showHint() {
        messageLabel.textColor = .token(DesignTokens.Color.textTertiary)
        messageLabel.stringValue = String(localized: "Click a shortcut, then press the new keys. Esc cancels, Delete clears.")
    }

    private func showError(_ text: String) {
        messageLabel.textColor = .token(DesignTokens.Color.statusWarning)
        messageLabel.stringValue = text
    }

    /// 当前底部提示文字；internal 供测试。
    var messageText: String { messageLabel.stringValue }
}
