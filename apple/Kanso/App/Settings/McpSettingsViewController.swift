import AppKit
import KansoBridge
import KansoUI

/// 设置 → MCP。是否出现这一页只由 `McpCoordinator.supported` 决定（见 `SettingsWindowController`），不看服务状态。
///
/// 页面内容放在 `NSScrollView` 里，高度有上限；会话、客户端、审计记录多了就滚动。内容高度变化后
/// 调用 `relayout()` 同步页面高度和窗口高度。
@MainActor
final class McpSettingsViewController: SettingsPageViewController, NSTextFieldDelegate {
    let mcp: McpCoordinator
    /// 授权读写用的存储，默认就是 `mcp.core`；测试注入可失败的替身。
    private let accessStore: any McpAccessStoring
    var copyText: (String) -> Void = { text in
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    let enabledRow = KCheckRow(title: String(localized: "Enable MCP"))
    let statusLabel = NSTextField(wrappingLabelWithString: "")
    /// 操作失败（授权写入、重新生成令牌）的原因；没有错误时隐藏。
    let errorLabel = NSTextField(wrappingLabelWithString: "")
    let pathLabel = NSTextField(wrappingLabelWithString: "")
    let copyClaudeButton = KButton(title: String(localized: "Copy Claude Code Configuration"), style: .secondary)
    let copyCodexButton = KButton(title: String(localized: "Copy Codex Configuration"), style: .secondary)
    let copyTokenButton = KButton(title: String(localized: "Copy Token"), style: .secondary)
    let regenerateButton = KButton(title: String(localized: "Regenerate"), style: .secondary)
    let sensitiveNotice = SettingsPageViewController.caption(McpText.sensitiveNotice)
    let auditClientField = KTextField()
    let auditSessionPopup = KPopUpField()
    let errorsOnlyRow = KCheckRow(title: String(localized: "Errors only"))
    let auditMoreButton = KButton(title: String(localized: "More"), style: .secondary)
    let auditClearButton = KButton(title: String(localized: "Clear"), style: .secondary)

    private let clientStack = NSStackView()
    private let grantStack = NSStackView()
    private let auditStack = NSStackView()
    private var levelPopups: [String: KPopUpField] = [:]
    private var groupPopups: [String: KPopUpField] = [:]
    /// 读取访问级别失败的行的原因文字（行里下拉框同时被禁用）。
    private(set) var levelErrorLabels: [String: NSTextField] = [:]
    private var auditRecords: [FfiMcpAuditRecord] = []
    private var auditSessionIDs: [String?] = []
    /// 上次提交（reset）时的筛选条件。翻页必须沿用它：用户可能在输入框里改了字但还没提交，
    /// 此时「更多」读输入框会让后一页和已显示的前几页来自不同条件。
    private var committedFilter: (client: String?, profileId: String?, errorsOnly: Bool) = (nil, nil, false)
    /// 读取或清空审计失败的原因；非 nil 时审计区显示它而不是记录列表。
    private var auditError: String?
    private let auditPageSize: UInt32 = 20

    private var deminiaturizeObserver: NSObjectProtocol?
    /// 客户端列表上次渲染时的内容；没变就不重建，避免每次事件都重排整页。
    private var renderedClients: [FfiMcpClientInfo]?
    private var contentView: NSView?
    private var heightConstraint: NSLayoutConstraint?

    init(mcp: McpCoordinator, accessStore: (any McpAccessStoring)? = nil) {
        self.mcp = mcp
        self.accessStore = accessStore ?? mcp.core
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) 不支持") }

    func levelPopup(for profileId: String) -> KPopUpField? { levelPopups[profileId] }
    func groupPopup(for groupId: String?) -> KPopUpField? { groupPopups[groupId ?? ""] }

    override func loadView() {
        enabledRow.onToggle = { [weak self] on in self?.toggleEnabled(on) }
        sensitiveNotice.isHidden = true

        statusLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        statusLabel.textColor = .token(DesignTokens.Color.textPrimary)
        pathLabel.font = .monospacedSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
        pathLabel.textColor = .token(DesignTokens.Color.textSecondary)
        pathLabel.stringValue = mcpHelperPath()
        errorLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        errorLabel.textColor = .token(DesignTokens.Color.statusError)
        errorLabel.isHidden = true

        copyClaudeButton.onPress = { [weak self] in self?.copyConfig(.claudeCode) }
        copyCodexButton.onPress = { [weak self] in self?.copyConfig(.codex) }
        copyTokenButton.onPress = { [weak self] in self?.copyToken() }
        regenerateButton.onPress = { [weak self] in self?.regenerate() }

        let switchCard = KCard(title: String(localized: "MCP"), symbol: "point.3.connected.trianglepath.dotted")
        switchCard.addRow(enabledRow)
        switchCard.addRow(SettingsPageViewController.caption(String(
            localized: "When on, AI tools on this Mac can access the sessions authorized below."
        )))
        switchCard.addRow(SettingsPageViewController.caption(String(
            localized: "If an AI tool runs inside tmux or screen in a local tab, Kanso cannot tell which tab it is in."
        )))

        let serviceCard = KCard(title: String(localized: "MCP service"), symbol: "antenna.radiowaves.left.and.right")
        serviceCard.addRow(statusLabel)
        serviceCard.addRow(pathLabel)
        serviceCard.addRow(errorLabel)

        let configCard = KCard(title: String(localized: "Configuration"), symbol: "doc.on.clipboard")
        configCard.addRow(buttonRow([copyClaudeButton, copyCodexButton]))
        configCard.addRow(SettingsPageViewController.caption(String(
            localized: "The command contains a token and may be saved in shell history. If that is a concern, clear the history after running it, or regenerate the token."
        )))
        configCard.addRow(LabeledField(title: String(localized: "Client token"), control: buttonRow([copyTokenButton, regenerateButton])))

        clientStack.orientation = .vertical
        clientStack.alignment = .leading
        clientStack.spacing = DesignTokens.Spacing.sm
        let clientsCard = KCard(title: String(localized: "Connected clients"), symbol: "person.2")
        clientsCard.addRow(clientStack)

        grantStack.orientation = .vertical
        grantStack.alignment = .leading
        grantStack.spacing = DesignTokens.Spacing.sm
        let grantsCard = KCard(title: String(localized: "Session access"), symbol: "lock")
        grantsCard.addRow(sensitiveNotice)
        grantsCard.addRow(grantStack)

        auditClientField.placeholder = String(localized: "Client")
        auditClientField.textField.delegate = self
        errorsOnlyRow.onToggle = { [weak self] _ in self?.reloadAudit(reset: true) }
        auditSessionPopup.onChange = { [weak self] _ in self?.reloadAudit(reset: true) }
        auditMoreButton.onPress = { [weak self] in self?.reloadAudit(reset: false) }
        auditClearButton.onPress = { [weak self] in self?.clearAudit() }
        auditStack.orientation = .vertical
        auditStack.alignment = .leading
        auditStack.spacing = DesignTokens.Spacing.xs
        let auditCard = KCard(title: String(localized: "Audit log"), symbol: "list.bullet.rectangle")
        auditCard.addRow(KCard.pair(
            LabeledField(title: String(localized: "Client"), control: auditClientField),
            LabeledField(title: String(localized: "Session"), control: auditSessionPopup)
        ))
        auditCard.addRow(errorsOnlyRow)
        auditCard.addRow(auditStack)
        auditCard.addRow(buttonRow([auditMoreButton, auditClearButton]))

        view = makeScrollingPage(makePageView([switchCard, serviceCard, configCard, clientsCard, grantsCard, auditCard]))
        // MCP 事件只更新状态和客户端列表，且只在页面可见时做；重新显示时 `viewWillAppear` 会补一次。
        mcp.addChangeHandler { [weak self] _ in
            guard let self, view.window?.isVisible == true else { return }
            refreshLive()
        }
        NotificationCenter.default.addObserver(
            forName: .sessionLibraryDidChange, object: mcp.core, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.reloadGrants() }
        }
        reloadStatus()
        reloadTokenButtons()
        reloadClients()
        reloadGrants()
        reloadAudit(reset: true)
        mcp.refreshTokenCache()
    }

