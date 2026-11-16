import AppKit
import KansoBridge

/// 生产显示层：把提问对话框作为 sheet 挂在主窗口上。
@MainActor
final class WindowPromptSheetHost: PromptSheetHost {
    /// 弱引用：窗口由窗口控制器持有，反向强引用会形成循环。
    weak var window: NSWindow?

    /// 对话框宽度；卡片内文字宽度由它推出，超出自动换行。
    private static let dialogWidth: CGFloat = 480

    private var sheet: NSWindow?
    private var filePanel: NSSavePanel?
    /// 按钮回调弱引用 builder，必须由本类持有到 sheet 关闭。
    private var builder: AnyObject?
    private var completion: (@MainActor (FfiPromptResponse) -> Void)?

    var canPresent: Bool { window != nil }

    func present(_ model: PromptDialogModel, completion: @escaping @MainActor (FfiPromptResponse) -> Void) {
        guard let window else { return }
        self.completion = completion
        if case .transfer(let request) = model {
            presentTransfer(request, window: window)
            return
        }
        let builder = PromptSheetBuilder(model: model, width: Self.dialogWidth) { [weak self] response in
            self?.finish(response)
        }
        let sheet = DialogSheetWindow(shell: builder.shell)
        sheet.resize(toContentSize: builder.contentSize)
        // Esc 等同点"取消"：普通无边框窗口的 sheet 不会自己响应 Esc。
        sheet.onCancel = { [weak builder] in builder?.cancel() }
        self.builder = builder
        sheet.initialFirstResponder = builder.initialFirstResponder
        self.sheet = sheet
        window.beginSheet(sheet)
    }

    func dismissCurrent() {
        completion = nil
        closeSheet()
    }

    private func finish(_ response: FfiPromptResponse) {
        let completion = self.completion
        self.completion = nil
        closeSheet()
        completion?(response)
    }

    private func closeSheet() {
        if let panel = filePanel {
            filePanel = nil
            panel.cancel(nil)
        }
        guard let sheet else { return }
        self.sheet = nil
        builder = nil
        window?.endSheet(sheet)
        sheet.orderOut(nil)
    }

    private func presentTransfer(_ request: FfiPromptRequest, window: NSWindow) {
        switch request {
        case .zmodemPickFiles, .xmodemPickFile:
            let panel = NSOpenPanel()
            panel.canChooseFiles = true
            panel.canChooseDirectories = false
            panel.allowsMultipleSelection = request == .zmodemPickFiles
            panel.message = String(localized: "Select files to send…")
            filePanel = panel
            panel.beginSheetModal(for: window) { [weak self] result in
                guard let self, self.filePanel === panel else { return }
                self.filePanel = nil
                self.finish(result == .OK ? .files(paths: panel.urls.map(\.path)) : .cancel)
            }
        case .zmodemSaveFile, .xmodemSaveFile:
            let panel = NSSavePanel()
            if case .zmodemSaveFile(let name, _) = request {
                panel.nameFieldStringValue = name
            }
            panel.canCreateDirectories = true
            filePanel = panel
            panel.beginSheetModal(for: window) { [weak self] result in
                guard let self, self.filePanel === panel else { return }
                self.filePanel = nil
                self.finish(result == .OK ? .savePath(path: panel.url!.path) : .cancel)
            }
        default: preconditionFailure("文件面板只接收终端传输提问")
        }
    }
}

/// 按 `PromptDialogModel` 搭建对话框内容（对话框外壳 + 卡片 + 输入框 + 按钮）。
/// 只做布局与取值，数据与回答在 view model 里。
@MainActor
private final class PromptSheetBuilder: NSObject {
    let shell: DialogShellView
    private(set) var initialFirstResponder: NSView?
    /// 对话框内容尺寸（外壳量好后的结果）。
    private(set) var contentSize = NSSize.zero

