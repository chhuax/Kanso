import AppKit
import KansoBridge
import KansoUI

/// 会话编辑对话框的内容（visual-style §3.3）。新建与编辑共用；逻辑都在 `SessionEditorModel`，这里只管控件与布局。
///
/// 结构：对话框外壳（头部 / 可滚动主体 / 底部按钮）> "会话"卡片 + "SSH 连接"或"本地 Shell"卡片。
/// 字段显隐完全由 `SessionEditorModel.isVisible` 决定，控件只创建一次、按需显隐。
@MainActor
final class SessionEditorViewController: NSViewController, NSTextFieldDelegate {
    /// 对话框关闭（保存成功或取消）时调用；由呈现方负责收起 sheet。
    var onFinish: (() -> Void)?
    /// 点"连接"且保存成功后调用，参数是会话 id；晚于 `onFinish`（先收起对话框，再开会话，
    /// 避免连接过程中的凭据提示 sheet 与编辑 sheet 叠在一起）。保存失败不会调用。
    var onLocalShellAction: ((FfiLocalProfile, Bool) throws -> Void)?
    private let customLocal: Bool
    private let kinds: [SessionKindChoice]
    var onConnect: ((String) -> Void)?

    static let width: CGFloat = 560

    /// 对话框允许的最大高度（呈现方按主窗口高度设置）；主体内容更高时主体滚动。
    var maxHeight: CGFloat = 680 {
        didSet { if isViewLoaded { resizeSheet() } }
    }

    private let model: SessionEditorModel

    let kindCards: KSegmentedCards
    let nameField = KTextField()
    let colorPalette = ColorPaletteView()
    let groupPopup = KPopUpField()
    let mcpPopup = KPopUpField()
    let mcpHint = NSTextField(wrappingLabelWithString: McpText.sensitiveNotice)
    let shellField = KTextField()
    let startDirField = KTextField()
    let hostField = KTextField()
    let portField = KTextField()
    let userField = KTextField()
    let authPopup = KPopUpField()
    let passwordField = KTextField(secure: true)
    let keyPathField = KTextField()
    let keyPassphraseField = KTextField(secure: true)
    let jumpPopup = KPopUpField()
    let proxyField = KTextField()
    let encodingPopup = KPopUpField()
    let localeField = KTextField()
    let keepaliveCheck = KCheckRow(title: String(localized: "Protocol Keepalive"))
    let keepaliveIntervalField = KTextField()
    let keepaliveMaxMissesField = KTextField()
    let idleCheck = KCheckRow(title: String(localized: "Idle Keepalive"))
    let idleIntervalField = KTextField()
    let idleStringField = KTextField()

    let cancelButton = KButton(title: String(localized: "Cancel"), style: .secondary)
    let saveButton = KButton(title: String(localized: "Save"), symbol: "square.and.arrow.down", style: .secondary)
    let connectButton = KButton(title: String(localized: "Connect"), symbol: "powerplug", style: .primary)

    private(set) var shell: DialogShellView!
    private let sessionCard = KCard(title: String(localized: "Session"), symbol: "tag")
    private let sshCard = KCard(title: String(localized: "SSH Connection"), symbol: "server.rack")
    private let localCard = KCard(title: String(localized: "Local Shell"), symbol: "terminal")
    /// 编码 | Locale 这一行只有一份控件：本地会话放在"本地 Shell"卡片，SSH 放在"SSH 连接"卡片。
    private var encodingRow: NSStackView!
    private var pairRows: [NSStackView] = []
    private var fieldViews: [SessionEditorField: NSView] = [:]
    private var isSaving = false

