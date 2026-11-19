import AppKit
import KansoBridge
import KansoUI

/// 设置 → MCP。是否出现这一页只由 `McpCoordinator.supported` 决定（见 `SettingsWindowController`），不看服务状态。
///
/// 页面只管总开关、接入配置和已连接的客户端；会话授权在会话启动器与编辑对话框里，活动记录在独立窗口
/// （spec/mcp.md §7）。页面内容放在 `NSScrollView` 里，高度有上限；客户端多了就滚动。内容高度变化后
/// 调用 `relayout()` 同步页面高度和窗口高度。
@MainActor
final class McpSettingsViewController: SettingsPageViewController {
    let mcp: McpCoordinator
    var copyText: (String) -> Void = { text in
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    let enabledRow = KCheckRow(title: String(localized: "Enable MCP"))
    let statusLabel = NSTextField(wrappingLabelWithString: "")
    /// 操作失败（开关、重新生成令牌）的原因；没有错误时隐藏。
    let errorLabel = NSTextField(wrappingLabelWithString: "")
    let pathLabel = NSTextField(wrappingLabelWithString: "")
    let clientPopup = KPopUpField()
    let commandLabel = NSTextField(wrappingLabelWithString: "")
    let copyCommandButton = KButton(title: String(localized: "Copy Command"), style: .primary)
    let regenerateButton = KButton(title: String(localized: "Regenerate Token"), style: .secondary)
    /// 已授权会话数的只读说明（0 / 1 / 多个三种文案）；设置页不列出会话、不改级别。
    let grantedLabel = SettingsPageViewController.caption("")
    let openActivityButton = KButton(title: String(localized: "Open Activity Log"), style: .secondary)

    /// 读已授权会话的来源；默认是 core，测试替换成可失败的替身来覆盖读失败路径。
    var accessStore: any McpAccessStoring

    private let clientStack = NSStackView()
    private var deminiaturizeObserver: NSObjectProtocol?
    /// 客户端列表上次渲染时的内容；没变就不重建，避免每次事件都重排整页。
    private var renderedClients: [FfiMcpClientInfo]?
    private var contentView: NSView?
    private var heightConstraint: NSLayoutConstraint?

    init(mcp: McpCoordinator) {
        self.mcp = mcp
        accessStore = mcp.core
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) 不支持") }

    override func loadView() {
        enabledRow.onToggle = { [weak self] on in self?.toggleEnabled(on) }
        // spec/mcp.md §2.5 / §7.1：tmux / screen 盲区放在总开关的悬停提示里，不占用常显的说明行。
        enabledRow.toolTip = String(
            localized: "If an AI tool runs inside tmux or screen in a local tab, Kanso cannot tell which tab it is in."
        )

        statusLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        statusLabel.textColor = .token(DesignTokens.Color.textPrimary)
        pathLabel.font = .monospacedSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
        pathLabel.textColor = .token(DesignTokens.Color.textSecondary)
        pathLabel.stringValue = mcpHelperPath()
        errorLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        errorLabel.textColor = .token(DesignTokens.Color.statusError)
        errorLabel.isHidden = true

        copyCommandButton.onPress = { [weak self] in self?.copyConfig() }
        regenerateButton.onPress = { [weak self] in self?.regenerate() }
        openActivityButton.onPress = { [weak self] in self?.mcp.onShowActivity?() }

        let switchCard = KCard(title: String(localized: "MCP"), symbol: "point.3.connected.trianglepath.dotted")
        switchCard.addRow(enabledRow)
        switchCard.addRow(SettingsPageViewController.caption(String(
            localized: "When on, AI tools on this Mac can access the sessions you have opened to them."
        )))
        switchCard.addRow(grantedLabel)
        switchCard.addRow(buttonRow([openActivityButton]))

        let serviceCard = KCard(title: String(localized: "MCP service"), symbol: "antenna.radiowaves.left.and.right")
        serviceCard.addRow(statusLabel)
        serviceCard.addRow(pathLabel)
        serviceCard.addRow(errorLabel)

        let configCard = KCard(title: String(localized: "Configuration"), symbol: "doc.on.clipboard")
        clientPopup.setItems([String(localized: "Claude Code"), String(localized: "Codex")])
        clientPopup.selectItem(at: 0)
        clientPopup.onChange = { [weak self] _ in
            self?.reloadCommand()
            self?.relayout()
        }
        configCard.addRow(LabeledField(title: String(localized: "Client"), control: clientPopup))
        commandLabel.font = .monospacedSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
        commandLabel.textColor = .token(DesignTokens.Color.textPrimary)
        commandLabel.isSelectable = true
        commandLabel.maximumNumberOfLines = 0
        commandLabel.lineBreakMode = .byCharWrapping
        commandLabel.preferredMaxLayoutWidth =
            Self.pageWidth - Self.inset * 2 - KCard.padding * 2 - DesignTokens.Spacing.fieldPaddingX * 2
        let commandBox = StyledBoxView()
        commandBox.fillToken = DesignTokens.Color.bgField
        commandBox.borderToken = DesignTokens.Color.borderField
        commandBox.cornerRadius = DesignTokens.Radius.md
        commandLabel.translatesAutoresizingMaskIntoConstraints = false
        commandBox.addSubview(commandLabel)
        NSLayoutConstraint.activate([
            commandLabel.topAnchor.constraint(equalTo: commandBox.topAnchor, constant: 8),
            commandLabel.bottomAnchor.constraint(equalTo: commandBox.bottomAnchor, constant: -8),
            commandLabel.leadingAnchor.constraint(equalTo: commandBox.leadingAnchor, constant: DesignTokens.Spacing.fieldPaddingX),
            commandLabel.trailingAnchor.constraint(equalTo: commandBox.trailingAnchor, constant: -DesignTokens.Spacing.fieldPaddingX),
        ])
        configCard.addRow(commandBox)
        configCard.addRow(buttonRow([copyCommandButton, regenerateButton]))
        configCard.addRow(SettingsPageViewController.caption(String(
            localized: "The command contains a token and may be saved in shell history. If that is a concern, clear the history after running it, or regenerate the token."
        )))

        clientStack.orientation = .vertical
        clientStack.alignment = .leading
        clientStack.spacing = DesignTokens.Spacing.sm
        let clientsCard = KCard(title: String(localized: "Connected clients"), symbol: "person.2")
        clientsCard.addRow(clientStack)

        view = makeScrollingPage(makePageView([switchCard, serviceCard, configCard, clientsCard]))
        // MCP 事件只更新状态和客户端列表，且只在页面可见时做；重新显示时 `viewWillAppear` 会补一次。
        mcp.addChangeHandler { [weak self] change in
            guard let self, view.window?.isVisible == true else { return }
            refreshLive(for: change)
        }
        NotificationCenter.default.addObserver(
            forName: .sessionLibraryDidChange, object: mcp.core, queue: .main
        ) { [weak self] _ in
            // 不看页面是否可见：只是一次 `mcpAccesses()` 读取，授权变化又不频繁。
            MainActor.assumeIsolated {
                self?.reloadGrantedSummary()
                self?.relayout()
            }
        }
        reloadStatus()
        reloadTokenButtons()
        reloadClients()
        reloadGrantedSummary()
        mcp.refreshTokenCache()
    }

    override func viewWillAppear() {
        super.viewWillAppear()
        refreshLive(for: [.status, .clients])
        // 窗口最小化期间事件被跳过；恢复时不会再触发 viewWillAppear，要自己补一次。
        if let window = view.window {
            deminiaturizeObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didDeminiaturizeNotification, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshLive(for: [.status, .clients]) }
            }
        }
    }

    override func viewDidDisappear() {
        super.viewDidDisappear()
        if let deminiaturizeObserver { NotificationCenter.default.removeObserver(deminiaturizeObserver) }
        deminiaturizeObserver = nil
    }

    /// 把页面内容放进滚动区。页面高度取内容高度和 `maxPageHeight` 的较小值，由 `relayout()` 维护。
    private func makeScrollingPage(_ content: NSView) -> NSView {
        let root = StyledBoxView()
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0
        let document = FlippedView()
        content.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(content)
        let scroll = makeScrollView(document: document)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(scroll)
        let height = root.heightAnchor.constraint(equalToConstant: Self.maxPageHeight)
        NSLayoutConstraint.activate([
            root.widthAnchor.constraint(equalToConstant: Self.pageWidth),
            height,
            scroll.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            content.topAnchor.constraint(equalTo: document.topAnchor),
            content.bottomAnchor.constraint(equalTo: document.bottomAnchor),
            content.leadingAnchor.constraint(equalTo: document.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: document.trailingAnchor),
        ])
        contentView = content
        heightConstraint = height
        return root
    }

    /// 内容增减后重算页面高度并让设置窗口跟上。客户端列表、授权数说明变化后都要调用，
    /// 否则窗口高度停在旧值，内容会被裁掉或留空。
    func relayout() {
        guard let contentView, let heightConstraint else { return }
        contentView.layoutSubtreeIfNeeded()
        let fitted = contentView.fittingSize.height
        guard fitted > 0 else { return }
        let target = min(fitted, Self.maxPageHeight)
        // 高度没变就不去动窗口；事件频繁时这里是热路径。
        guard target != heightConstraint.constant else { return }
        heightConstraint.constant = target
        (view.window?.windowController as? SettingsWindowController)?.resizeToFitCurrentPage()
    }

    /// 按变化类型做局部更新。由页面可见时的 change handler 与 `viewWillAppear` 调用；
    /// 测试直接调用以绕开「窗口可见」的门槛。
    ///
    /// 只有 `.marks` 或 `.audit` 时什么都不做：设置页既不显示标签标记，也不显示活动记录。
    /// `.clients` / `.status` / `.other` 刷新状态、客户端、令牌按钮与已授权会话数
    /// （`viewWillAppear` 用 `[.status, .clients]` 补一次全量刷新，窗口最小化期间错过的事件由它补上）。
    func refreshLive(for change: McpChange) {
        guard !change.isDisjoint(with: [.clients, .status, .other]) else { return }
        reloadStatus()
        reloadClients()
        reloadTokenButtons()
        reloadGrantedSummary()
        relayout()
    }

    private func showError(_ text: String?) {
        errorLabel.stringValue = text ?? ""
        errorLabel.isHidden = text == nil
    }

    private func toggleEnabled(_ on: Bool) {
        showError(nil)
        do {
            try mcp.setEnabled(on)
        } catch {
            enabledRow.isChecked = mcp.core.mcpEnabled()
            // 用 errorLabel 而不是 statusLabel：后面的 reloadStatus 会重写状态文字，错误要留到下一次用户操作。
            showError(CoreMessageText.text(for: error))
        }
        reloadStatus()
        reloadTokenButtons()
        relayout()
    }

    private func reloadStatus() {
        enabledRow.isChecked = mcp.core.mcpEnabled()
        switch mcp.status {
        case .running:
            statusLabel.stringValue = String(localized: "Running")
        case .stopped:
            statusLabel.stringValue = String(localized: "Stopped")
        case .unavailable(let reason):
            statusLabel.stringValue = String(localized: "Unavailable") + "\n" + McpText.unavailableReason(reason)
        }
        // 开关已打开但启动阶段就失败（取令牌或 start 抛错）时，状态回调不一定会来，这里单独说明。
        if let reason = mcp.startError, case .stopped = mcp.status {
            statusLabel.stringValue = String(localized: "MCP is turned on, but the service did not start.") + "\n" + reason
        }
    }

    /// 按钮状态只看协调器的令牌缓存，不在页面路径上读钥匙串。
    private func reloadTokenButtons() {
        let hasToken = mcp.cachedToken != nil
        copyCommandButton.isEnabled = hasToken
        regenerateButton.isEnabled = hasToken
        reloadCommand()
    }

    private var selectedKind: FfiMcpClientKind {
        clientPopup.selectedIndex == 1 ? .codex : .claudeCode
    }

    private func reloadCommand() {
        // 显示和复制共用完整命令；令牌读取或轮换后在同一处刷新，避免复制到旧命令。
        let command = configurationCommand(selectedKind)
        // 状态事件不改变命令时，保留用户正在选择的文字。
        if commandLabel.stringValue != command { commandLabel.stringValue = command }
    }

    private func configurationCommand(_ kind: FfiMcpClientKind) -> String {
        guard let token = mcp.cachedToken else { return "" }
        return mcpClientConfig(kind: kind, helperPath: mcpHelperPath(), token: token)
    }

    private func copyConfig() {
        guard mcp.cachedToken != nil else { return }
        // 原样复制，保留开头的空格。
        copyText(configurationCommand(selectedKind))
    }

    private func regenerate() {
        showError(nil)
        do {
            try mcp.regenerateToken()
        } catch {
            showError(String(localized: "Could not regenerate the token. The old token is still valid. \(CoreMessageText.text(for: error))"))
        }
        reloadTokenButtons()
        relayout()
    }

    private func reloadClients() {
        guard renderedClients != mcp.clients else { return }
        renderedClients = mcp.clients
        for view in clientStack.arrangedSubviews { view.removeFromSuperview() }
        if mcp.clients.isEmpty {
            clientStack.addArrangedSubview(SettingsPageViewController.caption(String(localized: "No clients connected.")))
            return
        }
        for client in mcp.clients {
            let name = NSTextField(labelWithString: McpText.clientName(client.name))
            name.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
            name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            let when = NSTextField(labelWithString: McpText.timeText(unixMs: client.connectedAtMs))
            when.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            when.textColor = .token(DesignTokens.Color.textSecondary)
            let calls = NSTextField(labelWithString: String(localized: "\(Int(client.calls)) calls"))
            calls.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            calls.textColor = .token(DesignTokens.Color.textSecondary)
            let disconnect = KButton(title: String(localized: "Disconnect"), style: .secondary)
            let clientId = client.id
            disconnect.onPress = { [weak self] in self?.mcp.server.disconnect(clientId: clientId) }
            let row = NSStackView(views: [name, when, calls, disconnect])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = DesignTokens.Spacing.sm
            clientStack.addArrangedSubview(row)
        }
    }

    /// 刷新已授权会话数说明。一次 `mcpAccesses()` 读出全部非「关闭」的授权，不逐个会话查库。
    /// 读失败显示原因（错误色），不显示数量：显示「0 个」会把读取失败误导成「全部关闭」。
    func reloadGrantedSummary() {
        do {
            let count = try accessStore.mcpAccesses().count
            grantedLabel.stringValue = McpText.grantedSessionsSummary(count: count)
            grantedLabel.textColor = .token(DesignTokens.Color.textTertiary)
        } catch {
            grantedLabel.stringValue = String(localized: "Could not read MCP access: \(CoreMessageText.text(for: error))")
            grantedLabel.textColor = .token(DesignTokens.Color.statusError)
        }
    }

    private func buttonRow(_ buttons: [NSView]) -> NSStackView {
        let row = NSStackView(views: buttons)
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = DesignTokens.Spacing.sm
        return row
    }
}
