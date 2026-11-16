import AppKit

/// 传输进度条：确定进度画填充比例；总量未知（或为 0）时画往返移动的色块（不确定进度，不能当作"传完了"）。
@MainActor
final class FilerProgressBar: StyledBoxView {
    private let fill = StyledBoxView()
    private var timer: Timer?
    private var phase: CGFloat = 0
    var isAnimating: Bool { timer != nil }
    var fraction: Double? { didSet { updateAnimation(); needsLayout = true } }

    override init(frame: NSRect) {
        super.init(frame: frame)
        fillToken = DesignTokens.Color.bgField
        cornerRadius = 2
        fill.fillToken = DesignTokens.Color.accent
        fill.cornerRadius = 2
        addSubview(fill)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("FilerProgressBar 只支持代码创建") }

    override func layout() {
        super.layout()
        if let f = fraction {
            fill.frame = NSRect(x: 0, y: 0, width: bounds.width * CGFloat(f), height: bounds.height)
        } else {
            let w = bounds.width * 0.3
            fill.frame = NSRect(x: (bounds.width - w) * phase, y: 0, width: w, height: bounds.height)
        }
    }

    private func updateAnimation() {
        guard fraction == nil, window != nil, !isHiddenOrHasHiddenAncestor else { timer?.invalidate(); timer = nil; return }
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 0.03, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.phase = (self.phase + 0.02).truncatingRemainder(dividingBy: 2)
                self.needsLayout = true
                self.layout()
            }
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateAnimation()
    }

    override func viewDidHide() {
        super.viewDidHide()
        updateAnimation()
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        updateAnimation()
    }
}

