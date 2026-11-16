import AppKit
import KansoBridge
import KansoUI

@MainActor
final class GitPanelView: StyledBoxView, NSTextFieldDelegate {
    let model: GitPanelModel
    var onClose: (() -> Void)?
    private let titleLabel = NSTextField(labelWithString: String(localized: "Changes"))
    private let repositoryLabel = NSTextField(labelWithString: "")
    private let branchLabel = NSTextField(labelWithString: "")
    private let totalsLabel = NSTextField(labelWithString: "")
    private let refreshButton = KButton(symbol: "arrow.clockwise", style: .icon)
    private let closeButton = KButton(symbol: "xmark", style: .icon)
    private let moreButton = KButton(symbol: "ellipsis", style: .icon)
    private let errorLabel = NSTextField(wrappingLabelWithString: "")
    private let dismissButton = KButton(symbol: "xmark", style: .icon)
    private let emptyLabel = NSTextField(wrappingLabelWithString: "")
    private let scroll = NSScrollView()
    private let document = GitDocumentView()
    let messageField = KTextField()
    private let commitButton = KButton(title: String(localized: "Commit"), symbol: "checkmark", style: .primary)
    private var rows: [GitFileRow] = []
    private let settings: SettingsStore
    private let copyPath: (String) -> Bool