    init(model: SessionEditorModel, customLocal: Bool = false, remoteOnly: Bool = false) {
        self.model = model
        self.customLocal = customLocal
        kinds = remoteOnly ? [.ssh, .sftp] : SessionKindChoice.allCases
        kindCards = KSegmentedCards(segments: kinds.map { .init(title: $0.title, symbol: $0.symbol) })
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SessionEditorViewController 只支持代码创建") }

    override func loadView() {
        let title = customLocal ? String(localized: "Customize Launch") : model.isEditing ? String(localized: "Edit Session") : String(localized: "New Session")
        let shell = DialogShellView(title: title, symbol: "server.rack", width: Self.width)
        self.shell = shell
        view = shell
        buildControls()
        buildCards()
        shell.bodyStack.addArrangedSubview(sessionCard)
        shell.bodyStack.addArrangedSubview(localCard)
        shell.bodyStack.addArrangedSubview(sshCard)
        for card in [sessionCard, localCard, sshCard] {
            card.translatesAutoresizingMaskIntoConstraints = false
            card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        }

        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.onPress = { [weak self] in self?.cancel() }
        // 回车 = 保存（与改版前一致）；"连接"是显式的第二个动作，不抢默认键。
        saveButton.keyEquivalent = "\r"
        if customLocal {
            saveButton.title = String(localized: "Set as Default")
            connectButton.title = String(localized: "Open")
        }
        saveButton.onPress = { [weak self] in self?.save() }
        connectButton.onPress = { [weak self] in self?.connect() }
        shell.setFooterButtons([cancelButton, saveButton, connectButton])
        shell.onClose = { [weak self] in self?.cancel() }

        loadValues()
        applyVisibility()
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        view.window?.makeFirstResponder(customLocal ? shellField.textField : nameField.textField)
    }

    // MARK: - 构建

    private func buildControls() {
        for field in [nameField, shellField, startDirField, hostField, portField, userField, passwordField,
                      keyPathField, keyPassphraseField, proxyField, localeField,
                      keepaliveIntervalField, keepaliveMaxMissesField, idleIntervalField, idleStringField] {
            field.textField.delegate = self
        }
        // 占位文字用示例值，不写成说明句（visual-style §3.1）。
        shellField.placeholder = "/bin/zsh"
        startDirField.placeholder = "~"
        hostField.placeholder = "example.com"
        portField.placeholder = String(SessionEditorModel.defaultPort)
        userField.placeholder = "user"
        keyPathField.placeholder = "~/.ssh/id_ed25519"
        localeField.placeholder = String(localized: "Automatic")
        proxyField.placeholder = "ssh -W %h:%p gateway"
        keepaliveIntervalField.placeholder = "30"
        keepaliveMaxMissesField.placeholder = "3"
        idleIntervalField.placeholder = "3600"
        idleStringField.placeholder = "\\r"
        // 编辑已保存的会话时，core 不会把密码回传；留空表示保持不变。
        let keepHint = String(localized: "Leave empty to keep the saved value")
        passwordField.placeholder = model.isEditing ? keepHint : ""
        keyPassphraseField.placeholder = model.isEditing ? keepHint : ""

        kindCards.onSelect = { [weak self] index in
            guard let self else { return }
            model.kind = kinds[index]
            applyVisibility()
        }

        authPopup.setItems([String(localized: "Password"), String(localized: "Private Key"), String(localized: "ssh-agent")])
        authPopup.onChange = { [weak self] index in
            self?.model.auth = Self.authOrder[index]
            self?.applyVisibility()
        }

        groupPopup.setItems([String(localized: "No Group")] + model.groups.map(\.name))
        groupPopup.onChange = { [weak self] index in
            guard let self else { return }
            model.groupID = index == 0 ? nil : model.groups[index - 1].id
        }

        mcpPopup.setItems(McpText.accessLevels.map(McpText.accessTitle))
        mcpPopup.onChange = { [weak self] index in
            guard let self else { return }
            let next = McpText.accessLevels[index]
            if model.mcpAccess == .off, next != .off { mcpHint.isHidden = false }
            model.mcpAccess = next
        }
        mcpHint.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        mcpHint.textColor = .token(DesignTokens.Color.textTertiary)
        mcpHint.preferredMaxLayoutWidth = Self.width - 80
        mcpHint.isHidden = true

        jumpPopup.setItems([String(localized: "None")] + model.jumpCandidates.map(\.name))
        jumpPopup.onChange = { [weak self] index in
            guard let self else { return }
            model.jumpID = index == 0 ? nil : model.jumpCandidates[index - 1].id
            applyVisibility()
        }

        encodingPopup.setItems(model.encodingOptions.map(\.title))
        encodingPopup.onChange = { [weak self] index in
            guard let self else { return }
            model.encoding = model.encodingOptions[index].label
        }

        // 色板里不放"无"：再次点击已选中的色点即取消选择（见 ColorPaletteView）。
        colorPalette.setColors(model.colorOptions.compactMap { $0 })
        colorPalette.onSelect = { [weak self] rgb in self?.model.color = rgb }
        colorPalette.toolTip = String(localized: "Click the selected color again to clear it")

        keepaliveCheck.onToggle = { [weak self] on in self?.model.keepaliveEnabled = on }
        idleCheck.onToggle = { [weak self] on in self?.model.idleEnabled = on }
    }

    private func buildCards() {
        func labeled(_ field: SessionEditorField, _ title: String, _ control: NSView) -> LabeledField {
            let labeled = LabeledField(title: title, control: control)
            fieldViews[field] = labeled
            return labeled
        }
        func pair(_ left: NSView, _ right: NSView) -> NSStackView {
            let row = KCard.pair(left, right)
            pairRows.append(row)
            return row
        }
        func withButton(_ field: KTextField, action: @escaping () -> Void) -> NSView {
            let button = KButton(title: String(localized: "Choose…"), style: .secondary)
            button.onPress = action
            // 按钮保持固有宽度，输入框吃掉剩余宽度。
            button.setContentHuggingPriority(.required, for: .horizontal)
            button.setContentCompressionResistancePriority(.required, for: .horizontal)
            let row = NSStackView(views: [field, button])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = DesignTokens.Spacing.sm
            return row
        }

        sessionCard.addRow(labeled(.kind, String(localized: "Type"), kindCards))
        sessionCard.addRow(pair(labeled(.name, String(localized: "Name"), nameField),
                                labeled(.color, String(localized: "Color"), colorPalette)))
        sessionCard.addRow(labeled(.group, String(localized: "Group"), groupPopup))
        let mcpColumn = NSStackView(views: [mcpPopup, mcpHint])
        mcpColumn.orientation = .vertical
        mcpColumn.alignment = .leading
        mcpColumn.spacing = 6
        sessionCard.addRow(labeled(.mcpAccess, String(localized: "MCP access"), mcpColumn))

        localCard.addRow(labeled(.shellCommand, String(localized: "Shell Command"), shellField))
        localCard.addRow(labeled(.startDir, String(localized: "Start Directory"),
                                 withButton(startDirField) { [weak self] in self?.chooseStartDir() }))

        sshCard.addRow(pair(labeled(.host, String(localized: "Host"), hostField), labeled(.port, String(localized: "Port"), portField)))
        sshCard.addRow(pair(labeled(.user, String(localized: "User"), userField),
                            labeled(.auth, String(localized: "Authentication"), authPopup)))
        sshCard.addRow(labeled(.password, String(localized: "Password"), passwordField))
        sshCard.addRow(labeled(.keyPath, String(localized: "Private Key"),
                               withButton(keyPathField) { [weak self] in self?.chooseKey() }))
        sshCard.addRow(labeled(.keyPassphrase, String(localized: "Key Passphrase"), keyPassphraseField))
        sshCard.addRow(labeled(.jump, String(localized: "Jump Host"), jumpPopup))
        sshCard.addRow(labeled(.proxyCommand, String(localized: "ProxyCommand"), proxyField))
        fieldViews[.keepaliveEnabled] = keepaliveCheck
        sshCard.addRow(keepaliveCheck)
        sshCard.addRow(pair(labeled(.keepaliveInterval, String(localized: "Interval (s)"), keepaliveIntervalField),
                            labeled(.keepaliveMaxMisses, String(localized: "Misses"), keepaliveMaxMissesField)))
        fieldViews[.idleEnabled] = idleCheck
        sshCard.addRow(idleCheck)
        sshCard.addRow(pair(labeled(.idleInterval, String(localized: "Idle Interval (s)"), idleIntervalField),
                            labeled(.idleString, String(localized: "Idle String"), idleStringField)))

        encodingRow = pair(labeled(.encoding, String(localized: "Encoding"), encodingPopup),
                           labeled(.locale, String(localized: "Locale"), localeField))
    }

    // MARK: - 模型 <-> 控件

    private func loadValues() {
        nameField.stringValue = model.name
        kindCards.selectedIndex = kinds.firstIndex(of: model.kind) ?? 0
        colorPalette.selected = model.color
        groupPopup.selectItem(at: model.groupID.flatMap { id in model.groups.firstIndex { $0.id == id } }.map { $0 + 1 } ?? 0)
        mcpPopup.selectItem(at: McpText.accessIndex(model.mcpAccess))
        mcpPopup.isEnabled = model.isMcpAccessEnabled
        if let reason = model.mcpAccessLoadError {
            mcpHint.stringValue = String(localized: "Could not read the MCP access level: \(reason)")
            mcpHint.isHidden = false
        }
        shellField.stringValue = model.shellCommand
        startDirField.stringValue = model.startDir
        hostField.stringValue = model.host
        portField.stringValue = model.portText
        userField.stringValue = model.user
        authPopup.selectItem(at: Self.authOrder.firstIndex(of: model.auth) ?? 0)
        keyPathField.stringValue = model.keyPath
        jumpPopup.selectItem(at: model.jumpID.flatMap { id in model.jumpCandidates.firstIndex { $0.id == id } }.map { $0 + 1 } ?? 0)
        proxyField.stringValue = model.proxyCommand
        encodingPopup.selectItem(at: model.encodingOptions.firstIndex { $0.label == model.encoding } ?? 0)
        localeField.stringValue = model.locale
        keepaliveCheck.isChecked = model.keepaliveEnabled
        keepaliveIntervalField.stringValue = model.keepaliveIntervalText
        keepaliveMaxMissesField.stringValue = model.keepaliveMaxMissesText
        idleCheck.isChecked = model.idleEnabled
        idleIntervalField.stringValue = model.idleIntervalText
        idleStringField.stringValue = model.idleString
    }

    private static let authOrder: [FfiAuthMethod] = [.password, .privateKey, .agent]

    /// 文本框内容写回模型。每次编辑都整体同步：字段不多，比逐个绑定简单，也不会漏。
    private func syncTextToModel() {
        model.name = nameField.stringValue
        model.shellCommand = shellField.stringValue
        model.startDir = startDirField.stringValue
        model.host = hostField.stringValue
        let port = SessionEditorModel.sanitizedPortText(portField.stringValue)
        if port != portField.stringValue { portField.stringValue = port }
        model.portText = port
        model.user = userField.stringValue
        model.password = passwordField.stringValue
        model.keyPath = keyPathField.stringValue
        model.keyPassphrase = keyPassphraseField.stringValue
        model.proxyCommand = proxyField.stringValue
        model.locale = localeField.stringValue
        model.keepaliveEnabled = keepaliveCheck.isChecked
        let keepaliveInterval = SessionEditorModel.sanitizedPortText(keepaliveIntervalField.stringValue)
        if keepaliveInterval != keepaliveIntervalField.stringValue { keepaliveIntervalField.stringValue = keepaliveInterval }
        model.keepaliveIntervalText = keepaliveInterval
        let keepaliveMaxMisses = SessionEditorModel.sanitizedPortText(keepaliveMaxMissesField.stringValue)
        if keepaliveMaxMisses != keepaliveMaxMissesField.stringValue { keepaliveMaxMissesField.stringValue = keepaliveMaxMisses }
        model.keepaliveMaxMissesText = keepaliveMaxMisses
        model.idleEnabled = idleCheck.isChecked
        let idleInterval = SessionEditorModel.sanitizedPortText(idleIntervalField.stringValue)
        if idleInterval != idleIntervalField.stringValue { idleIntervalField.stringValue = idleInterval }
        model.idleIntervalText = idleInterval
        model.idleString = idleStringField.stringValue
    }

    func controlTextDidChange(_ obj: Notification) {
        syncTextToModel()
        // 两项互斥：ProxyCommand 有内容时禁用跳板机选择。
        applyVisibility()
    }

    // MARK: - 可见性与高度

    /// 按模型显隐字段、卡片，并重新计算对话框高度。
    func applyVisibility() {
        for (field, view) in fieldViews { view.isHidden = !model.isVisible(field) }
        sessionCard.isHidden = customLocal
        saveButton.isHidden = model.opensTemporaryShell && !customLocal
        saveButton.keyEquivalent = model.opensTemporaryShell ? "" : "\r"
        connectButton.keyEquivalent = model.opensTemporaryShell ? "\r" : ""
        localCard.isHidden = model.kind != .local || (model.opensTemporaryShell && !customLocal)
        sshCard.isHidden = !model.isSSHLike

        // 编码 | Locale 只有一份控件，跟着类型换卡片。
        let host: KCard? = switch model.kind {
        case .local: localCard
        case .ssh: sshCard
        case .sftp: nil
        }
        if let host, encodingRow.superview !== host.contentStack {
            // 先摘下再加：旧卡片里的约束随 removeFromSuperview 一并失效。
            encodingRow.removeFromSuperview()
            host.addRow(encodingRow)
        }
        // 成对的行：两个字段都隐藏时整行隐藏，否则父栈会为一个空行保留间距。
        for row in pairRows { row.isHidden = row.arrangedSubviews.allSatisfy(\.isHidden) }

        proxyField.isEnabled = model.isProxyCommandEnabled
        jumpPopup.isEnabled = model.isJumpEnabled
        shell.setMessage(model.errorText)
        resizeSheet()
    }

    /// 显隐字段后按内容重新计算对话框高度；宽度固定。
    private func resizeSheet() {
        let height = shell.fittingHeight(maxHeight: maxHeight)
        let size = NSSize(width: Self.width, height: height)
        preferredContentSize = size
        if let window = view.window as? DialogSheetWindow {
            window.resize(toContentSize: size)
        } else {
            view.window?.setContentSize(size)
        }
    }

// MARK: - 选择器

    @objc private func chooseStartDir() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = startDirField.stringValue.isEmpty ? nil : URL(fileURLWithPath: startDirField.stringValue)
        present(panel) { [weak self] url in
            self?.startDirField.stringValue = url.path
            self?.syncTextToModel()
        }
    }

