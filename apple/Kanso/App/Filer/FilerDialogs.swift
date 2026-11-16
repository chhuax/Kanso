import AppKit
import UniformTypeIdentifiers

@MainActor
final class FilerNameDialog {
    let shell: DialogShellView
    let field = KTextField()
    private let model: FilerModel
    private var sheet: DialogSheetWindow?
    private var onClose: (() -> Void)?

    init(model: FilerModel) {
        self.model = model
        let renaming: Bool
        if case .rename = model.editing {
            renaming = true
        } else {
            renaming = false
        }
        shell = DialogShellView(title: renaming ? String(localized: "Rename") : String(localized: "New Folder"),
                                symbol: renaming ? "pencil" : "folder.badge.plus", width: 420)
        if case .rename(let entry) = model.editing { field.stringValue = entry.name }
        let card = KCard(title: model.source?.title ?? "", symbol: model.source?.isRemote == true ? "network" : "desktopcomputer")
        card.addRow(field)
        shell.bodyStack.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        let cancel = KButton(title: String(localized: "Cancel"), style: .secondary)
        let save = KButton(title: String(localized: "Save"), style: .primary)
        save.keyEquivalent = "\r"
        cancel.onPress = { [weak self] in self?.cancel() }
        save.onPress = { [weak self] in self?.save() }
        shell.onClose = { [weak self] in self?.cancel() }
        shell.setFooterButtons([cancel, save])
    }

    func present(in window: NSWindow, onClose: @escaping () -> Void) {
        self.onClose = onClose
        let sheet = DialogSheetWindow(shell: shell)
        self.sheet = sheet
        sheet.resize(toContentSize: NSSize(width: 420, height: shell.fittingHeight(maxHeight: 480)))
        sheet.onCancel = { [weak self] in self?.cancel() }
        window.beginSheet(sheet)
        sheet.makeFirstResponder(field.textField)
        field.textField.selectText(nil)
    }

    func cancel() {
        close()
        model.cancelEditing()
    }

    private func save() {
        if field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { cancel(); return }
        Task { [weak self] in
            guard let self else { return }
            let finished = await model.commitEditing(name: field.stringValue, viaBlur: false)
            if finished { close() } else { shell.setMessage(model.notice?.text) }
        }
    }

    private func close() {
        if let sheet {
            sheet.sheetParent?.endSheet(sheet)
            sheet.orderOut(nil)
        }
        sheet = nil
        onClose?()
        onClose = nil
    }
}

/// Filer 的对话框内容（删除确认、同名冲突）。只负责布局与取值：既用于 sheet，也用于截图测试。
@MainActor
final class FilerDialogContent: NSObject {
    struct Button {
        var title: String
        var primary = false
        /// 回车触发。
        var isDefault = false
    }

    let shell: DialogShellView
    private(set) var contentSize = NSSize.zero
    private var buttons: [KButton] = []
    private var checkbox: NSButton?
    /// 用户作出选择后回调（按钮序号、复选框状态）；构造后由工厂方法设置。
    var onChoice: ((Int, Bool) -> Void)?
    private var done = false
    static let width: CGFloat = 460
    private static let maxHeight: CGFloat = 560

    var applyToRest: Bool { checkbox?.state == .on }

    /// - Parameters:
    ///   - lines: 卡片里的文字行；`mono` 行用等宽字体（路径）。
    ///   - checkboxTitle: 非 nil 时显示复选框。
    ///   - cancelIndex: Esc / 关闭按钮等价的按钮序号。
    init(
        title: String, symbol: String, cardTitle: String, cardSymbol: String, lines: [(text: String, mono: Bool, secondary: Bool)],
        checkboxTitle: String? = nil, buttons specs: [Button], cancelIndex: Int
    ) {
        shell = DialogShellView(title: title, symbol: symbol, width: Self.width)
        let card = KCard(title: cardTitle, symbol: cardSymbol)
        card.translatesAutoresizingMaskIntoConstraints = false
        shell.bodyStack.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        let textWidth = Self.width - DialogShellView.bodyInset * 2 - KCard.padding * 2
        for line in lines {
            let label = NSTextField(wrappingLabelWithString: line.text)
            label.font = line.mono ? .monospacedSystemFont(ofSize: 12, weight: .regular) : .systemFont(ofSize: DesignTokens.Font.uiBody)
            label.textColor = .token(line.secondary ? DesignTokens.Color.textSecondary : DesignTokens.Color.textPrimary)
            label.isSelectable = true
            label.preferredMaxLayoutWidth = textWidth
            card.contentStack.addArrangedSubview(label)
        }
        if let checkboxTitle {
            let box = NSButton(checkboxWithTitle: checkboxTitle, target: nil, action: nil)
            card.contentStack.addArrangedSubview(box)
            checkbox = box
        }
        super.init()
        var made: [KButton] = []
        for (index, spec) in specs.enumerated() {
            let b = KButton(title: spec.title, style: spec.primary ? .primary : .secondary)
            b.onPress = { [weak self] in self?.choose(index) }
            if spec.isDefault { b.keyEquivalent = "\r" }
            made.append(b)
        }
        buttons = made
        shell.setFooterButtons(made)
        shell.onClose = { [weak self] in self?.choose(cancelIndex) }
        contentSize = NSSize(width: Self.width, height: shell.fittingHeight(maxHeight: Self.maxHeight))
    }