    init(model: GitPanelModel, settings: SettingsStore = SettingsStore(), copyPath: @escaping (String) -> Bool = { SystemFilerPrompter().copyToPasteboard($0) }) {
        self.model = model
        self.settings = settings
        self.copyPath = copyPath
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgPanel
        cornerRadius = 0
        titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        repositoryLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        repositoryLabel.lineBreakMode = .byTruncatingMiddle
        branchLabel.font = .systemFont(ofSize: 11)
        branchLabel.textColor = .secondaryLabelColor
        branchLabel.lineBreakMode = .byTruncatingMiddle
        totalsLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .medium)
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center
        errorLabel.textColor = .systemRed
        errorLabel.font = .systemFont(ofSize: 11)
        refreshButton.toolTip = String(localized: "Refresh Changes")
        closeButton.toolTip = String(localized: "Close Changes Panel")
        moreButton.toolTip = String(localized: "More Actions")
        refreshButton.onPress = { [weak model] in model?.dismissError(); model?.refresh() }
        closeButton.onPress = { [weak self] in self?.onClose?() }
        moreButton.onPress = { [weak self] in self?.showMore() }
        dismissButton.onPress = { [weak model] in model?.dismissError() }
        messageField.textField.placeholderString = String(localized: "Commit Message (Return to Commit)")
        messageField.textField.delegate = self
        commitButton.onPress = { [weak model] in Task { await model?.commit() } }
        scroll.documentView = document
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        for view in [titleLabel, repositoryLabel, branchLabel, totalsLabel, refreshButton, closeButton, moreButton,
                     errorLabel, dismissButton, emptyLabel, scroll, messageField, commitButton] { addSubview(view) }
        let previous = model.onChange
        model.onChange = { [weak self] in previous?(); self?.reload() }
        settings.observe(self) { [weak self] section in
            if section == .display { self?.reload() }
        }
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("GitPanelView 只支持代码创建") }

    func controlTextDidChange(_ notification: Notification) { model.message = messageField.textField.stringValue }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        guard commandSelector == #selector(NSResponder.insertNewline(_:)) else { return false }
        Task { await model.commit() }
        return true
    }

    private func reload() {
        let summary = model.summary
        repositoryLabel.stringValue = summary.map { ($0.root as NSString).lastPathComponent } ?? ""
        repositoryLabel.toolTip = summary?.root
        branchLabel.stringValue = summary.map { "⎇  \($0.branch)" } ?? ""
        branchLabel.toolTip = summary?.branch
        let totals = NSMutableAttributedString()
        if let summary {
            totals.append(NSAttributedString(string: "+\(summary.added)", attributes: [.foregroundColor: NSColor.systemGreen]))
            totals.append(NSAttributedString(string: "  −\(summary.deleted)", attributes: [.foregroundColor: NSColor.systemRed]))
        }
        totalsLabel.attributedStringValue = totals
        errorLabel.stringValue = model.error ?? ""
        errorLabel.isHidden = model.error == nil
        dismissButton.isHidden = model.error == nil
        emptyLabel.stringValue = model.isLoading && summary == nil ? String(localized: "Reading Changes…") : summary == nil ? GitPanelModel.emptyText : String(localized: "Working Tree Clean")
        emptyLabel.isHidden = summary?.files.isEmpty == false
        refreshButton.isEnabled = !model.isWriting
        moreButton.isHidden = summary?.files.isEmpty != false
        moreButton.isEnabled = model.canWrite
        commitButton.isHidden = moreButton.isHidden
        messageField.isHidden = moreButton.isHidden
        messageField.textField.isEnabled = !model.isWriting
        if messageField.textField.stringValue != model.message { messageField.textField.stringValue = model.message }
        commitButton.isEnabled = model.canCommit
        let files = summary?.files ?? []
        titleLabel.attributedStringValue = Self.title(count: files.count)
        // 按路径复用行：展开一个文件只重建这一行。整表重建在几百个改动时很慢（每行都带文本视图），
        // 且一次展开会触发两次（展开时、差异读回时）。
        var previous = Dictionary(rows.map { ($0.state.file.path, $0) }, uniquingKeysWith: { first, _ in first })
        rows = files.map { file in
            let state = GitFileRow.State(file: file, expanded: model.expanded.contains(file.path), diff: model.diffs[file.path], error: model.diffErrors[file.path], display: settings.display)
            if let row = previous.removeValue(forKey: file.path) {
                if row.state == state { return row }
                row.removeFromSuperview()
            }
            let row = GitFileRow(state: state)
            row.onToggle = { [weak model] in model?.toggle(file.path) }
            row.onDiscard = { [weak self] in self?.confirmDiscard(file) }
            row.onCopy = { [weak self] in
                guard let self else { return }
                self.model.copyPath(file.path, using: self.copyPath)
            }
            document.addSubview(row)
            return row
        }
        for row in previous.values { row.removeFromSuperview() }
        for row in rows { row.isEnabled = !model.isWriting && !model.isLoading }
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let width = bounds.width
        titleLabel.frame = NSRect(x: 16, y: 12, width: max(0, width - 120), height: 20)
        closeButton.frame = NSRect(x: width - 36, y: 10, width: 24, height: 24)
        refreshButton.frame = NSRect(x: width - 66, y: 10, width: 24, height: 24)
        moreButton.frame = NSRect(x: width - 96, y: 10, width: 24, height: 24)
        repositoryLabel.frame = NSRect(x: 16, y: 48, width: max(0, width - 130), height: 20)
        totalsLabel.frame = NSRect(x: width - 125, y: 49, width: 110, height: 18)
        branchLabel.frame = NSRect(x: 16, y: 73, width: width - 32, height: 18)
        let errorHeight: CGFloat = model.error == nil ? 0 : min(110, max(48, errorLabel.sizeThatFits(NSSize(width: max(0, width - 60), height: 1000)).height + 16))
        errorLabel.frame = NSRect(x: 16, y: 103, width: width - 60, height: errorHeight)
        dismissButton.frame = NSRect(x: width - 36, y: 103, width: 24, height: 24)
        let footer: CGFloat = messageField.isHidden ? 0 : 96
        scroll.frame = NSRect(x: 0, y: 105 + errorHeight, width: width, height: max(0, bounds.height - 105 - errorHeight - footer))
        emptyLabel.frame = NSRect(x: 24, y: 130 + errorHeight, width: max(0, width - 48), height: 65)
        messageField.frame = NSRect(x: 12, y: bounds.height - 86, width: width - 24, height: 30)
        commitButton.frame = NSRect(x: 12, y: bounds.height - 46, width: width - 24, height: 30)
        let contentWidth = scroll.contentSize.width
        var top: CGFloat = 8
        for row in rows {
            let height = row.height
            row.frame = NSRect(x: 10, y: top, width: max(0, contentWidth - 20), height: height)
            top += height + 8
        }
        document.frame = NSRect(x: 0, y: 0, width: contentWidth, height: max(top, scroll.contentSize.height))
    }

    /// 标题后跟改动文件数（次要颜色）；没有摘要或工作区干净时只显示标题。
    private static func title(count: Int) -> NSAttributedString {
        let title = NSMutableAttributedString(string: String(localized: "Changes"), attributes: [
            .font: NSFont.systemFont(ofSize: 13, weight: .semibold),
            .foregroundColor: NSColor.labelColor,
        ])
        if count > 0 {
            title.append(NSAttributedString(string: "  \(count)", attributes: [
                .foregroundColor: NSColor.secondaryLabelColor,
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .medium),
            ]))
        }
        return title
    }

    private func showMore() {
        guard model.canWrite else { return }
        let menu = NSMenu()
        menu.addItem(ActionMenuItem(String(localized: "Discard All Changes")) { [weak self] in self?.confirmDiscard(nil) })
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: moreButton.bounds.maxY), in: moreButton)
    }

    private func confirmDiscard(_ file: FfiGitFile?) {
        guard model.canWrite, let summary = model.summary, let window else { return }
        let revision = model.revision
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = file.map { String(localized: "Discard changes to “\($0.path)”?") }
            ?? String(localized: "Discard all changes in “\((summary.root as NSString).lastPathComponent)”?")
        let newContent = file.map { ["A", "U", "C"].contains($0.status) } ?? false
        alert.informativeText = file == nil
            ? String(localized: "Staged, unstaged, and untracked changes will be permanently lost. Git-ignored files are kept.")
            : newContent
            ? String(localized: "New content will be permanently deleted.")
            : String(localized: "Staged and unstaged edits will be lost; renames will restore the original path.")
        alert.informativeText += "\n" + String(localized: "There is no in-app undo, Trash, or automatic backup.")
        alert.addButton(withTitle: String(localized: "Cancel"))
        alert.addButton(withTitle: String(localized: "Discard"))
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertSecondButtonReturn else { return }
            Task { @MainActor in
                guard let self, self.model.revision == revision else { return }
                await self.model.discard(file?.path)
            }
        }
    }
}