/// 辅助 Filer 面板（filer.md §1–§8）：来源标识、工具条、路径栏、列表、底部统计 / 进度 / 拖放说明。
/// 状态全部来自 `FilerModel`；这里只负责显示与把用户操作转给 model。
@MainActor
final class FilerPanelView: StyledBoxView, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    let model: FilerModel
    /// "定位终端目录"：由协调者接到与 ⌘J 相同的动作。
    var onLocate: (() -> Void)?
    var prompter: FilerPrompter?

    // 控件
    private let identityIcon = NSImageView()
    private let identityLabel = NSTextField(labelWithString: "")
    private let paneTitle = NSTextField(labelWithString: "")
    private let renameButton = KButton(symbol: "pencil", style: .icon)
    private let newFileButton = KButton(symbol: "doc.badge.plus", style: .icon)
    private let newFolderButton = KButton(symbol: "folder.badge.plus", style: .icon)
    private let uploadButton = KButton(symbol: "icloud.and.arrow.up", style: .icon)
    private let downloadButton = KButton(symbol: "icloud.and.arrow.down", style: .icon)
    private let refreshButton = KButton(symbol: "arrow.clockwise", style: .icon)
    private let deleteButton = KButton(symbol: "trash", style: .icon)
    private let moreButton = KButton(symbol: "ellipsis", style: .icon)
    private let upButton = KButton(symbol: "arrow.up", style: .icon)
    private let locateButton = KButton(symbol: "scope", style: .icon)
    private let homeButton = KButton(symbol: "house", style: .icon)
    let pathBar = FilerPathBar()
    private let nameHeader = NSTextField(labelWithString: String(localized: "Name").uppercased())
    private let dateHeader = NSTextField(labelWithString: String(localized: "Modified").uppercased())
    private let sizeHeader = NSTextField(labelWithString: String(localized: "Size").uppercased())
    private let scroll = NSScrollView()
    let table = FilerTableView()
    private let stateLabel = NSTextField(wrappingLabelWithString: "")
    private let stateButton = KButton(title: String(localized: "Go to Home Folder"), style: .secondary)
    private let editRow = StyledBoxView()
    private let editIcon = NSImageView()
    private let editField = NSTextField()
    private let noticeLabel = NSTextField(wrappingLabelWithString: "")
    private let statsLabel = NSTextField(labelWithString: "")
    private let progressTitle = NSTextField(labelWithString: "")
    private let progressBytes = NSTextField(labelWithString: "")
    private let progressBar = FilerProgressBar()
    private let cancelButton = KButton(symbol: "xmark.circle", style: .icon)
    private let dropOverlay = StyledBoxView()

    private var rows: [FilerEntry] = []
    private var listingRevision = -1
    private var renderedSelection: [String] = []
    private(set) var listingReloadCount = 0
    private var syncingSelection = false
    private var dropHint: (text: String, accepted: Bool)?
    private var hideScheduled: FilerTransferStatus?
    private var isCommittingEdit = false
    private var nameDialog: FilerNameDialog?
    var connectionText: String? { didSet { if connectionText != oldValue { reload() } } }
    var dualDragWriter: ((FilerEntry) -> NSPasteboardWriting?)?
    var dualDropVerdict: ((NSPasteboard) -> FilerDropVerdict)?
    var dualDrop: ((NSPasteboard) -> Bool)?

    static let rowHeight: CGFloat = 28
    /// 头部合成一行所需的最小面板宽度。
    static let oneRowMinWidth: CGFloat = 640
    /// 头部（身份、操作、路径）底边，列表头从这里开始；分隔线画在它上面一像素。
    private var headerBottom: CGFloat = 0

    init(model: FilerModel) {
        self.model = model
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgPanel
        cornerRadius = 0
        setUp()
        let previous = model.onChange
        model.onChange = { [weak self] in previous?(); self?.reload() }
        registerForDraggedTypes(model.isDualPane ? [SftpDualDrag.type] : [.fileURL])
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("FilerPanelView 只支持代码创建") }

    // MARK: 搭建

    private func setUp() {
        identityIcon.contentTintColor = .token(DesignTokens.Color.textSecondary)
        identityLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption, weight: .medium)
        identityLabel.textColor = .token(DesignTokens.Color.textSecondary)
        identityLabel.lineBreakMode = .byTruncatingTail
        paneTitle.font = .systemFont(ofSize: DesignTokens.Font.uiTitle, weight: .semibold)
        paneTitle.textColor = .token(DesignTokens.Color.textPrimary)
        paneTitle.isHidden = !model.isDualPane
        renameButton.toolTip = String(localized: "Rename")
        renameButton.isHidden = !model.isDualPane
        renameButton.onPress = { [weak self] in self?.model.beginRename() }
        addSubview(paneTitle)
        addSubview(renameButton)

        let tips: [(KButton, String)] = [
            (newFileButton, String(localized: "New File")), (newFolderButton, String(localized: "New Folder")),
            (uploadButton, String(localized: "Upload")), (downloadButton, String(localized: "Download")),
            (refreshButton, String(localized: "Refresh")), (deleteButton, String(localized: "Delete")),
            (moreButton, String(localized: "More")), (upButton, String(localized: "Enclosing Folder")),
            (locateButton, String(localized: "Show Terminal Directory")), (homeButton, String(localized: "Home Folder")),
            (cancelButton, String(localized: "Cancel Transfer")),
        ]
        for (b, tip) in tips { b.toolTip = tip; addSubview(b) }
        newFileButton.onPress = { [weak self] in self?.model.beginCreate(directory: false) }
        newFolderButton.onPress = { [weak self] in self?.model.beginCreate(directory: true) }
        uploadButton.onPress = { [weak self] in self?.showUploadMenu() }
        downloadButton.onPress = { [weak self] in Task { await self?.model.downloadSelected() } }
        refreshButton.onPress = { [weak self] in self?.model.refresh() }
        deleteButton.onPress = { [weak self] in Task { await self?.model.deleteSelected() } }
        moreButton.onPress = { [weak self] in self?.showMoreMenu() }
        upButton.onPress = { [weak self] in self?.model.goUp() }
        homeButton.onPress = { [weak self] in self?.model.goHome() }
        locateButton.onPress = { [weak self] in self?.onLocate?() }
        cancelButton.onPress = { [weak self] in self?.model.cancelTransfers() }

        pathBar.onSubmit = { [weak self] in self?.model.navigate(toInput: $0) }
        pathBar.onNavigate = { [weak self] in self?.model.open(directory: $0) }
        pathBar.restoredText = { [weak self] in self?.model.restoredPathText ?? "" }

        for h in [nameHeader, dateHeader, sizeHeader] {
            h.font = .systemFont(ofSize: 10, weight: .semibold)
            h.textColor = .token(DesignTokens.Color.textTertiary)
        }
        dateHeader.alignment = .right
        sizeHeader.alignment = .right
        sizeHeader.isHidden = !model.isDualPane
        addSubview(sizeHeader)
        if model.isDualPane {
            for button in [newFileButton, uploadButton, downloadButton, locateButton] { button.isHidden = true }
        }

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.focusRingType = .none
        table.style = .plain
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(rowDoubleClicked)
        table.setDraggingSourceOperationMask([.copy], forLocal: false)
        table.setDraggingSourceOperationMask(model.isDualPane ? [.copy] : [], forLocal: true)
        table.onReturn = { [weak self] in
            guard let self, self.model.selectedEntries.count == 1, let e = self.model.selectedEntries.first else { return }
            self.model.activate(e)
        }
        table.onDelete = { [weak self] in Task { await self?.model.deleteSelected() } }
        table.onRename = { [weak self] in self?.model.beginRename() }
        table.menuProvider = { [weak self] row in self?.makeMenu(row: row) }
        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        addSubview(scroll)

        stateLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        stateLabel.textColor = .token(DesignTokens.Color.textTertiary)
        stateLabel.alignment = .center
        stateLabel.isSelectable = true
        stateButton.onPress = { [weak self] in self?.model.goHome() }
        addSubview(stateLabel)
        addSubview(stateButton)

        editRow.fillToken = DesignTokens.Color.bgField
        editRow.cornerRadius = DesignTokens.Radius.sm
        editIcon.contentTintColor = .token(DesignTokens.Color.accent)
        editField.isBezeled = false
        editField.drawsBackground = false
        editField.focusRingType = .none
        editField.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        editField.delegate = self
        editRow.addSubview(editIcon)
        editRow.addSubview(editField)
        addSubview(editRow)

        noticeLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        noticeLabel.maximumNumberOfLines = 4
        noticeLabel.isSelectable = true
        addSubview(noticeLabel)

        statsLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        statsLabel.textColor = .token(DesignTokens.Color.textTertiary)
        for l in [progressTitle, progressBytes] {
            l.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
            l.textColor = .token(DesignTokens.Color.textSecondary)
            l.lineBreakMode = .byTruncatingMiddle
        }
        for v in [identityIcon, identityLabel, pathBar, nameHeader, dateHeader, statsLabel, progressTitle, progressBytes, progressBar] { addSubview(v) }

        dropOverlay.borderToken = DesignTokens.Color.accent
        dropOverlay.borderWidth = 2
        dropOverlay.cornerRadius = 0
        dropOverlay.isHidden = true
        addSubview(dropOverlay, positioned: .above, relativeTo: nil)
    }

    // MARK: 渲染

    /// 显示状态 → 控件。幂等，model 每次变化都会调用。
    func reload() {
        let source = model.source
        paneTitle.stringValue = source?.isRemote == true ? String(localized: "Remote Files") : String(localized: "Local Files")
        identityIcon.image = .symbol(source?.isRemote == true ? "network" : "desktopcomputer")
        var identity = source?.title ?? String(localized: "No session")
        if model.isStale { identity += " · " + String(localized: "Connection closed") }
        if identityLabel.stringValue != identity {
            identityLabel.stringValue = identity
            // 身份文字宽度参与头部布局。
            needsLayout = true
        }
        identityLabel.textColor = .token(model.isStale ? DesignTokens.Color.statusWarning : DesignTokens.Color.textSecondary)
        identityLabel.toolTip = identity

        let editingActive = model.editing != .none
        let canWrite = model.canWrite
        let remote = model.source?.isRemote == true
        newFileButton.isEnabled = canWrite
        newFolderButton.isEnabled = canWrite
        renameButton.isEnabled = canWrite && model.selectedEntries.count == 1
        uploadButton.isEnabled = canWrite && remote
        downloadButton.isEnabled = remote && !model.selectedEntries.isEmpty && !model.isStale && model.tab?.isConnected == true
        deleteButton.isEnabled = canWrite && !model.selectedEntries.isEmpty
        refreshButton.isEnabled = model.availability == .available && !model.isStale
        upButton.isEnabled = model.availability == .available && model.directory != nil
        homeButton.isEnabled = model.availability == .available && !model.isStale
        locateButton.isEnabled = model.availability == .available && !model.isStale
        if model.isDualPane, model.tab?.isConnected != true {
            for button in [refreshButton, upButton, homeButton] { button.isEnabled = false }
        }
        uploadButton.toolTip = remote ? String(localized: "Upload") : String(localized: "Upload (connected SSH sessions only)")
        downloadButton.toolTip = remote ? String(localized: "Download") : String(localized: "Download (connected SSH sessions only)")

        if !pathBar.isEditing { pathBar.text = model.directory ?? "" }

        let listingChanged = listingRevision != model.listingRevision
        if listingChanged {
            listingRevision = model.listingRevision
            rows = model.visibleEntries
            table.reloadData()
            listingReloadCount += 1
        }
        if listingChanged || renderedSelection != model.selectedPaths {
            renderedSelection = model.selectedPaths
            syncSelection()
        }

        // 中间状态页：只有没有条目可显示时才出现。
        var message: String?
        var showHome = false
        switch model.availability {
        case .noTab: message = String(localized: "No session is open")
        case .unavailableSftp: message = String(localized: "The file browser is already open in this tab.")
        case .available:
            switch model.phase {
            case .failed(let e):
                message = e.message(source: source)
                showHome = true
            case .waitingConnection: message = String(localized: "Connecting…")
            case .idle where model.isStale && rows.isEmpty: message = String(localized: "Connection closed")
            case .loading where rows.isEmpty: message = String(localized: "Loading…")
            case .loaded where rows.isEmpty: message = String(localized: "Empty folder")
            default: break
            }
        }
        stateLabel.stringValue = message ?? ""
        if let connectionText {
            stateLabel.stringValue = connectionText
            message = connectionText
            showHome = false
        }
        stateLabel.textColor = .token(model.phase.isFailure ? DesignTokens.Color.statusError : DesignTokens.Color.textTertiary)
        stateLabel.isHidden = message == nil
        stateButton.isHidden = !showHome
        scroll.isHidden = connectionText != nil || (message != nil && rows.isEmpty)

        editRow.isHidden = !editingActive || model.isDualPane
        if editingActive {
            if model.isDualPane { presentNameDialogIfNeeded() } else { beginEditRowIfNeeded() }
        } else if let nameDialog {
            nameDialog.cancel()
        }

        if let n = model.notice ?? model.remoteEditNotice {
            noticeLabel.stringValue = n.text
            noticeLabel.textColor = .token(n.isError ? DesignTokens.Color.statusError : DesignTokens.Color.textSecondary)
            noticeLabel.isHidden = false
        } else {
            noticeLabel.isHidden = true
        }
        renderFooter()
        needsLayout = true
        scheduleHideIfNeeded()
    }

    private func renderFooter() {
        let status = model.transferStatus?.isRunning == true ? model.transferStatus : (model.remoteEditDownload ?? model.transferStatus)
        let progressVisible = status != nil && dropHint == nil
        for v in [progressTitle, progressBytes, progressBar, cancelButton] { v.isHidden = !progressVisible }
        statsLabel.isHidden = progressVisible
        if let dropHint {
            statsLabel.stringValue = dropHint.text
            statsLabel.textColor = .token(dropHint.accepted ? DesignTokens.Color.accent : DesignTokens.Color.statusError)
        } else if let status {
            let name = status.count > 1 ? "\(status.name) (\(status.index)/\(status.count))" : status.name
            switch status.phase {
            case .running: progressTitle.stringValue = "\(status.actionText) \(name)"
            case .completed: progressTitle.stringValue = String(localized: "Done") + " · " + status.name
            case .cancelled: progressTitle.stringValue = String(localized: "Cancelled")
            case .failed: progressTitle.stringValue = String(localized: "Failed") + " · " + status.name
            }
            progressTitle.toolTip = status.origin
            progressBytes.stringValue = status.byteText
            progressBar.fraction = status.fraction
            cancelButton.isHidden = !status.isRunning
        } else if let notice = model.remoteEditNotice {
            statsLabel.stringValue = notice.text
            statsLabel.toolTip = notice.text
            statsLabel.textColor = .token(notice.isError ? DesignTokens.Color.statusError : DesignTokens.Color.textSecondary)
        } else if model.isLoading {
            statsLabel.stringValue = String(localized: "Loading…")
            statsLabel.textColor = .token(DesignTokens.Color.textTertiary)
        } else if model.availability == .available, model.phase == .loaded {
            statsLabel.stringValue = model.isDualPane ? String(localized: "\(model.stats.total) items") + " · " + (model.directory ?? "") : model.stats.text
            statsLabel.textColor = .token(DesignTokens.Color.textTertiary)
        } else {
            statsLabel.stringValue = ""
        }
        if let connectionText { statsLabel.stringValue = connectionText }
        dropOverlay.isHidden = dropHint == nil
        dropOverlay.borderToken = dropHint?.accepted == false ? DesignTokens.Color.statusError : DesignTokens.Color.accent
    }

    /// 完成 / 取消约 1.8 秒后恢复条目统计，失败约 3 秒（filer.md §6.2）；失败原因仍留在提示区。
    private func scheduleHideIfNeeded() {
        guard !model.isDualPane else { return }
        guard let s = model.transferStatus, !s.isRunning, hideScheduled != s else { return }
        hideScheduled = s
        let delay: TimeInterval = if case .failed = s.phase { 3 } else { 1.8 }
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.model.transferStatus == s else { return }
                self.model.dismissTransferStatus()
            }
        }
    }

    // MARK: 就地输入

    private func presentNameDialogIfNeeded() {
        guard nameDialog == nil, let window else { return }
        let dialog = FilerNameDialog(model: model)
        nameDialog = dialog
        dialog.present(in: window) { [weak self] in self?.nameDialog = nil }
    }

    private func beginEditRowIfNeeded() {
        guard editField.window == nil || window?.firstResponder !== editField.currentEditor() else { return }
        guard !isCommittingEdit else { return }
        switch model.editing {
        case .createFile:
            editIcon.image = FilerIconCache.image(for: .generic); editField.stringValue = ""
        case .createDirectory:
            editIcon.image = FilerIconCache.image(for: .folder); editField.stringValue = ""
        case .rename(let e):
            editIcon.image = FilerIconCache.image(for: FilerIconCategory.category(for: e)); editField.stringValue = e.name
        case .none: return
        }
        window?.makeFirstResponder(editField)
        if case .rename(let e) = model.editing, let editor = editField.currentEditor() {
            // 重命名预选主文件名（不含扩展名）。
            let stem = (e.name as NSString).deletingPathExtension.utf16.count
            editor.selectedRange = NSRange(location: 0, length: e.isDirectory ? e.name.utf16.count : stem)
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard control === editField else { return false }
        if selector == #selector(NSResponder.insertNewline(_:)) {
            commitEdit(viaBlur: false)
            return true
        }
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            isCommittingEdit = true
            model.cancelEditing()
            isCommittingEdit = false
            window?.makeFirstResponder(table)
            return true
        }
        return false
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard (obj.object as? NSTextField) === editField, !isCommittingEdit, model.editing != .none else { return }
        // 失焦提交；Return / Esc 已在 doCommandBy 处理（它们也会触发结束编辑，用标志位避免提交两次）。
        let movement = (obj.userInfo?["NSTextMovement"] as? Int) ?? 0
        if movement == NSReturnTextMovement || movement == NSCancelTextMovement { return }
        commitEdit(viaBlur: true)
    }

    private func commitEdit(viaBlur: Bool) {
        isCommittingEdit = true
        let text = editField.stringValue
        Task { [weak self] in
            guard let self else { return }
            let close = await self.model.commitEditing(name: text, viaBlur: viaBlur)
            self.isCommittingEdit = false
            if close { self.window?.makeFirstResponder(self.table) }
        }
    }

    // MARK: 表格

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("cell")
        let cell = (tableView.makeView(withIdentifier: id, owner: nil) as? FilerCellView) ?? {
            let c = FilerCellView(); c.identifier = id; return c
        }()
        cell.configure(rows[row], dual: model.isDualPane)
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? { FilerRowView() }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !syncingSelection else { return }
        model.setSelection(paths: table.selectedRowIndexes.compactMap { $0 < rows.count ? rows[$0].path : nil })
    }

    /// 按路径恢复选中：分批插入后行号变了，选中的仍是同一个对象。
    private func syncSelection() {
        syncingSelection = true
        let set = Set(model.selectedPaths)
        table.selectRowIndexes(IndexSet(rows.enumerated().filter { set.contains($0.element.path) }.map(\.offset)), byExtendingSelection: false)
        syncingSelection = false
    }

    @objc private func rowDoubleClicked() {
        let row = table.clickedRow
        guard row >= 0, row < rows.count else { return }
        model.activate(rows[row])
    }

    /// 拖出：本地条目交给系统（文件 URL）；远端条目给 file promise，只在接收方要求时下载。
    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
        guard row < rows.count else { return nil }
        let entry = rows[row]
        if model.isDualPane { return dualDragWriter?(entry) }
        guard model.source?.isRemote == true else { return FilerPasteboard.localItem(path: entry.path) }
        return makePromise?(entry)
    }

    /// 远端条目拖出的提供者工厂（协调者装配：它持有传输引擎与交付登记）。
    var makePromise: ((FilerEntry) -> NSPasteboardWriting?)?

    // MARK: 菜单

    private func menu(_ items: [NSMenuItem]) -> NSMenu {
        let m = NSMenu()
        for i in items { m.addItem(i) }
        return m
    }

    func makeMenu(row: Int) -> NSMenu? {
        guard !model.isDualPane else { return nil }
        guard model.availability == .available else { return nil }
        let remote = model.source?.isRemote == true
        let writable = model.canWrite
        if row >= 0 {
            let picked = model.selectedEntries
            guard !picked.isEmpty else { return nil }
            var items: [NSMenuItem] = []
            let single = picked.count == 1 ? picked[0] : nil
            let open = ActionMenuItem(String(localized: "Open")) { [weak self] in if let single { self?.model.activate(single) } }
            if remote { open.isEnabled = model.tab?.isConnected == true }
            if single == nil { open.isEnabled = false }
            items.append(open)
            if let single, !single.isDirectory, !single.isSymlink, remote {
                let with = NSMenuItem(title: String(localized: "Open With"), action: nil, keyEquivalent: "")
                let applications = NSMenu()
                for application in model.recentEditorApplications {
                    applications.addItem(ActionMenuItem(application.deletingPathExtension().lastPathComponent) { [weak self] in
                        self?.model.openRemoteFile(single, application: application)
                    })
                }
                if !applications.items.isEmpty { applications.addItem(.separator()) }
                applications.addItem(ActionMenuItem(String(localized: "Other Application…")) { [weak self] in
                    self?.model.openRemoteFile(single, chooseApplication: true)
                })
                with.submenu = applications
                with.isEnabled = model.tab?.isConnected == true
                items.append(with)
            }
            if remote {
                let dl = ActionMenuItem(String(localized: "Download")) { [weak self] in Task { await self?.model.downloadSelected() } }
                dl.isEnabled = model.tab?.isConnected == true && !model.isStale
                items.append(dl)
            }
            items.append(.separator())
            items.append(ActionMenuItem(String(localized: "Copy Full Path")) { [weak self] in self?.model.copyPath(of: picked) })
            items.append(ActionMenuItem(String(localized: "Copy Name")) { [weak self] in self?.model.copyName(of: picked) })
            let rename = ActionMenuItem(String(localized: "Rename")) { [weak self] in self?.model.beginRename() }
            rename.isEnabled = writable && single != nil
            items.append(rename)
            items.append(.separator())
            let del = ActionMenuItem(String(localized: "Delete…")) { [weak self] in Task { await self?.model.deleteSelected() } }
            del.isEnabled = writable
            items.append(del)
            return menu(items)
        }
        var items: [NSMenuItem] = []
        let nf = ActionMenuItem(String(localized: "New File")) { [weak self] in self?.model.beginCreate(directory: false) }
        nf.isEnabled = writable
        let nd = ActionMenuItem(String(localized: "New Folder")) { [weak self] in self?.model.beginCreate(directory: true) }
        nd.isEnabled = writable
        items += [nf, nd]
        if remote {
            let uf = ActionMenuItem(String(localized: "Upload Files…")) { [weak self] in Task { await self?.model.chooseAndUpload(directories: false) } }
            let ud = ActionMenuItem(String(localized: "Upload Folders…")) { [weak self] in Task { await self?.model.chooseAndUpload(directories: true) } }
            uf.isEnabled = writable; ud.isEnabled = writable
            items += [.separator(), uf, ud]
        }
        items.append(.separator())
        items.append(ActionMenuItem(String(localized: "Copy Directory Path")) { [weak self] in self?.model.copyDirectoryPath() })
        items.append(ActionMenuItem(String(localized: "Refresh")) { [weak self] in self?.model.refresh() })
        return menu(items)
    }

    private func showUploadMenu() {
        let m = menu([
            ActionMenuItem(String(localized: "Upload Files…")) { [weak self] in Task { await self?.model.chooseAndUpload(directories: false) } },
            ActionMenuItem(String(localized: "Upload Folders…")) { [weak self] in Task { await self?.model.chooseAndUpload(directories: true) } },
        ])
        // 菜单锚在按钮左下角，AppKit 会自动把它限制在屏幕内，不会超出窗口右边界而点不到。
        m.popUp(positioning: nil, at: NSPoint(x: uploadButton.frame.minX, y: uploadButton.frame.maxY + 2), in: self)
    }

    private func showMoreMenu() {
        let m = menu([ActionMenuItem(String(localized: "Show Hidden Files"), checked: model.showsHiddenFiles) { [weak self] in
            guard let self else { return }
            self.model.showsHiddenFiles.toggle()
        }])
        m.popUp(positioning: nil, at: NSPoint(x: moreButton.frame.minX, y: moreButton.frame.maxY + 2), in: self)
    }

    // MARK: 拖入（整个面板都是落点）

    private func verdict(_ info: NSDraggingInfo) -> FilerDropVerdict {
        if model.isDualPane { return dualDropVerdict?(info.draggingPasteboard) ?? .ignore }
        // 落点判定用 AppKit 给的窗口点坐标，不做像素缩放（见 `FilerDropGeometry`）。
        let urls = FilerPasteboard.fileURLs(info.draggingPasteboard)
        return model.dropVerdict(urls: urls, isFromSelf: FilerPasteboard.isFromFiler(info.draggingPasteboard))
    }

    private func applyDrag(_ info: NSDraggingInfo) -> NSDragOperation {
        switch verdict(info) {
        case .accept(let hint):
            dropHint = (hint, true); renderFooter(); return .copy
        case .reject(let reason):
            dropHint = (reason, false); renderFooter(); return []
        case .ignore:
            clearDropHint(); return []
        }
    }

    private func clearDropHint() {
        guard dropHint != nil else { return }
        dropHint = nil
        renderFooter()
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation { applyDrag(sender) }
    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation { applyDrag(sender) }
    override func draggingExited(_ sender: NSDraggingInfo?) { clearDropHint() }
    override func draggingEnded(_ sender: NSDraggingInfo) { clearDropHint() }
    override func prepareForDragOperation(_ sender: NSDraggingInfo) -> Bool {
        if case .accept = verdict(sender) { return true }
        // 拒绝的落点也要给出原因，不能无声忽略。
        if !model.isDualPane, case .reject = verdict(sender) { _ = model.performDrop(urls: FilerPasteboard.fileURLs(sender.draggingPasteboard), isFromSelf: false) }
        return false
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        defer { clearDropHint() }
        if model.isDualPane { return dualDrop?(sender.draggingPasteboard) ?? false }
        return model.performDrop(
            urls: FilerPasteboard.fileURLs(sender.draggingPasteboard),
            isFromSelf: FilerPasteboard.isFromFiler(sender.draggingPasteboard)
        )
    }

    // MARK: 布局

    override func layout() {
        super.layout()
        let w = bounds.width
        let pad: CGFloat = 10
        // 安全区：内容视图被标题栏遮住时从它下方开始（现在窗口不是全尺寸内容视图，通常为 0）。
        var y = max(safeAreaInsets.top, 0)
        if model.isDualPane {
            paneTitle.frame = NSRect(x: pad, y: y + 12, width: max(0, w - pad * 2), height: 20)
            y += 38
        }
        // 头部布局：身份（本地 / 用户@主机）与操作按钮共用一行，路径另起一行；面板够宽时（单栏、≥ 640pt）
        // 三者合成一行。不再给身份单独占一行，头部从三行减到两行或一行。
        let b: CGFloat = 26
        let rowH: CGFloat = 34
        let actions = model.isDualPane ? [newFolderButton, renameButton, deleteButton, refreshButton] : [newFileButton, newFolderButton, uploadButton, downloadButton, refreshButton, deleteButton]
        let oneRow = !model.isDualPane && w >= Self.oneRowMinWidth
        // 右侧：`…` 靠最右，操作按钮从它左边往左排。
        moreButton.frame = NSRect(x: w - 8 - b, y: y + (rowH - b) / 2, width: b, height: b)
        var right = moreButton.frame.minX - 4
        for btn in actions.reversed() {
            right -= b
            btn.frame = NSRect(x: right, y: y + (rowH - b) / 2, width: b, height: b)
            right -= 4
        }
        let actionsMinX = right
        identityIcon.frame = NSRect(x: pad, y: y + (rowH - 14) / 2, width: 14, height: 14)
        let labelX = pad + 20
        // 用 cell 的尺寸而不是 intrinsicContentSize：后者在文字刚改过、还没布局时可能是旧值，短文字会被截成"…"。
        let labelW = ceil(identityLabel.cell?.cellSize.width ?? 0)
        if oneRow {
            // 身份文字最多占 160pt，其余给路径。
            identityLabel.frame = NSRect(x: labelX, y: y + (rowH - 16) / 2, width: min(labelW, 160), height: 16)
        } else {
            identityLabel.frame = NSRect(x: labelX, y: y + (rowH - 16) / 2, width: max(0, min(labelW, actionsMinX - labelX - 8)), height: 16)
            y += rowH
        }
        // 路径段：上一级、定位、主目录、路径栏。
        let navX = oneRow ? identityLabel.frame.maxX + 10 : 8
        let pathMaxX = oneRow ? actionsMinX - 6 : w - 8
        upButton.frame = NSRect(x: navX, y: y + (rowH - b) / 2, width: b, height: b)
        locateButton.frame = NSRect(x: navX + b + 2, y: y + (rowH - b) / 2, width: b, height: b)
        homeButton.frame = NSRect(x: navX + (b + 2) * (model.isDualPane ? 1 : 2), y: y + (rowH - b) / 2, width: b, height: b)
        let pathX = homeButton.frame.maxX + 6
        pathBar.frame = NSRect(x: pathX, y: y + (rowH - b) / 2, width: max(0, pathMaxX - pathX), height: b)
        y += rowH
        headerBottom = y
        // 列表头。
        nameHeader.frame = NSRect(x: FilerCellView.pad + 24, y: y + 4, width: 120, height: 14)
        let dateWidth = FilerCellView.dateWidth(dual: model.isDualPane)
        dateHeader.frame = NSRect(x: w - FilerCellView.pad - dateWidth - 4, y: y + 4, width: dateWidth + 4, height: 14)
        sizeHeader.frame = NSRect(x: dateHeader.frame.minX - FilerCellView.sizeWidth - 4, y: y + 4, width: FilerCellView.sizeWidth, height: 14)
        if model.isDualPane { nameHeader.frame.size.width = max(0, sizeHeader.frame.minX - nameHeader.frame.minX - 8) }
        y += 22
        // 底部区域高度。
        let progressVisible = model.transferStatus != nil && dropHint == nil
        let footerH: CGFloat = progressVisible ? 46 : 24
        var noticeH: CGFloat = 0
        if !noticeLabel.isHidden {
            noticeH = min(64, noticeLabel.sizeThatFits(NSSize(width: w - pad * 2, height: 0)).height + 6)
        }
        if !editRow.isHidden {
            editRow.frame = NSRect(x: 6, y: y + 2, width: w - 12, height: 24)
            editIcon.frame = NSRect(x: 8, y: 4, width: 16, height: 16)
            editField.frame = NSRect(x: 30, y: 4, width: max(0, editRow.bounds.width - 38), height: 16)
            y += 28
        }
        let listBottom = bounds.height - footerH - noticeH
        scroll.frame = NSRect(x: 0, y: y, width: w, height: max(0, listBottom - y))
        let stateRect = NSRect(x: pad, y: y + 12, width: max(0, w - pad * 2), height: max(0, listBottom - y - 12))
        let textH = min(stateRect.height, stateLabel.sizeThatFits(NSSize(width: stateRect.width, height: 0)).height)
        stateLabel.frame = NSRect(x: stateRect.minX, y: stateRect.minY + (stateButton.isHidden ? 40 : 20), width: stateRect.width, height: textH)
        let sbw: CGFloat = 150
        stateButton.frame = NSRect(x: (w - sbw) / 2, y: stateLabel.frame.maxY + 10, width: sbw, height: 30)
        noticeLabel.frame = NSRect(x: pad, y: listBottom + 3, width: w - pad * 2, height: max(0, noticeH - 6))
        let fy = bounds.height - footerH
        statsLabel.frame = NSRect(x: pad, y: fy + 5, width: w - pad * 2, height: 14)
        progressTitle.frame = NSRect(x: pad, y: fy + 4, width: max(0, w - pad * 2 - 28), height: 14)
        cancelButton.frame = NSRect(x: w - pad - 22, y: fy + 1, width: 22, height: 22)
        progressBar.frame = NSRect(x: pad, y: fy + 22, width: w - pad * 2, height: 4)
        progressBytes.frame = NSRect(x: pad, y: fy + 29, width: w - pad * 2, height: 14)
        dropOverlay.frame = bounds
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        // 工具条与列表之间的分隔线。
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        NSRect(x: 0, y: headerBottom - 1, width: bounds.width, height: 1).fill()
        NSRect(x: 0, y: bounds.height - (model.transferStatus != nil && dropHint == nil ? 46 : 24), width: bounds.width, height: 1).fill()
    }

    // MARK: 测试入口

    var visibleRowCount: Int { rows.count }
    var footerText: String { statsLabel.stringValue }
    var stateText: String { stateLabel.stringValue }
    var progressTitleText: String { progressTitle.stringValue }
    var isDropOverlayVisible: Bool { !dropOverlay.isHidden }
    func debugMenu(row: Int) -> NSMenu? { makeMenu(row: row) }
}

private extension FilerPhase {
    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}
