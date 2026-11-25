import AppKit
import KansoBridge
import KansoUI

/// MCP 活动记录窗口的内容（spec/mcp.md §7.3）：AI 工具通过 MCP 做过的每一次操作，按时间倒序分页显示，
/// 可按客户端、会话、「只看失败」筛选，也可以清空。
///
/// 由 `McpActivityWindowController` 创建并持有。所有读库都在主线程同步完成（一页 20 条）。
@MainActor
final class McpActivityViewController: NSViewController, NSTextFieldDelegate {
    private let core: KansoCore
    /// 读一页记录；默认读 core 的审计库。core 的 FFI 不提供写入审计的入口，测试只能替换它来制造记录。
    var readPage: (FfiMcpAuditFilter) throws -> [FfiMcpAuditRecord]
    /// 清空审计库；默认调 core。测试可替换。
    var clearRecords: () throws -> Void
    /// 读会话列表（填充下拉框与会话名缓存）；默认读 core。测试替换它来统计读取次数。
    var readProfiles: () -> [FfiProfile]
    /// 读全部非「关闭」的授权（一次读出，不逐行查）；默认读 core。测试可替换。失败会抛错。
    var readAccesses: () throws -> [String: FfiMcpAccess]
    /// 清空前的确认。默认以 sheet 挂在窗口上；测试注入，不弹真的对话框。
    var confirmClear: () async -> Bool = { true }
    /// 用户要求关闭窗口（Esc）；由窗口控制器接线。
    var onClose: (() -> Void)?

    let introLabel = NSTextField(wrappingLabelWithString: String(
        localized: "Every action an AI tool takes through MCP is recorded here. Records stay on this Mac for 30 days."
    ))
    /// 授权读取失败时显示原因（错误色），位于说明行下方；读取成功时隐藏。
    let accessErrorLabel = NSTextField(wrappingLabelWithString: "")
    let clientField = KTextField()
    let sessionPopup = KPopUpField()
    let errorsOnlyRow = KCheckRow(title: String(localized: "Errors only"))
    let moreButton = KButton(title: String(localized: "More"), style: .secondary)
    let clearButton = KButton(title: String(localized: "Clear"), style: .secondary)
    /// 翻过页之后记录库有变化时出现；点击回到第一页并重载。没有变化时隐藏。
    /// 服务端的回调不区分「写入新记录」与「清理旧记录」，所以文案保持中性，不能写成「有新记录」。
    let newActivityButton = KButton(title: String(localized: "Activity log changed — Reload"), style: .secondary)

    private let recordStack = NSStackView()
    private var records: [FfiMcpAuditRecord] = []
    private var sessionIDs: [String?] = []
    /// 上次提交（reset）时的筛选条件。翻页必须沿用它：用户可能在输入框里改了字但还没提交，
    /// 此时「更多」读输入框会让后一页和已显示的前几页来自不同条件。
    private var committedFilter: (client: String?, profileId: String?, errorsOnly: Bool) = (nil, nil, false)
    /// 读取或清空审计失败的原因；非 nil 时列表区显示它而不是记录。
    private(set) var auditError: String?
    private let pageSize: UInt32 = 20
    /// 会话 id → 名称，由 `refreshSessionData` 刷新（首次加载、整体重载、会话库变化），
    /// 自动刷新与翻页渲染直接复用，不再重复拉取 profiles。
    private var profileNames: [String: String] = [:]
    /// 会话 id → 当前 MCP 级别，只含不是「关闭」的会话；与 `profileNames` 同时刷新。
    /// 读取失败时为 nil：授权未知，不能把会话判成「未授权」。
    private var profileAccesses: [String: FfiMcpAccess]? = [:]
    /// 最近一次读取授权失败的原因；非 nil 时窗口说明行下方显示它。
    private(set) var accessError: String?
    nonisolated(unsafe) private var libraryObserver: NSObjectProtocol?
    /// 是否已经翻过页（显示的记录超过第一页）。翻过页时自动刷新不能重载，否则用户已加载的页会被丢掉。
    private(set) var hasPagedBeyondFirstPage = false