private final class GitDocumentView: NSView {
    override var isFlipped: Bool { true }
}

@MainActor
final class GitFileRow: StyledBoxView {
    var onToggle: (() -> Void)?
    var onDiscard: (() -> Void)?
    var onCopy: (() -> Void)?
    var isEnabled = true { didSet { toggle.isEnabled = isEnabled; discard.isEnabled = isEnabled; copy.isEnabled = isEnabled } }
    private let toggle = KButton(style: .icon)
    private let discard = KButton(symbol: "arrow.uturn.backward", style: .icon)
    private let copy = KButton(symbol: "doc.on.doc", style: .icon)
    private let header = StyledBoxView()
    private let icon = NSImageView()
    private let path = PassthroughLabel(labelWithString: "")
    private let status = PassthroughLabel(labelWithString: "")
    private let badge = StyledBoxView()
    private let counts = PassthroughLabel(labelWithString: "")
    private let textScroll = GitDiffScrollView()
    private let text: GitDiffTextView?
    private let detail = NSTextField(wrappingLabelWithString: "")
    /// 决定这一行外观的全部输入；相同则复用已有行。
    struct State: Equatable {
        let file: FfiGitFile
        let expanded: Bool
        let diff: FfiGitDiff?
        let error: String?
        let display: DisplaySettings
    }
    let state: State
    let height: CGFloat

