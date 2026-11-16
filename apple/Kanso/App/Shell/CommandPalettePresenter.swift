import AppKit
import KansoBridge
import KansoUI

/// 标题栏中央的命令面板入口：外观像搜索框，点击或 ⌘⇧P 打开浮层。
@MainActor
final class CommandPaletteEntryView: NSView {
    var onActivate: (() -> Void)?

    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: "")

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = DesignTokens.Radius.md
        layer?.borderWidth = 1

        icon.image = .symbol("magnifyingglass")
        icon.symbolConfiguration = .init(pointSize: DesignTokens.Font.uiCaption, weight: .regular)
        icon.contentTintColor = .token(DesignTokens.Color.textTertiary)

        label.stringValue = String(localized: "Search")
        label.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        label.textColor = .token(DesignTokens.Color.textTertiary)
        label.lineBreakMode = .byTruncatingTail

        for v in [icon, label] { addSubview(v) }
        toolTip = String(localized: "Command Palette")
        setAccessibilityRole(.button)
        setAccessibilityLabel(String(localized: "Command Palette"))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("CommandPaletteEntryView 只支持代码创建") }

    override var intrinsicContentSize: NSSize { NSSize(width: 240, height: 22) }

    override func updateLayer() {
        layer?.backgroundColor = resolvedCGColor(.token(DesignTokens.Color.bgField))
        layer?.borderColor = resolvedCGColor(.token(DesignTokens.Color.borderField))
    }

    override func layout() {
        super.layout()
        let pad: CGFloat = 8
        let iconSize: CGFloat = 12
        icon.frame = NSRect(x: pad, y: (bounds.height - iconSize) / 2, width: iconSize, height: iconSize)
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(
            x: icon.frame.maxX + 6, y: (bounds.height - h) / 2,
            width: max(0, bounds.width - icon.frame.maxX - 6 - pad), height: h
        )
    }

    override func mouseDown(with event: NSEvent) { onActivate?() }

    override func accessibilityPerformPress() -> Bool {
        onActivate?()
        return true
    }
}

/// 命令面板的呈现与接线：FloatingPanel + 四类结果收集 + 动作分发。
@MainActor
final class CommandPalettePresenter {
    private unowned let windowController: MainWindowController
    private var panel: FloatingPanel?
    private var content: CommandPaletteViewController?
    private let entryView = CommandPaletteEntryView()
    var fileLister: @Sendable (String) -> [CommandPaletteSnapshot.File] = { CommandPaletteFileLister.list(directory: $0) }
    private var fileTask: Task<Void, Never>?
    /// 主菜单来源；默认 `NSApp.mainMenu`，测试注入。
    var menuProvider: () -> NSMenu? = { NSApp.mainMenu }

    init(windowController: MainWindowController) {
        self.windowController = windowController
        entryView.onActivate = { [weak self] in self?.toggle() }
    }

    /// 把入口装到主窗口标题栏中央。
    func installTitlebarEntry(on window: NSWindow) {
        // 直接放进标题栏视图并水平居中。不用 `.bottom` 附件：那会在标题栏下面再加一条，把内容整体往下推。
        // 标题栏视图是关闭按钮的父视图；取不到（理论上不会）就不放入口，⌘⇧P 仍可打开面板。
        guard let titlebar = window.standardWindowButton(.closeButton)?.superview else { return }
        entryView.translatesAutoresizingMaskIntoConstraints = false
        titlebar.addSubview(entryView)
        NSLayoutConstraint.activate([
            entryView.centerXAnchor.constraint(equalTo: titlebar.centerXAnchor),
            entryView.centerYAnchor.constraint(equalTo: titlebar.centerYAnchor),
            entryView.widthAnchor.constraint(equalToConstant: 240),
            entryView.heightAnchor.constraint(equalToConstant: 22),
        ])
    }

    var isOpen: Bool { panel != nil }

    func toggle() {
        if isOpen { close() } else { open() }
    }

    func open() {
        close()
        windowController.controller.sessions.closeLauncher()
        guard let hostWindow = windowController.window else { return }

        let model = CommandPaletteModel()
        model.actions = CommandPaletteModel.Actions(
            activateTab: { [weak self] id in
                self?.windowController.controller.workspace.activate(id)
                self?.windowController.controller.requestTerminalFocus?()
            },
            openSession: { [weak self] id in
                self?.windowController.controller.perform {
                    try self?.windowController.controller.openProfile(id)
                }
                self?.windowController.controller.requestTerminalFocus?()
            },
            runCommand: { [weak self] id in
                guard let self, let item = CommandPaletteMenuCollector.findItem(id: id, in: self.menuProvider()) else { return }
                guard let action = item.action else { return }
                let target = item.target ?? NSApp.target(forAction: action, to: nil, from: item)
                _ = NSApp.sendAction(action, to: target, from: item)
            },
            revealFile: { [weak self] path in
                self?.windowController.setFilerCollapsed(false)
                self?.windowController.filer.reveal(path: path)
            },
            dismiss: { [weak self] in self?.close() }
        )
        model.replaceSnapshot(buildSnapshot())

        let content = CommandPaletteViewController(model: model)
        _ = content.view
        let panel = FloatingPanel(content: content.view, size: content.preferredContentSize)
        content.onPreferredSizeChange = { [weak panel] size in panel?.resize(to: size) }
        panel.onDismiss = { [weak self, weak panel] in
            guard let self, self.panel === panel else { return }
            self.fileTask?.cancel()
            self.fileTask = nil
            self.panel = nil
            self.content = nil
        }
        self.panel = panel
        self.content = content

        let entryScreen = hostWindow.convertToScreen(entryView.convert(entryView.bounds, to: nil))
        let width = content.preferredContentSize.width
        let anchor = NSRect(
            x: entryScreen.midX - width / 2, y: entryScreen.minY, width: width, height: 0
        )
        panel.present(below: anchor, parent: hostWindow)
        content.focusSearchField()
        if let directory = localDirectory {
            let list = fileLister
            fileTask = Task { [weak self, weak content] in
                let files = await CommandPaletteFileLister.load(directory: directory, list: list)
                guard !Task.isCancelled, let self, let content, self.content === content else { return }
                model.replaceFiles(files)
                content.reload()
            }
        }
    }

    func close() {
        fileTask?.cancel()
        fileTask = nil
        panel?.dismiss()
        panel = nil
        content = nil
    }

    /// 从当前窗口状态收集四类结果。
    func buildSnapshot() -> CommandPaletteSnapshot {
        let workspace = windowController.controller.workspace
        let home = NSHomeDirectory()
        var snap = CommandPaletteSnapshot()

        for (index, id) in workspace.sidebarOrder.enumerated() {
            guard let item = workspace.item(id) else { continue }
            snap.tabs.append(.init(
                id: id,
                title: TabPresentation.primaryTitle(item, homeDirectory: home),
                detail: TabPresentation.shortcutLabel(sidebarIndex: index)
            ))
        }

        let store = windowController.controller.core
        for profile in store.profiles() {
            snap.sessions.append(.init(
                id: profile.id,
                title: profile.name,
                detail: SessionLauncherModel.subtitle(for: profile),
                color: profile.color
            ))
        }

        snap.commands = CommandPaletteMenuCollector.collect(from: menuProvider())
            .filter { $0.id != KeyCommand.commandPalette.menuIdentifier.rawValue }

        return snap
    }

    private var localDirectory: String? {
        let workspace = windowController.controller.workspace
        guard let session = workspace.activeSession, let item = workspace.item(session),
              item.kind == .local, let cwd = item.cwd, !cwd.isEmpty else { return nil }
        return cwd
    }
}