    init(core: KansoCore) {
        self.core = core
        readPage = { try core.mcpAudit(filter: $0) }
        clearRecords = { try core.mcpAuditClear() }
        readProfiles = { core.profiles() }
        readAccesses = { try core.mcpLevelMap() }
        super.init(nibName: nil, bundle: nil)
    }

    deinit {
        if let libraryObserver { NotificationCenter.default.removeObserver(libraryObserver) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("McpActivityViewController 只支持代码创建") }

    /// 当前显示的记录条数，供测试读取。
    var displayedRecordCount: Int { records.count }

    override func loadView() {
        let root = StyledBoxView()
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0
        view = root

        introLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        introLabel.textColor = .token(DesignTokens.Color.textTertiary)
        accessErrorLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        accessErrorLabel.textColor = .token(DesignTokens.Color.statusError)
        accessErrorLabel.isHidden = true

        clientField.placeholder = String(localized: "Client")
        clientField.textField.delegate = self
        errorsOnlyRow.onToggle = { [weak self] _ in self?.commitFilterAndReload() }
        sessionPopup.onChange = { [weak self] _ in self?.commitFilterAndReload() }
        moreButton.onPress = { [weak self] in self?.loadNextPage() }
        clearButton.onPress = { [weak self] in
            Task { @MainActor in await self?.clear() }
        }
        // 回到第一页但沿用已提交的条件：输入框里没提交的文字不该借这次点击生效。
        newActivityButton.onPress = { [weak self] in self?.reloadCommitted() }
        newActivityButton.isHidden = true

        let filters = KCard.pair(
            LabeledField(title: String(localized: "Client"), control: clientField),
            LabeledField(title: String(localized: "Session"), control: sessionPopup)
        )

        recordStack.orientation = .vertical
        recordStack.alignment = .leading
        recordStack.spacing = DesignTokens.Spacing.md
        let document = FlippedView()
        recordStack.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(recordStack)
        let scroll = NSScrollView()
        scroll.documentView = document
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.borderType = .noBorder
        document.translatesAutoresizingMaskIntoConstraints = false
        let pad = KCard.padding
        NSLayoutConstraint.activate([
            // 文档宽度跟随可视区：记录文字随窗口宽度换行。
            document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            recordStack.topAnchor.constraint(equalTo: document.topAnchor, constant: pad),
            recordStack.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -pad),
            recordStack.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: pad),
            recordStack.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -pad),
        ])
        let listBox = StyledBoxView()
        listBox.fillToken = DesignTokens.Color.bgCard
        listBox.borderToken = DesignTokens.Color.borderCard
        listBox.cornerRadius = DesignTokens.Radius.lg
        scroll.translatesAutoresizingMaskIntoConstraints = false
        listBox.addSubview(scroll)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: listBox.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: listBox.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: listBox.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: listBox.trailingAnchor),
        ])

        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(249), for: .horizontal)
        let bar = NSStackView(views: [newActivityButton, spacer, moreButton, clearButton])
        bar.orientation = .horizontal
        bar.alignment = .centerY
        bar.spacing = DesignTokens.Spacing.sm

        let column = NSStackView(views: [introLabel, accessErrorLabel, filters, errorsOnlyRow, listBox, bar])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = DesignTokens.Spacing.md
        column.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(column)
        let inset = DesignTokens.Spacing.xl
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: root.topAnchor, constant: inset),
            column.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -inset),
            column.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: inset),
            column.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -inset),
        ])
        for row in [introLabel, accessErrorLabel, filters, errorsOnlyRow, listBox, bar] {
            row.translatesAutoresizingMaskIntoConstraints = false
            row.leadingAnchor.constraint(equalTo: column.leadingAnchor).isActive = true
            row.trailingAnchor.constraint(equalTo: column.trailingAnchor).isActive = true
        }
        // 列表区吃掉窗口多出来的高度，其余行保持贴合内容。
        listBox.setContentHuggingPriority(.defaultLow, for: .vertical)
        listBox.heightAnchor.constraint(greaterThanOrEqualToConstant: 120).isActive = true

        // 会话被改名、删除或授权级别变化时刷新下拉框与缓存；窗口不可见时也要处理，
        // 否则重新打开前缓存已过期。读取只是一次 profiles + 一次 mcpAccesses。
        libraryObserver = NotificationCenter.default.addObserver(
            forName: .sessionLibraryDidChange, object: core, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.handleSessionLibraryChange() }
        }
        refreshSessionData()
        commitFilterAndReload()
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        // 失焦、回车都会进来；文字与已提交的筛选相同时重查没有意义，还会丢掉翻页进度。
        let text = clientField.stringValue.trimmingCharacters(in: .whitespaces)
        guard (text.isEmpty ? nil : text) != committedFilter.client else { return }
        commitFilterAndReload()
    }

    /// 焦点在筛选框时字段编辑器会先吃掉 Esc（cancelOperation），窗口收不到；这里转成关闭窗口。
    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        guard commandSelector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        onClose?()
        return true
    }

    override func cancelOperation(_ sender: Any?) { onClose?() }

    /// 窗口被打开、前置或从最小化恢复时调用：期间到达的 `.audit` 事件被跳过，要自己补一次。
    /// 回到第一页并保持筛选；已翻过的页被丢弃，用户重新打开窗口时看最新的更符合预期。
    /// 整体重载会重新读会话列表，并提交控件里的当前条件（窗口重新出现时用户看到什么就按什么查）。
    func refreshFromStart() {
        refreshSessionData()
        commitFilterAndReload()
    }

    /// 窗口可见期间收到 `.audit`：未翻页就重载第一页，翻过页就只提示记录库有变化。
    /// 只重读记录并重建记录行：筛选条件用 `committedFilter`（输入框里未提交的文字不能借自动刷新生效），
    /// 会话下拉框与会话名缓存也不动（它们只在会话库变化和整体重载时刷新）。
    func handleAuditChange() {
        // 上次读授权失败（可能是瞬时的库忙）：顺带重试，否则错误提示要等会话库变化或重开窗口才会消失。
        if profileAccesses == nil { refreshAccesses() }
        if hasPagedBeyondFirstPage {
            newActivityButton.isHidden = false
        } else {
            reloadCommitted()
        }
    }

    /// 会话库变化：刷新下拉框与缓存。选中的会话被删除时下拉框回到「全部会话」，筛选条件随之改变，需要重查；
    /// 否则只重建记录行（会话名、「未授权的会话」可能变了），不丢翻页进度。
    private func handleSessionLibraryChange() {
        let selectionLost = refreshSessionData()
        if selectionLost {
            commitFilterAndReload()
        } else {
            render()
        }
    }

    /// 用户提交筛选（回车 / 失焦 / 切换下拉 / 勾选）或整体重载：从控件读取条件并记为已提交，再回到第一页重载。
    private func commitFilterAndReload() {
        let client = clientField.stringValue.trimmingCharacters(in: .whitespaces)
        let index = sessionPopup.indexOfSelectedItem
        committedFilter = (
            client.isEmpty ? nil : client,
            sessionIDs.indices.contains(index) ? sessionIDs[index] : nil,
            errorsOnlyRow.isChecked
        )
        reloadCommitted()
    }

    /// 按已提交的条件回到第一页重载；不读任何控件。
    private func reloadCommitted() {
        records = []
        hasPagedBeyondFirstPage = false
        newActivityButton.isHidden = true
        loadPage(reset: true)
    }

    /// 「更多」：沿用已提交的条件读下一页。
    private func loadNextPage() { loadPage(reset: false) }

    private func loadPage(reset: Bool) {
        let filter = FfiMcpAuditFilter(
            client: committedFilter.client,
            profileId: committedFilter.profileId,
            errorsOnly: committedFilter.errorsOnly,
            beforeId: reset ? nil : records.last?.id,
            limit: pageSize
        )
        do {
            let page = try readPage(filter)
            auditError = nil
            if reset {
                records = page
            } else {
                records.append(contentsOf: page)
                // 读回空页（恰好整页数量的记录时点「更多」）没有新增内容：不能算翻过页，
                // 否则之后的 `.audit` 自动刷新会被误拦成只提示。
                if !page.isEmpty { hasPagedBeyondFirstPage = true }
            }
            moreButton.isHidden = page.count < Int(pageSize)
        } catch {
            // 读取失败不能当作「没有记录」；保留已显示的记录用于翻页失败的情况，但错误说明优先显示。
            auditError = String(localized: "Could not read the audit log: \(CoreMessageText.text(for: error))")
            moreButton.isHidden = true
        }
        render()
    }

    /// 刷新会话下拉框、名称与级别缓存。返回「原来选中的具体会话已不存在」（下拉框已回到全部会话）。
    @discardableResult
    private func refreshSessionData() -> Bool {
        let selectionLost = refillSessionPopup()
        refreshAccesses()
        return selectionLost
    }

    /// 重读「会话 → 级别」缓存；失败时置 nil 并显示原因。
    private func refreshAccesses() {
        do {
            profileAccesses = try readAccesses()
            accessError = nil
        } catch {
            // 读失败不能当成「全部关闭」：否则每条失败记录都会被误标成「未授权的会话」。
            profileAccesses = nil
            accessError = String(localized: "Could not read the MCP access level: \(CoreMessageText.text(for: error))")
        }
        accessErrorLabel.stringValue = accessError ?? ""
        accessErrorLabel.isHidden = accessError == nil
    }

    private func refillSessionPopup() -> Bool {
        let profiles = readProfiles().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        let selected = sessionIDs.indices.contains(sessionPopup.indexOfSelectedItem)
            ? sessionIDs[sessionPopup.indexOfSelectedItem] : nil
        sessionIDs = [nil] + profiles.map(\.id)
        profileNames = Dictionary(uniqueKeysWithValues: profiles.map { ($0.id, $0.name) })
        sessionPopup.setItems([String(localized: "All sessions")] + profiles.map(\.name))
        if let selected, let index = sessionIDs.firstIndex(of: selected) {
            sessionPopup.selectItem(at: index)
            return false
        }
        // 原选中的会话已被删除：setItems 之后选中项不保证回到第 0 项，显式回到「全部会话」。
        sessionPopup.selectItem(at: 0)
        return selected != nil
    }

    private func render() {
        for view in recordStack.arrangedSubviews { view.removeFromSuperview() }
        if let auditError {
            let label = Self.wrappingLabel(auditError, color: DesignTokens.Color.statusError)
            recordStack.addArrangedSubview(label)
            fillWidth(label)
            return
        }
        if records.isEmpty {
            let label = Self.wrappingLabel(
                String(localized: "No audit records."), color: DesignTokens.Color.textTertiary
            )
            recordStack.addArrangedSubview(label)
            fillWidth(label)
            return
        }
        for record in records {
            let session = McpText.auditSessionTitle(
                profileId: record.profileId, failed: record.errorCode != nil,
                names: profileNames, accesses: profileAccesses
            )
            var parts = McpText.auditHeadParts(record)
            if let session { parts.append(session) }
            if let error = record.errorCode { parts.append(error) }
            if let tab = record.sessionId, record.tool == "kanso_terminal_run" || record.tool == "kanso_terminal_send" {
                parts.append("tab=\(tab)")
            }
            switch record.dispatch {
            case .some(.submitted):
                parts.append(String(localized: "Sent"))
            case .some(.rejected):
                parts.append(String(localized: "Rejected before sending"))
            case .some(.unconfirmed):
                parts.append(String(localized: "Sending unconfirmed"))
            case .none:
                break
            }
            let head = Self.wrappingLabel(parts.joined(separator: " · "), color: DesignTokens.Color.textSecondary)
            var lines: [NSView] = [head]
            var summaries: [String] = []
            if let input = record.input {
                summaries.append(record.tool == "kanso_terminal_send"
                    ? String(localized: "Terminal input: \(input)")
                    : String(localized: "Command: \(input)"))
            }
            if !record.args.isEmpty { summaries.append(record.args) }
            for summary in summaries {
                // 长命令和参数各显示一行，悬停看全文。
                let args = NSTextField(labelWithString: McpText.argsSummary(summary))
                args.font = .monospacedSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
                args.textColor = .token(DesignTokens.Color.textTertiary)
                args.lineBreakMode = .byTruncatingTail
                args.toolTip = summary
                args.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                lines.append(args)
            }
            let row = NSStackView(views: lines)
            row.orientation = .vertical
            row.alignment = .leading
            row.spacing = 2
            recordStack.addArrangedSubview(row)
            fillWidth(row)
            for line in lines { fillWidth(line, in: row) }
        }
    }

    private static func wrappingLabel(_ text: String, color: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.textColor = .token(color)
        return label
    }

    /// 让行撑满记录栈的宽度，换行才会随窗口宽度变化。
    private func fillWidth(_ view: NSView, in container: NSView? = nil) {
        let target = container ?? recordStack
        view.translatesAutoresizingMaskIntoConstraints = false
        view.widthAnchor.constraint(equalTo: target.widthAnchor).isActive = true
    }

    /// 确认后清空。取消、清空失败都不会丢掉已显示的记录；失败时在列表区显示原因。
    func clear() async {
        guard await confirmClear() else { return }
        do {
            try clearRecords()
            reloadCommitted()
        } catch {
            auditError = String(localized: "Could not clear the audit log: \(CoreMessageText.text(for: error))")
            render()
        }
    }
}

