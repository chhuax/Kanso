import AppKit
import KansoBridge
import KansoUI

/// 设置 → MCP。是否出现这一页只由调用方传入的 `mcpSupported()` 决定，不看服务状态。
@MainActor
final class McpSettingsViewController: SettingsPageViewController, NSTextFieldDelegate {
    let mcp: McpCoordinator
    var copyText: (String) -> Void = { text in
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    let enabledRow = KCheckRow(title: String(localized: "Enable MCP"))
    let statusLabel = NSTextField(wrappingLabelWithString: "")
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
    private var auditRecords: [FfiMcpAuditRecord] = []
    private var auditSessionIDs: [String?] = []
    private let auditPageSize: UInt32 = 20

    init(mcp: McpCoordinator) {
        self.mcp = mcp
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

        view = makePageView([switchCard, serviceCard, configCard, clientsCard, grantsCard, auditCard])
        mcp.addChangeHandler { [weak self] in
            self?.reloadStatus()
            self?.reloadClients()
            self?.reloadTokenButtons()
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
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        reloadAudit(reset: true)
    }

    private func toggleEnabled(_ on: Bool) {
        do {
            try mcp.setEnabled(on)
        } catch {
            enabledRow.isChecked = mcp.core.mcpEnabled()
            statusLabel.stringValue = CoreMessageText.text(for: error)
        }
        reloadStatus()
        reloadTokenButtons()
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
    }

    private func reloadTokenButtons() {
        let hasToken = (try? mcp.currentToken()) != nil
        copyClaudeButton.isEnabled = hasToken
        copyCodexButton.isEnabled = hasToken
        copyTokenButton.isEnabled = hasToken
        regenerateButton.isEnabled = hasToken
    }

    private func copyConfig(_ kind: FfiMcpClientKind) {
        guard let token = try? mcp.currentToken() else { return }
        // 原样复制，保留开头的空格。
        copyText(mcpClientConfig(kind: kind, helperPath: mcpHelperPath(), token: token))
    }

    private func copyToken() {
        guard let token = try? mcp.currentToken() else { return }
        copyText(token)
    }

    private func regenerate() {
        try? mcp.regenerateToken()
        reloadTokenButtons()
    }

    private func reloadClients() {
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
            let level = (try? mcp.core.mcpAccess(profileId: profile.id)) ?? .off
            let popup = KPopUpField()
            popup.setItems(McpText.accessLevels.map(McpText.accessTitle))
            popup.selectItem(at: McpText.accessIndex(level))
            let profileId = profile.id
            popup.onChange = { [weak self] index in
                guard let self else { return }
                let next = McpText.accessLevels[index]
                let previous = (try? self.mcp.core.mcpAccess(profileId: profileId)) ?? .off
                self.setLevels([profileId], to: next, previous: [profileId: previous])
            }
            levelPopups[profile.id] = popup
            let name = NSTextField(labelWithString: profile.name)
            name.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
            name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            let row = NSStackView(views: [name, popup])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = DesignTokens.Spacing.sm
            grantStack.addArrangedSubview(row)
            popup.widthAnchor.constraint(equalToConstant: 160).isActive = true
        }
    }

    private func setLevels(_ ids: [String], to level: FfiMcpAccess, previous: [String: FfiMcpAccess]? = nil) {
        var raised = false
        for id in ids {
            let before = previous?[id] ?? ((try? mcp.core.mcpAccess(profileId: id)) ?? .off)
            if before == .off, level != .off { raised = true }
            try? mcp.core.setMcpAccess(profileId: id, level: level)
            levelPopups[id]?.selectItem(at: McpText.accessIndex(level))
        }
        if raised { sensitiveNotice.isHidden = false }
    }

    private func reloadAudit(reset: Bool) {
        if reset {
            auditRecords = []
            refillAuditSessionPopup()
        }
        let client = auditClientField.stringValue.trimmingCharacters(in: .whitespaces)
        let index = auditSessionPopup.indexOfSelectedItem
        let profileId = auditSessionIDs.indices.contains(index) ? auditSessionIDs[index] : nil
        let filter = FfiMcpAuditFilter(
            client: client.isEmpty ? nil : client,
            profileId: profileId,
            errorsOnly: errorsOnlyRow.isChecked,
            beforeId: reset ? nil : auditRecords.last?.id,
            limit: auditPageSize
        )
        let page = (try? mcp.core.mcpAudit(filter: filter)) ?? []
        if reset { auditRecords = page } else { auditRecords.append(contentsOf: page) }
        auditMoreButton.isHidden = page.count < Int(auditPageSize)
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
        }
    }

    private func renderAudit() {
        for view in auditStack.arrangedSubviews { view.removeFromSuperview() }
        if auditRecords.isEmpty {
            auditStack.addArrangedSubview(SettingsPageViewController.caption(String(localized: "No audit records.")))
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
        }
    }

    private func clearAudit() {
        try? mcp.core.mcpAuditClear()
        reloadAudit(reset: true)
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