    private let model: PromptDialogModel
    private let width: CGFloat
    private let textWidth: CGFloat
    private let onResponse: (FfiPromptResponse) -> Void
    private var fields: [KTextField] = []
    private var rememberButton: NSButton?
    private let primaryButton = KButton(style: .primary)
    private let cancelButton = KButton(style: .secondary)
    private var done = false

    /// 屏幕较矮时主体滚动，不让对话框超出。
    private static let maxHeight: CGFloat = 640

    init(model: PromptDialogModel, width: CGFloat, onResponse: @escaping (FfiPromptResponse) -> Void) {
        self.model = model
        self.onResponse = onResponse
        self.width = width
        // 卡片内文字宽度 = 对话框宽 - 主体左右边距 - 卡片左右内边距。
        self.textWidth = width - DialogShellView.bodyInset * 2 - KCard.padding * 2
        let (title, symbol): (String, String) = switch model {
        case .hostKeyChanged: (String(localized: "Host Key Changed"), "exclamationmark.triangle")
        case .keyboardInteractive: (String(localized: "Verification Required"), "lock.shield")
        case .credential(let m):
            (m.isPassphrase ? String(localized: "Key Passphrase Required") : String(localized: "Password Required"), "key")
        case .transfer: preconditionFailure("传输提问使用系统文件面板")
        }
        shell = DialogShellView(title: title, symbol: symbol, width: width)
        super.init()
        build()
    }

    // MARK: - 布局

    private func build() {
        switch model {
        case .hostKeyChanged(let m): addHostKey(m)
        case .keyboardInteractive(let m): addKeyboardInteractive(m)
        case .credential(let m): addCredential(m)
        case .transfer: break
        }
        addButtons()
        shell.onClose = { [weak self] in self?.cancel() }
        contentSize = NSSize(width: width, height: shell.fittingHeight(maxHeight: Self.maxHeight))
    }

    private func addCard(_ title: String, symbol: String) -> KCard {
        let card = KCard(title: title, symbol: symbol)
        card.translatesAutoresizingMaskIntoConstraints = false
        shell.bodyStack.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        return card
    }