    override func viewWillAppear() {
        super.viewWillAppear()
        refreshLive()
        // 窗口最小化期间事件被跳过；恢复时不会再触发 viewWillAppear，要自己补一次。
        if let window = view.window {
            deminiaturizeObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didDeminiaturizeNotification, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshLive() }
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

    /// 内容增减后重算页面高度并让设置窗口跟上。客户端、授权、审计列表变化后都要调用，
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

    /// MCP 事件触发的局部更新：状态、客户端列表、令牌按钮。
    private func refreshLive() {
        reloadStatus()
        reloadClients()
        reloadTokenButtons()
        relayout()
    }

    private func showError(_ text: String?) {
        errorLabel.stringValue = text ?? ""
        errorLabel.isHidden = text == nil
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        reloadAudit(reset: true)
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
        copyClaudeButton.isEnabled = hasToken
        copyCodexButton.isEnabled = hasToken
        copyTokenButton.isEnabled = hasToken
        regenerateButton.isEnabled = hasToken
    }

    private func copyConfig(_ kind: FfiMcpClientKind) {
        guard let token = mcp.cachedToken else { return }
        // 原样复制，保留开头的空格。
        copyText(mcpClientConfig(kind: kind, helperPath: mcpHelperPath(), token: token))
    }

    private func copyToken() {
        guard let token = mcp.cachedToken else { return }
        copyText(token)
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
            let when = NSTextField(labelWithString: Self.timeText(unixMs: client.connectedAtMs))
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

    func reloadGrants() {
        for view in grantStack.arrangedSubviews { view.removeFromSuperview() }
        levelPopups.removeAll()
        levelErrorLabels.removeAll()
        groupPopups.removeAll()
        let profiles = mcp.core.profiles().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        let groups = mcp.core.groups().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        var buckets: [String: [FfiProfile]] = [:]
        var ungrouped: [FfiProfile] = []
        let groupIDs = Set(groups.map(\.id))
        for profile in profiles {
            if let group = profile.group, groupIDs.contains(group) {
                buckets[group, default: []].append(profile)
            } else {
                ungrouped.append(profile)
            }
        }
        if !ungrouped.isEmpty {
            addGrantSection(title: String(localized: "No Group"), groupId: nil, profiles: ungrouped)
        }
        for group in groups {
            addGrantSection(title: group.name, groupId: group.id, profiles: buckets[group.id] ?? [])
        }
        relayout()
    }

    private func addGrantSection(title: String, groupId: String?, profiles: [FfiProfile]) {
        let header = NSTextField(labelWithString: title)
        header.font = .systemFont(ofSize: DesignTokens.Font.uiBody, weight: .semibold)
        let batch = KPopUpField()
        batch.setItems([
            String(localized: "Set group"),
            McpText.accessTitle(.off),
            McpText.accessTitle(.readOnly),
            McpText.accessTitle(.full),
        ])
        let ids = profiles.map(\.id)
        batch.onChange = { [weak self] index in
            guard let self, index > 0 else { return }
            let level = McpText.accessLevels[index - 1]
            self.setLevels(ids, to: level)
            batch.selectItem(at: 0)
        }
        groupPopups[groupId ?? ""] = batch
        let headerRow = NSStackView(views: [header, batch])
        headerRow.orientation = .horizontal
        headerRow.alignment = .centerY
        headerRow.spacing = DesignTokens.Spacing.sm
        grantStack.addArrangedSubview(headerRow)
        batch.widthAnchor.constraint(equalToConstant: 160).isActive = true
        for profile in profiles {
            let popup = KPopUpField()
            popup.setItems(McpText.accessLevels.map(McpText.accessTitle))
            let profileId = profile.id
            popup.onChange = { [weak self] index in
                self?.setLevels([profileId], to: McpText.accessLevels[index])
            }
            levelPopups[profile.id] = popup
            let name = NSTextField(labelWithString: profile.name)
            name.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
            name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            let row = NSStackView(views: [name, popup])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = DesignTokens.Spacing.sm
            do {
                popup.selectItem(at: McpText.accessIndex(try accessStore.mcpAccess(profileId: profile.id)))
            } catch {
                // 读不到就不能假装是「关闭」：禁用下拉框并说明原因，这一行也不参与敏感信息提示的判断。
                let reason = CoreMessageText.text(for: error)
                popup.isEnabled = false
                let label = NSTextField(labelWithString: String(localized: "Could not read the MCP access level: \(reason)"))
                label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
                label.textColor = .token(DesignTokens.Color.statusError)
                label.lineBreakMode = .byTruncatingTail
                label.toolTip = reason
                label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                levelErrorLabels[profile.id] = label
                row.addArrangedSubview(label)
            }
            grantStack.addArrangedSubview(row)
            popup.widthAnchor.constraint(equalToConstant: 160).isActive = true
        }
    }

    /// 写入授权。失败时下拉框回到实际存储的级别（重新读取），并在页面上显示原因，不让界面停在没生效的值上。
    private func setLevels(_ ids: [String], to level: FfiMcpAccess) {
        showError(nil)
        var raised = false
        var failure: Error?
        for id in ids {
            // 读不到原级别（nil）时不能当作 .off，否则会误弹敏感信息提示。
            let before = try? accessStore.mcpAccess(profileId: id)
            do {
                try accessStore.setMcpAccess(profileId: id, level: level)
                levelPopups[id]?.selectItem(at: McpText.accessIndex(level))
                if before == .off, level != .off { raised = true }
            } catch {
                failure = error
                if let actual = try? accessStore.mcpAccess(profileId: id) {
                    levelPopups[id]?.selectItem(at: McpText.accessIndex(actual))
                }
            }
        }
        if raised { sensitiveNotice.isHidden = false }
        if let failure {
            showError(String(localized: "Could not change the MCP access level: \(CoreMessageText.text(for: failure))"))
        }
        relayout()
    }

    private func reloadAudit(reset: Bool) {
        if reset {
            auditRecords = []
            refillAuditSessionPopup()
            let client = auditClientField.stringValue.trimmingCharacters(in: .whitespaces)
            let index = auditSessionPopup.indexOfSelectedItem
            committedFilter = (
                client.isEmpty ? nil : client,
                auditSessionIDs.indices.contains(index) ? auditSessionIDs[index] : nil,
                errorsOnlyRow.isChecked
            )
        }
        let filter = FfiMcpAuditFilter(
            client: committedFilter.client,
            profileId: committedFilter.profileId,
            errorsOnly: committedFilter.errorsOnly,
            beforeId: reset ? nil : auditRecords.last?.id,
            limit: auditPageSize
        )
        do {
            let page = try mcp.core.mcpAudit(filter: filter)
            auditError = nil
            if reset { auditRecords = page } else { auditRecords.append(contentsOf: page) }
            auditMoreButton.isHidden = page.count < Int(auditPageSize)
        } catch {
            // 读取失败不能当作「没有记录」；保留已显示的记录用于翻页失败的情况，但错误说明优先显示。
            auditError = String(localized: "Could not read the audit log: \(CoreMessageText.text(for: error))")
            auditMoreButton.isHidden = true
        }
        renderAudit()
    }

    private func refillAuditSessionPopup() {
        let profiles = mcp.core.profiles().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        let selected = auditSessionIDs.indices.contains(auditSessionPopup.indexOfSelectedItem)
            ? auditSessionIDs[auditSessionPopup.indexOfSelectedItem] : nil
        auditSessionIDs = [nil] + profiles.map(\.id)
        auditSessionPopup.setItems([String(localized: "All sessions")] + profiles.map(\.name))
        if let selected, let index = auditSessionIDs.firstIndex(of: selected) {
            auditSessionPopup.selectItem(at: index)
        } else {
            // 原选中的会话已被删除：setItems 之后选中项不保证回到第 0 项，显式回到「全部会话」。
            auditSessionPopup.selectItem(at: 0)
        }
    }

    private func renderAudit() {
        for view in auditStack.arrangedSubviews { view.removeFromSuperview() }
        if let auditError {
            let label = NSTextField(wrappingLabelWithString: auditError)
            label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            label.textColor = .token(DesignTokens.Color.statusError)
            label.preferredMaxLayoutWidth = SettingsPageViewController.pageWidth - 80
            auditStack.addArrangedSubview(label)
            relayout()
            return
        }
        if auditRecords.isEmpty {
            auditStack.addArrangedSubview(SettingsPageViewController.caption(String(localized: "No audit records.")))
            relayout()
            return
        }
        let names = Dictionary(uniqueKeysWithValues: mcp.core.profiles().map { ($0.id, $0.name) })
        for record in auditRecords {
            let session = McpText.auditSessionTitle(profileId: record.profileId, names: names)
            var parts = [Self.timeText(unixMs: record.atMs), McpText.clientName(record.client)]
            if !record.tool.isEmpty { parts.append(record.tool) }
            if let session { parts.append(session) }
            if let error = record.errorCode { parts.append(error) }
            let label = NSTextField(wrappingLabelWithString: parts.joined(separator: " · "))
            label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            label.textColor = .token(DesignTokens.Color.textSecondary)
            label.preferredMaxLayoutWidth = SettingsPageViewController.pageWidth - 80
            auditStack.addArrangedSubview(label)
            if !record.args.isEmpty {
                // 参数摘要可能很长（整条命令），界面只放截断后的一行，悬停看全文。
                let args = NSTextField(labelWithString: McpText.argsSummary(record.args))
                args.font = .monospacedSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
                args.textColor = .token(DesignTokens.Color.textTertiary)
                args.lineBreakMode = .byTruncatingTail
                args.toolTip = record.args
                args.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                args.preferredMaxLayoutWidth = SettingsPageViewController.pageWidth - 80
                auditStack.addArrangedSubview(args)
            }
        }
        relayout()
    }

    private func clearAudit() {
        do {
            try mcp.core.mcpAuditClear()
            reloadAudit(reset: true)
        } catch {
            auditError = String(localized: "Could not clear the audit log: \(CoreMessageText.text(for: error))")
            renderAudit()
        }
    }

    private func buttonRow(_ buttons: [NSView]) -> NSStackView {
        let row = NSStackView(views: buttons)
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = DesignTokens.Spacing.sm
        return row
    }

    private static func timeText(unixMs: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(unixMs) / 1000).formatted(date: .abbreviated, time: .shortened)
    }
}
