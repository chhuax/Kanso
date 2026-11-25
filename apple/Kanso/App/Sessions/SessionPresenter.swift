import AppKit
import KansoBridge
import KansoUI

/// 会话启动器（实色浮动面板）与编辑对话框（sheet）的呈现与接线。
///
/// 由 `WorkspaceController` 持有；入口（命令面板或空白处右键打开启动器，＋ 与 ⌘N 打开新建会话，标签右键编辑）都调它，
/// 保证同一时刻最多一个浮层、一个编辑 sheet。
@MainActor
final class SessionPresenter {
    private unowned let controller: WorkspaceController
    let dialogs: SessionDialogs
    weak var window: NSWindow? {
        didSet { (dialogs as? AlertSessionDialogs)?.window = window }
    }

    private var panel: FloatingPanel?
    /// 面板内容的控制器：面板只持有它的 view，控制器（搜索 / 键盘 / 数据）必须由这里持有到面板关闭。
    private var launcherContent: SessionLauncherViewController?
    /// 最近一次浮层的锚点，供"弹出对话框后重新打开启动器"使用。
    private var anchor: (view: NSView, rect: NSRect, edge: NSRectEdge)?
    private var collapsedGroupIDs: Set<String> = []
    private var editorSheet: NSWindow?
    /// 编辑对话框的控制器：sheet 的 contentView 是它的 view，控制器本身要保持存活。
    private var editorController: SessionEditorViewController?
    /// 导入对话框 / 结果对话框的 sheet 与控制器（同一时刻最多一个，与编辑 sheet 互斥）。
    private var dialogSheet: NSWindow?
    private var dialogOwner: AnyObject?
    /// 导入 / 导出用的文件面板；测试替换为不弹窗的替身。
    var filePanels: DataFilePanels = SystemDataFilePanels()
    var onDataImported: (() -> Void)?
    /// 编辑对话框与启动器是否显示「MCP 访问」。由主窗口注入 `McpCoordinator.supported`，这里不直接问渠道。
    var mcpSupported: () -> Bool = { false }

    init(controller: WorkspaceController, dialogs: SessionDialogs = AlertSessionDialogs()) {
        self.controller = controller
        self.dialogs = dialogs
    }

    var store: SessionStore { controller.core }

    // MARK: - 启动器

    /// 弹出启动器。已经显示时先关掉再开（右键换位置）。面板总是出现在锚点下方，`preferredEdge` 只用于
    /// 记住锚点，供对话框结束后重新打开。
    func showLauncher(relativeTo rect: NSRect, of view: NSView, preferredEdge: NSRectEdge) {
        closeLauncher()
        guard let hostWindow = view.window ?? window else { return }
        anchor = (view, rect, preferredEdge)
        // MCP 菜单与标记只在渠道支持时出现：传 nil 即关掉，与编辑对话框的 `showsMcpAccess` 同一个判断来源。
        let model = SessionLauncherModel(
            store: store, mcpAccess: mcpSupported() ? controller.core : nil, collapsedGroupIDs: collapsedGroupIDs
        )
        model.onCollapsedGroupsChange = { [weak self] in self?.collapsedGroupIDs = $0 }
        let actions = SessionLauncherActions(
            newLocalShell: { [weak self] in
                self?.closeLauncher()
                self?.controller.perform { try self?.controller.openLocalShell() }
            },
            newSession: { [weak self] in
                self?.closeLauncher()
                self?.presentEditor(editing: nil)
            },
            connect: { [weak self] id in
                self?.closeLauncher()
                self?.controller.perform { try self?.controller.openProfile(id) }
                self?.controller.requestTerminalFocus?()
            },
            edit: { [weak self] profile in
                self?.closeLauncher()
                self?.presentEditor(editing: profile)
            },
            dismiss: { [weak self] in self?.closeLauncher() },
            importSshConfig: { [weak self] in
                self?.closeLauncher()
                self?.presentSshImport()
            },
            openSettings: { [weak self] in
                self?.closeLauncher()
                self?.presentEditor(editing: nil, customLocal: true)
            },
            // 弹确认框 / 输入框（sheet）会让主窗口失去 key，面板随之关闭，所以先主动关、做完再按原锚点重开。
            withDialog: { [weak self] work in
                guard let self else { return }
                closeLauncher()
                Task { @MainActor in
                    await work()
                    self.controller.refresh()
                    // 操作完成后回到启动器，方便连续整理分组。
                    if let anchor = self.anchor, anchor.view.window != nil {
                        self.showLauncher(relativeTo: anchor.rect, of: anchor.view, preferredEdge: anchor.edge)
                    }
                }
            }
        )
        let content = SessionLauncherViewController(model: model, dialogs: dialogs, actions: actions)
        _ = content.view
        let panel = FloatingPanel(content: content.view, size: content.preferredContentSize)
        content.onPreferredSizeChange = { [weak panel] size in panel?.resize(to: size) }
        panel.onDismiss = { [weak self, weak panel] in
            // 只清理自己：右键换位置时旧面板的关闭回调晚于新面板的创建，不能把新面板的引用清掉。
            guard let self, self.panel === panel else { return }
            self.panel = nil
            launcherContent = nil
        }
        self.panel = panel
        launcherContent = content
        let screenRect = hostWindow.convertToScreen(view.convert(rect, to: nil))
        panel.present(below: screenRect, parent: hostWindow)
        content.focusSearchField()
    }