    /// 普通文字行。`selectable` 用于服务器原文与指纹，方便复制。
    @discardableResult
    private func text(_ text: String, to card: KCard, mono: Bool = false, secondary: Bool = false) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = mono ? .monospacedSystemFont(ofSize: 12, weight: .regular) : .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.textColor = .token(secondary ? DesignTokens.Color.textSecondary : DesignTokens.Color.textPrimary)
        label.isSelectable = true
        label.preferredMaxLayoutWidth = textWidth
        card.contentStack.addArrangedSubview(label)
        return label
    }

    /// "是谁在问"：host:port、经由跳板、关联标签。
    private func addTarget(_ target: PromptTarget, to card: KCard) {
        text(target.hostPort, to: card, mono: true)
        if let via = target.viaText { text(via, to: card, secondary: true) }
        if let session = target.sessionTitle {
            text(String(localized: "Session: \(session)"), to: card, secondary: true)
        }
    }

    private func addHostKey(_ m: HostKeyPromptModel) {
        let server = addCard(String(localized: "Server"), symbol: "server.rack")
        addTarget(m.target, to: server)
        let key = addCard(String(localized: "Host Key"), symbol: "key")
        text(String(localized: "The host key of this server no longer matches the one on record. This could be a man-in-the-middle attack, or the server may have been reinstalled. Only continue if you trust the change."), to: key)
        text(String(localized: "Key type"), to: key, secondary: true)
        text(m.keyType, to: key, mono: true)
        text(String(localized: "SHA256 fingerprint"), to: key, secondary: true)
        text(m.fingerprint, to: key, mono: true)
        text(String(localized: "Previous key recorded at"), to: key, secondary: true)
        text(m.knownHostsLocation, to: key, mono: true)
    }

    private func addKeyboardInteractive(_ m: KeyboardInteractivePromptModel) {
        let server = addCard(String(localized: "Server"), symbol: "server.rack")
        addTarget(m.target, to: server)
        text(String(localized: "User: \(m.user)"), to: server, secondary: true)
        let card = addCard(String(localized: "Verification"), symbol: "lock.shield")
        // 以下均为服务器原文，不本地化。
        if let name = m.name { text(name, to: card) }
        if let instruction = m.instruction { text(instruction, to: card) }
        for line in m.preamble { text(line, to: card) }
        for (index, field) in m.fields.enumerated() {
            text(field.prompt, to: card)
            let input = makeInput(secure: field.isSecure)
            input.textField.tag = index
            card.addRow(input)
            fields.append(input)
        }
        initialFirstResponder = fields.first?.textField
    }

    private func addCredential(_ m: CredentialPromptModel) {
        let server = addCard(String(localized: "Server"), symbol: "server.rack")
        addTarget(m.target, to: server)
        text(String(localized: "User: \(m.user)"), to: server, secondary: true)
        if let path = m.keyPath { text(String(localized: "Key: \(path)"), to: server, mono: true) }
        let card = addCard(String(localized: "Credential"), symbol: "key")
        text(m.fieldLabel, to: card, secondary: true)
        let input = makeInput(secure: true)
        card.addRow(input)
        fields.append(input)
        initialFirstResponder = input.textField
        let remember = NSButton(checkboxWithTitle: m.rememberTitle, target: nil, action: nil)
        remember.state = m.remember ? .on : .off
        card.contentStack.addArrangedSubview(remember)
        rememberButton = remember
    }

    private func makeInput(secure: Bool) -> KTextField {
        // echo == false 必须用 NSSecureTextField（KTextField(secure:) 内部使用）而不是自己把文字遮起来：
        // 它由系统保证不回显并启用 secure event input。
        KTextField(secure: secure)
    }

    private func addButtons() {
        let primaryTitle: String, cancelTitle: String, defaultIsCancel: Bool
        switch model {
        case .hostKeyChanged(let m):
            (primaryTitle, cancelTitle, defaultIsCancel) = (m.acceptTitle, m.cancelTitle, m.defaultButton == .cancel)
        case .keyboardInteractive(let m):
            (primaryTitle, cancelTitle, defaultIsCancel) = (m.continueTitle, m.cancelTitle, m.defaultButton == .cancel)
        case .credential(let m):
            (primaryTitle, cancelTitle, defaultIsCancel) = (m.connectTitle, m.cancelTitle, m.defaultButton == .cancel)
        case .transfer: preconditionFailure("传输提问使用系统文件面板")
        }
        primaryButton.title = primaryTitle
        cancelButton.title = cancelTitle
        primaryButton.onPress = { [weak self] in self?.primaryClicked() }
        cancelButton.onPress = { [weak self] in self?.cancel() }
        // 回车触发的按钮：主机密钥对话框是"取消"（危险操作不能被一次回车确认），其余是主按钮。
        (defaultIsCancel ? cancelButton : primaryButton).keyEquivalent = "\r"
        shell.setFooterButtons([cancelButton, primaryButton])
    }

    // MARK: - 动作

    private func primaryClicked() {
        switch model {
        case .hostKeyChanged(let m):
            send(m.acceptResponse)
        case .keyboardInteractive(let m):
            for field in fields { m.setAnswer(field.stringValue, at: field.textField.tag) }
            send(m.continueResponse)
        case .credential(let m):
            m.setSecret(fields.first?.stringValue ?? "")
            m.remember = rememberButton?.state == .on
            send(m.connectResponse)
        case .transfer: break
        }
    }

    func cancel() { send(model.cancelResponse) }

    /// 只回答一次；回答后立刻清空输入框，缩短明文在内存里的停留时间。
    private func send(_ response: FfiPromptResponse) {
        guard !done else { return }
        done = true
        for field in fields { field.stringValue = "" }
        onResponse(response)
    }
}