/// MCP 活动记录窗口（spec/mcp.md §7.3）。同一时间只有一个：由 `MainWindowController` 持有，
/// 入口（菜单栏、状态栏指示器、设置页按钮）都通过它的 `present()` 打开，再次打开只前置。
///
/// 窗口可见时订阅 `McpCoordinator` 的 `.audit` 变化自动刷新；见 `McpActivityViewController.handleAuditChange`。
@MainActor
final class McpActivityWindowController: NSWindowController {
    /// 同一时间窗口内的多个 `.audit` 合并为一次处理的间隔（秒）。服务端本来就每 500 ms 批量落库，
    /// 这里再合并一层，避免调用密集时窗口里的列表被频繁重建。
    static let auditCoalesceDelay: TimeInterval = 1

    let content: McpActivityViewController
    private let mcp: McpCoordinator
    /// 合并计时的取消闭包；非 nil 表示已经有一次处理在排队，此时到来的 `.audit` 直接并入它。
    private var pendingAudit: (() -> Void)?
    nonisolated(unsafe) private var deminiaturizeObserver: NSObjectProtocol?
    nonisolated(unsafe) private var hideObservers: [NSObjectProtocol] = []
    /// 窗口内容是否是最新的。创建时 `loadView` 已读过一次，所以初始为真；窗口关闭或最小化后期间的
    /// `.audit` 事件被丢弃，内容过期，下次显示要整体重载。
    private var contentIsFresh = true