    func closeLauncher() {
        panel?.dismiss()
        panel = nil
        launcherContent = nil
    }

    /// 构造嵌入左侧栏「会话」页的会话管理控制器，共享当前窗口的分组折叠状态、MCP 支持判断与各类编辑对话框。
    func makeEmbeddedSessionsViewController(
        onDidOpen: @escaping @MainActor () -> Void,
        onDismiss: @escaping @MainActor () -> Void
    ) -> SessionLauncherViewController {
        let model = SessionLauncherModel(
            store: store, mcpAccess: mcpSupported() ? controller.core : nil, collapsedGroupIDs: collapsedGroupIDs
        )
        model.onCollapsedGroupsChange = { [weak self] in self?.collapsedGroupIDs = $0 }
        let actions = SessionLauncherActions(
            newLocalShell: { [weak self] in
                self?.controller.perform { try self?.controller.openLocalShell() }
                onDidOpen()
                self?.controller.requestTerminalFocus?()
            },
            newSession: { [weak self] in
                self?.presentEditor(editing: nil)
            },
            connect: { [weak self] id in
                self?.controller.perform { try self?.controller.openProfile(id) }
                onDidOpen()
                self?.controller.requestTerminalFocus?()
            },
            edit: { [weak self] profile in
                self?.presentEditor(editing: profile)
            },
            dismiss: onDismiss,
            importSshConfig: { [weak self] in
                self?.presentSshImport()
            },
            openSettings: { [weak self] in
                self?.presentEditor(editing: nil, customLocal: true)
            },
            withDialog: { [weak self] work in
                guard let self else { return }
                Task { @MainActor in
                    await work()
                    self.controller.refresh()
                }
            }
        )
        return SessionLauncherViewController(model: model, dialogs: dialogs, actions: actions, embedded: true)
    }

    // MARK: - 编辑对话框

    /// 以 sheet 打开编辑对话框；`profile` 为 nil 表示新建。已有 sheet 时忽略（避免叠多个）。
    func presentEditor(editing profile: FfiProfile?, customLocal: Bool = false) {
        guard editorSheet == nil, let window else { return }
        let model = SessionEditorModel(
            store: store, editing: profile,
            showsMcpAccess: mcpSupported(), mcpAccessStore: controller.core
        )
        if customLocal {
            let defaults = controller.core.localShellDefaults()
            model.shellCommand = defaults.shellCommand
            model.startDir = defaults.startDir ?? ""
            model.encoding = defaults.encoding
            model.locale = defaults.locale ?? ""
        } else if profile == nil {
            model.kind = .ssh
        }
        let editor = SessionEditorViewController(model: model, customLocal: customLocal, remoteOnly: profile == nil && !customLocal)
        // 先加载视图再量尺寸；主体放不下时滚动，所以按主窗口高度留出上下边距。
        editor.maxHeight = max(360, window.frame.height - 120)
        _ = editor.view
        let sheet = DialogSheetWindow(shell: editor.view)
        sheet.resize(toContentSize: editor.preferredContentSize)
        editorSheet = sheet
        editorController = editor
        let finish: () -> Void = { [weak self, weak sheet] in
            // 取消、保存成功、Esc 可能先后触发多次，只收一次。
            guard let self, let sheet, editorSheet === sheet else { return }
            window.endSheet(sheet)
            sheet.orderOut(nil)
            editorSheet = nil
            editorController = nil
            controller.refresh()
        }
        editor.onFinish = finish
        sheet.onCancel = finish
        editor.onLocalShellAction = { [weak self] profile, open in
            guard let self else { return }
            if open {
                try controller.openLocalShell(profile: profile)
                controller.requestTerminalFocus?()
            } else {
                try controller.core.setLocalShellDefaults(profile: profile)
            }
        }
        editor.onConnect = { [weak self] id in
            guard let self else { return }
            controller.perform { try self.controller.openProfile(id) }
            controller.requestTerminalFocus?()
        }
        window.beginSheet(sheet)
    }