    @objc private func chooseKey() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        // 私钥通常放在 ~/.ssh，路径与文件名常以点开头，必须显示隐藏文件。
        panel.showsHiddenFiles = true
        panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".ssh", isDirectory: true)
        present(panel) { [weak self] url in
            self?.keyPathField.stringValue = url.path
            self?.syncTextToModel()
        }
    }

    private func present(_ panel: NSOpenPanel, completion: @escaping (URL) -> Void) {
        let handler: (NSApplication.ModalResponse) -> Void = { response in
            if response == .OK, let url = panel.url { completion(url) }
        }
        if let window = view.window { panel.beginSheetModal(for: window, completionHandler: handler) } else { panel.begin(completionHandler: handler) }
    }

    // MARK: - 保存 / 取消 / 连接

    private func cancel() { onFinish?() }

    private func save() { performSave(thenConnect: false) }

    private func connect() { performSave(thenConnect: true) }

    /// 保存；`thenConnect` 为真时保存成功后再通知 `onConnect`。
    /// 返回的 Task 只为让测试能等待结果，界面代码不需要关心。
    @discardableResult
    func performSave(thenConnect: Bool) -> Task<Void, Never> {
        Task { @MainActor in
            // 保存中再点一次直接忽略：save 是异步的（写凭据），重复触发会产生重复写入。
            guard !isSaving else { return }
            if customLocal {
                syncTextToModel()
                guard let profile = model.assemble(), case .local(let local) = profile.kind else { return }
                do {
                    try onLocalShellAction?(local, thenConnect)
                    if thenConnect { onFinish?() }
                    else { shell.setMessage(String(localized: "Saved as Default")) }
                } catch {
                    shell.setMessage(CoreMessageText.text(for: error))
                }
                resizeSheet()
                return
            }
            syncTextToModel()
            isSaving = true
            saveButton.isEnabled = false
            connectButton.isEnabled = false
            let outcome = await model.save()
            isSaving = false
            saveButton.isEnabled = true
            connectButton.isEnabled = true
            if outcome == .saved {
                onFinish?()
                if thenConnect { onConnect?(model.profileID) }
            } else {
                // 失败不关闭对话框：错误显示在底部，用户改完可以直接再保存 / 连接。
                applyVisibility()
            }
        }
    }
}