    /// 只回答一次。
    func choose(_ index: Int) {
        guard !done else { return }
        done = true
        onChoice?(index, applyToRest)
    }

    var buttonTitles: [String] { buttons.map(\.title) }
}

extension FilerDialogContent {
    static func conflict(_ request: FilerConflictRequest, onChoice: @escaping (FilerConflictDecision) -> Void) -> FilerDialogContent {
        let text = request.isDirectory
            ? String(localized: "The folder “\(request.itemName)” already exists in the destination. Folders are merged; files with the same name inside follow your choice.")
            : String(localized: "“\(request.itemName)” already exists in the destination.")
        let content = FilerDialogContent(
            title: String(localized: "Item Already Exists"), symbol: "doc.on.doc", cardTitle: String(localized: "Conflict"), cardSymbol: "exclamationmark.triangle",
            lines: [(text, false, false), (request.destinationDescription, true, true)],
            checkboxTitle: String(localized: "Apply to remaining conflicts in this batch"),
            buttons: [
                .init(title: String(localized: "Cancel")),
                .init(title: String(localized: "Skip"), isDefault: true),
                .init(title: request.isDirectory ? String(localized: "Overwrite Files") : String(localized: "Overwrite"), primary: true),
            ],
            cancelIndex: 0
        )
        content.onChoice = { index, apply in
            let choice: FilerConflictChoice = index == 2 ? .overwrite : (index == 1 ? .skip : .cancel)
            onChoice(FilerConflictDecision(choice: choice, applyToRest: choice != .cancel && apply))
        }
        return content
    }

    static func delete(_ request: FilerDeleteRequest, onChoice: @escaping (Bool) -> Void) -> FilerDialogContent {
        var lines: [(text: String, mono: Bool, secondary: Bool)] = []
        if request.items.count == 1, let item = request.items.first {
            lines.append((item.name, false, false))
            lines.append((item.path, true, true))
        } else {
            lines.append((String(localized: "\(request.items.count) items"), false, false))
            for item in request.items.prefix(6) { lines.append((item.name, true, true)) }
            if request.items.count > 6 { lines.append((String(localized: "…and \(request.items.count - 6) more"), false, true)) }
        }
        lines.append((String(localized: "Location: ") + request.source.title, false, true))
        lines.append((request.warning, false, false))
        let content = FilerDialogContent(
            title: request.title, symbol: "trash", cardTitle: String(localized: "Permanent Delete"), cardSymbol: "exclamationmark.triangle",
            lines: lines, buttons: [
                .init(title: String(localized: "Cancel")),
                .init(title: String(localized: "Delete"), primary: true, isDefault: true),
            ],
            cancelIndex: 0
        )
        content.onChoice = { index, _ in onChoice(index == 1) }
        return content
    }
}

/// 生产 `FilerPrompter`：对话框以 sheet 挂在主窗口，文件选择用系统面板。
@MainActor
final class SystemFilerPrompter: FilerPrompter {
    /// 弱引用：窗口由窗口控制器持有。
    weak var window: NSWindow?