    /// 编辑已保存的会话（侧栏标签右键）。会话已被删除时什么也不做。
    func editProfile(id: String) {
        guard let profile = store.profiles().first(where: { $0.id == id }) else { return }
        presentEditor(editing: profile)
    }

    // MARK: - 导入 OpenSSH 配置 / 导出与导入数据

    /// 是否已有 sheet（编辑 / 导入 / 结果）：同一时刻只允许一个。
    private var hasSheet: Bool { editorSheet != nil || dialogSheet != nil }

    /// 以 sheet 打开"导入 OpenSSH 配置"对话框。已有 sheet 时忽略。
    func presentSshImport() {
        guard !hasSheet, let window else { return }
        let model = SshConfigImportModel(core: controller.core)
        let content = SshConfigImportViewController(model: model, panels: filePanels)
        content.onImported = { [weak self] in self?.controller.refresh() }
        content.maxHeight = max(360, window.frame.height - 120)
        _ = content.view
        presentDialog(shell: content.view, size: content.preferredContentSize, owner: content) { finish in
            content.onFinish = finish
        }
        content.startScan()
    }

    /// 会话 → 导出数据…：选位置 -> 导出 -> 成功给结果对话框，失败给错误提示。
    func exportData() {
        guard !hasSheet else { return }
        let flow = DataTransferFlow(core: controller.core, panels: filePanels)
        Task { @MainActor [weak self] in
            guard let self else { return }
            switch await flow.export(window: window) {
            case .cancelled:
                break
            case .exported(let path):
                let content = ImportResultContent(
                    summary: String(localized: "Sessions, groups, snippets and settings were exported to “\(path)”. Credentials are not included."),
                    detailsTitle: nil, details: []
                )
                presentResult(title: String(localized: "Export Complete"), symbol: "square.and.arrow.up", content: content)
            case .failed(let message):
                dialogs.showError(title: String(localized: "Export failed"), message: message)
            }
        }
    }

    /// 会话 → 导入数据…：选 `.kanso` 文件 -> 导入 -> 结果对话框（新增 / 更新 / 跳过及原因）。
    func importData() {
        guard !hasSheet else { return }
        let flow = DataTransferFlow(core: controller.core, panels: filePanels)
        Task { @MainActor [weak self] in
            guard let self else { return }
            switch await flow.importData(window: window) {
            case .cancelled:
                break
            case .imported(let content):
                self.onDataImported?()
                self.controller.refresh()
                presentResult(title: String(localized: "Import Complete"), symbol: "square.and.arrow.down", content: content)
            case .failed(let message):
                dialogs.showError(title: String(localized: "Import failed"), message: message)
            }
        }
    }

    private func presentResult(title: String, symbol: String, content: ImportResultContent) {
        guard !hasSheet, let window else { return }
        let result = DataResultViewController(
            title: title, symbol: symbol, content: content, maxHeight: max(360, window.frame.height - 120)
        )
        _ = result.view
        presentDialog(shell: result.view, size: result.preferredContentSize, owner: result) { finish in
            result.onFinish = finish
        }
    }

    /// 呈现一个对话框 sheet。`wire` 收到"收起并释放"的闭包，由调用方接到控制器的完成回调上；
    /// 取消、Esc、× 可能先后触发多次，只收一次。
    private func presentDialog(shell: NSView, size: NSSize, owner: AnyObject, wire: (@escaping () -> Void) -> Void) {
        guard let window else { return }
        let sheet = DialogSheetWindow(shell: shell)
        sheet.resize(toContentSize: size)
        dialogSheet = sheet
        dialogOwner = owner
        let finish: () -> Void = { [weak self, weak sheet] in
            guard let self, let sheet, dialogSheet === sheet else { return }
            window.endSheet(sheet)
            sheet.orderOut(nil)
            dialogSheet = nil
            dialogOwner = nil
        }
        wire(finish)
        sheet.onCancel = finish
        window.beginSheet(sheet)
    }
}