    init(state: State) {
        self.state = state
        let (file, expanded, diff, error) = (state.file, state.expanded, state.diff, state.error)
        let hasLines = diff?.lines.isEmpty == false
        text = expanded && hasLines ? GitDiffTextView(lines: diff?.lines ?? [], display: state.display) : nil
        let notice = diff?.notice.map { CoreMessageText.text($0) } ?? ""
        let hasDetail = error != nil || !notice.isEmpty || diff?.truncated == true || !hasLines
        height = 40 + (expanded ? (text.map { $0.contentSize.height + 16 } ?? 0) + (hasDetail ? (hasLines ? 48 : 64) : 0) : 0)
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgPanel
        borderToken = DesignTokens.Color.borderSubtle
        cornerRadius = DesignTokens.Radius.md
        layer?.masksToBounds = true
        header.fillToken = DesignTokens.Color.bgCard
        header.cornerRadius = 0
        badge.fillToken = DesignTokens.Color.diffBadgeBackground
        badge.cornerRadius = 9
        toggle.symbol = expanded ? "chevron.down" : "chevron.right"
        toggle.onPress = { [weak self] in self?.onToggle?() }
        discard.onPress = { [weak self] in self?.onDiscard?() }
        copy.onPress = { [weak self] in self?.onCopy?() }
        copy.toolTip = String(localized: "Copy Full Path")
        discard.toolTip = String(localized: "Discard Changes to This File")
        let category = FilerIconCategory.category(for: FilerEntry(name: (file.path as NSString).lastPathComponent, path: file.path, isDirectory: false))
        icon.image = FilerIconCache.image(for: category)
        icon.contentTintColor = category.tint
        path.stringValue = file.path + (file.original.map { "  ←  \($0)" } ?? "")
        path.font = .systemFont(ofSize: 12)
        path.lineBreakMode = .byTruncatingMiddle
        path.toolTip = path.stringValue
        status.stringValue = file.status
        status.font = .monospacedSystemFont(ofSize: 11, weight: .medium)
        status.textColor = .token(file.status == "!" ? DesignTokens.Color.statusError : DesignTokens.Color.textSecondary)
        let countText = NSMutableAttributedString(string: file.added.map { "+\($0)" } ?? "—", attributes: [.foregroundColor: NSColor.token(DesignTokens.Color.diffAdded)])
        countText.append(NSAttributedString(string: " · ", attributes: [.foregroundColor: NSColor.token(DesignTokens.Color.textSecondary)]))
        countText.append(NSAttributedString(string: file.deleted.map { "−\($0)" } ?? "—", attributes: [.foregroundColor: NSColor.token(DesignTokens.Color.diffRemoved)]))
        counts.font = .monospacedDigitSystemFont(ofSize: 10, weight: .medium)
        countText.addAttribute(.font, value: counts.font!, range: NSRange(location: 0, length: countText.length))
        counts.lineBreakMode = .byClipping
        counts.attributedStringValue = countText
        badge.addSubview(counts)
        textScroll.documentView = text
        textScroll.drawsBackground = false
        textScroll.hasVerticalScroller = false
        textScroll.hasHorizontalScroller = true
        textScroll.autohidesScrollers = false
        textScroll.scrollerStyle = .overlay
        textScroll.verticalScrollElasticity = .none
        textScroll.isHidden = !expanded || !hasLines
        detail.font = .systemFont(ofSize: 11)
        detail.textColor = .token(error == nil ? DesignTokens.Color.textSecondary : DesignTokens.Color.statusError)
        detail.stringValue = error ?? (diff.map {
            let notice = $0.notice.map { CoreMessageText.text($0) } ?? ""
            return notice + ($0.truncated ? (notice.isEmpty ? "" : "\n") + String(localized: "Diff Truncated") : "")
        } ?? String(localized: "Reading Diff…"))
        detail.isHidden = !expanded || !hasDetail
        for view in [header, toggle, icon, path, copy, badge, status, discard, textScroll, detail] { addSubview(view) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("GitFileRow 只支持代码创建") }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let hit = super.hitTest(point) else { return nil }
        let local = convert(point, from: superview)
        if local.y < 40, ![toggle, copy, discard].contains(where: { hit === $0 || hit.isDescendant(of: $0) }) { return self }
        return hit
    }

    override func mouseDown(with event: NSEvent) {
        if isEnabled, convert(event.locationInWindow, from: nil).y < 40 { onToggle?() }
        else { super.mouseDown(with: event) }
    }

    override func layout() {
        super.layout()
        let width = bounds.width
        header.frame = NSRect(x: 1, y: 1, width: width - 2, height: 39)
        toggle.frame = NSRect(x: 4, y: 8, width: 22, height: 24)
        icon.frame = NSRect(x: 29, y: 13, width: 14, height: 14)
        let badgeWidth = ceil(counts.attributedStringValue.size().width) + 16
        let pathWidth = min(path.intrinsicContentSize.width, max(0, width - 50 - 24 - badgeWidth - 66))
        path.frame = NSRect(x: 50, y: 11, width: pathWidth, height: 18)
        copy.frame = NSRect(x: path.frame.maxX + 2, y: 8, width: 22, height: 24)
        badge.frame = NSRect(x: copy.frame.maxX + 4, y: 11, width: badgeWidth, height: 18)
        counts.frame = NSRect(x: 6, y: 2, width: badgeWidth - 12, height: 14)
        status.frame = NSRect(x: width - 51, y: 11, width: 14, height: 18)
        discard.frame = NSRect(x: width - 30, y: 8, width: 24, height: 24)
        textScroll.frame = NSRect(x: 1, y: 40, width: width - 2, height: text.map { $0.contentSize.height + 16 } ?? 0)
        if let text {
            text.frame = NSRect(origin: .zero, size: NSSize(width: max(textScroll.contentSize.width, text.contentSize.width), height: textScroll.contentSize.height))
        }
        detail.frame = NSRect(x: 16, y: textScroll.isHidden ? 48 : textScroll.frame.maxY + 4, width: width - 32, height: textScroll.isHidden ? 48 : 40)
    }
}