    /// - Parameter autosaveName: 窗口位置与大小的记忆名；测试传 nil，避免写入用户真实的 `UserDefaults.standard`。
    init(mcp: McpCoordinator, autosaveName: String? = "KansoMcpActivityWindow") {
        self.mcp = mcp
        content = McpActivityViewController(core: mcp.core)
        let defaultSize = NSSize(width: 640, height: 520)
        let minSize = NSSize(width: 480, height: 340)
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: defaultSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false
        )
        window.title = String(localized: "MCP Activity")
        window.contentViewController = content
        window.minSize = minSize
        // 赋 contentViewController 会把窗口缩到根视图初始约束的最小宽度，这里恢复默认尺寸。
        window.setContentSize(defaultSize)
        // 关闭后窗口留在持有者手里，下次 `present()` 直接再显示。
        window.isReleasedWhenClosed = false
        super.init(window: window)

        if let autosaveName {
            // 没有已保存的位置，或旧版本存下了低于最小尺寸的窄框时，恢复默认尺寸并居中；
            // setFrameAutosaveName 必须在这之后调用，否则会把居中前的位置存下来。
            if !window.setFrameUsingName(autosaveName)
                || window.frame.width < minSize.width
                || window.frame.height < minSize.height
            {
                window.setContentSize(defaultSize)
                window.center()
            }
            window.setFrameAutosaveName(autosaveName)
        } else {
            window.center()
        }
        content.confirmClear = { [weak self] in await self?.confirmClearWithAlert() ?? false }
        content.onClose = { [weak self] in self?.window?.performClose(nil) }
        // 只在窗口可见时处理；不可见期间的事件被丢弃，`present()` 与恢复最小化时会整体重载补上。
        mcp.addChangeHandler { [weak self] change in
            guard let self, change.contains(.audit) else { return }
            // 窗口不可见（关闭、最小化，或整个 App 被隐藏）时丢掉事件，但要记下内容已过期，
            // 下次 `present()` 才会重读；只靠关闭 / 最小化两个特例会漏掉 ⌘H 隐藏的情况。
            guard window.isVisible else {
                contentIsFresh = false
                return
            }
            scheduleAuditRefresh()
        }
        // 关闭与最小化之后内容过期；恢复最小化只在 didDeminiaturize 里重载一次，`present()` 不再重复。
        for name in [NSWindow.willCloseNotification, NSWindow.didMiniaturizeNotification] {
            hideObservers.append(NotificationCenter.default.addObserver(
                forName: name, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.contentIsFresh = false }
            })
        }
        deminiaturizeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didDeminiaturizeNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.pendingAudit?()
                self.pendingAudit = nil
                self.content.refreshFromStart()
                self.contentIsFresh = true
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("McpActivityWindowController 只支持代码创建") }

    deinit {
        if let deminiaturizeObserver { NotificationCenter.default.removeObserver(deminiaturizeObserver) }
        for observer in hideObservers { NotificationCenter.default.removeObserver(observer) }
    }

    /// 打开或前置窗口。内容过期（窗口关闭或最小化期间的 `.audit` 事件没处理过）时从第一页重新读取。
    /// 首次创建时 `loadView` 刚读过，不重复读；从最小化恢复由 didDeminiaturize 的观察者重载，这里不再读一遍
    /// （`showWindow` 会触发恢复，两处都读会白读一次库）。
    func present() {
        // 只有窗口不可见、要整体重载时才取消排队的刷新（重载已经包含它）。
        // 窗口已可见时只前置：排队的刷新必须保留，否则这次前置会把期间到达的新记录吞掉。
        if window?.isVisible != true {
            pendingAudit?()
            pendingAudit = nil
            if window?.isMiniaturized != true, !contentIsFresh {
                content.refreshFromStart()
                contentIsFresh = true
            }
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
    }

    /// 排一次合并处理。已有排队的就并入：窗口里最终只看到最新状态，中间态没有意义。
    /// 计时走协调器注入的 `scheduleAfter`，测试可以手动触发。
    private func scheduleAuditRefresh() {
        guard pendingAudit == nil else { return }
        pendingAudit = mcp.scheduleAfter(Self.auditCoalesceDelay) { [weak self] in
            guard let self else { return }
            pendingAudit = nil
            // 排队期间窗口可能被关掉；关闭期间不读库，重新打开时整体重载。
            guard window?.isVisible == true else { return }
            content.handleAuditChange()
        }
    }

    private func confirmClearWithAlert() async -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "Clear the MCP activity log?")
        alert.informativeText = String(localized: "All records of what AI tools did through MCP will be deleted. This can’t be undone.")
        alert.addButton(withTitle: String(localized: "Clear")).hasDestructiveAction = true
        alert.addButton(withTitle: String(localized: "Cancel"))
        guard let window else { return alert.runModal() == .alertFirstButtonReturn }
        return await withCheckedContinuation { continuation in
            alert.beginSheetModal(for: window) { continuation.resume(returning: $0 == .alertFirstButtonReturn) }
        }
    }
}