    private func present(_ content: FilerDialogContent) async {
        guard let window else {
            // 没有窗口可挂 sheet：不能让调用方一直等，按取消处理。
            content.choose(0)
            return
        }
        let sheet = DialogSheetWindow(shell: content.shell)
        sheet.resize(toContentSize: content.contentSize)
        sheet.onCancel = { [weak content] in content?.choose(0) }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let previous = content.onChoice
            content.onChoice = { index, apply in
                previous?(index, apply)
                window.endSheet(sheet)
                sheet.orderOut(nil)
                continuation.resume()
            }
            window.beginSheet(sheet)
        }
    }

    func confirmDelete(_ request: FilerDeleteRequest) async -> Bool {
        var result = false
        await present(.delete(request) { result = $0 })
        return result
    }

    func resolveConflict(_ request: FilerConflictRequest) async -> FilerConflictDecision {
        var result = FilerConflictDecision(choice: .cancel, applyToRest: false)
        await present(.conflict(request) { result = $0 })
        return result
    }

    func chooseUploadSources(directories: Bool) async -> [URL] {
        let panel = NSOpenPanel()
        panel.canChooseFiles = !directories
        panel.canChooseDirectories = directories
        panel.allowsMultipleSelection = true
        return await run(panel) ? panel.urls : []
    }

    func chooseDownloadFile(defaultName: String) async -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = defaultName
        panel.canCreateDirectories = true
        return await run(panel) ? panel.url : nil
    }

    func chooseDownloadDirectory() async -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = String(localized: "Choose")
        return await run(panel) ? panel.url : nil
    }

    func copyToPasteboard(_ text: String) -> Bool {
        let pb = NSPasteboard.general
        pb.clearContents()
        return pb.setString(text, forType: .string)
    }

    func openLocal(_ path: String) -> Bool {
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func chooseEditorApplication() async -> URL? {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.application]
        panel.directoryURL = URL(fileURLWithPath: "/Applications", isDirectory: true)
        return await run(panel) ? panel.url : nil
    }

    func openRemoteCopy(_ url: URL, application: URL?) async throws {
        guard let application = application ?? NSWorkspace.shared.urlForApplication(toOpen: url) else {
            throw FilerError(.other(String(localized: "No application can open this file")))
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            NSWorkspace.shared.open([url], withApplicationAt: application, configuration: NSWorkspace.OpenConfiguration()) { _, error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
        }
    }

    func confirmLargeRemoteEdit(name: String, source: String) async -> Bool {
        var allowed = false
        let content = FilerDialogContent(
            title: String(localized: "Open Large Remote File"), symbol: "exclamationmark.triangle",
            cardTitle: name, cardSymbol: "doc", lines: [
                (source, false, true), (String(localized: "This file is larger than 50 MB. Download and open it?"), false, false)
            ], buttons: [
                .init(title: String(localized: "Cancel")), .init(title: String(localized: "Open"), primary: true, isDefault: true)
            ], cancelIndex: 0
        )
        content.onChoice = { index, _ in allowed = index == 1 }
        await present(content)
        return allowed
    }

    func resolveRemoteEditConflict(name: String, source: String) async -> RemoteEditConflictChoice {
        var index = 0
        let content = FilerDialogContent(
            title: String(localized: "Remote File Changed"), symbol: "exclamationmark.triangle",
            cardTitle: name, cardSymbol: "doc", lines: [
                (source, false, true), (String(localized: "Sync is paused. Download Again discards your local changes."), false, false)
            ], buttons: [
                .init(title: String(localized: "Cancel")), .init(title: String(localized: "Overwrite")),
                .init(title: String(localized: "Download Again")), .init(title: String(localized: "Save a Copy…"))
            ], cancelIndex: 0
        )
        content.onChoice = { choice, _ in index = choice }
        await present(content)
        switch index {
        case 1: return .overwrite
        case 2: return .redownload
        case 3:
            let alert = NSAlert()
            alert.messageText = String(localized: "Save a Copy…")
            alert.informativeText = String(localized: "Enter a different file name")
            let input = NSTextField(string: (name as NSString).deletingPathExtension + "-copy" +
                ((name as NSString).pathExtension.isEmpty ? "" : "." + (name as NSString).pathExtension))
            input.frame = NSRect(x: 0, y: 0, width: 320, height: 24)
            alert.accessoryView = input
            alert.addButton(withTitle: String(localized: "Save"))
            alert.addButton(withTitle: String(localized: "Cancel"))
            let response = if let window { await alert.beginSheetModal(for: window) } else { alert.runModal() }
            return response == .alertFirstButtonReturn ? .saveCopy(input.stringValue) : .cancel
        default: return .cancel
        }
    }

    func showRetainedRemoteEdits(_ urls: [URL]) async {
        guard !urls.isEmpty else { return }
        let content = FilerDialogContent(
            title: String(localized: "Changes Have Not Reached the Server"), symbol: "exclamationmark.triangle",
            cardTitle: String(localized: "Local Copies Kept"), cardSymbol: "doc", lines:
                [(String(localized: "Your changes remain at these locations:"), false, false)] + urls.map { ($0.path, true, false) },
            buttons: [.init(title: String(localized: "OK"), primary: true, isDefault: true)], cancelIndex: 0
        )
        await present(content)
    }

    private func run(_ panel: NSSavePanel) async -> Bool {
        guard let window else { return panel.runModal() == .OK }
        return await withCheckedContinuation { continuation in
            panel.beginSheetModal(for: window) { continuation.resume(returning: $0 == .OK) }
        }
    }
}
