import AppKit
import KansoBridge
import KansoUI
import UniformTypeIdentifiers

/// 把 `FilerModel` 接到真实世界：工作区（跟随活动标签）、core（SFTP、传输、工作目录）、窗口（展开 / 状态栏）。
/// 一个窗口一个。
@MainActor
final class FilerCoordinator: FilerEnvironment {
    let model: FilerModel
    lazy var panel: FilerPanelView = {
        let panel = FilerPanelView(model: model)
        panel.prompter = prompter
        panel.onLocate = { [weak self] in self?.locate(session: nil) }
        panel.makePromise = { [weak self] entry in self?.makePromise(entry) }
        return panel
    }()
    let prompter = SystemFilerPrompter()
    private let controller: WorkspaceController
    private let hub = FilerTransferHub()
    private let backend: FilerRemoteBackend
    private var engines: [SessionID: RemoteTransferEngine] = [:]
    private var knownSessions: Set<SessionID> = []
    private var deliveries: [FilePromiseDelivery] = []
    private var userHostCache: [String: String] = [:]
    private(set) var dualWorkspaces: [SessionID: SftpDualView] = [:]
    private(set) var remoteEdits: RemoteEditManager?
    private var remoteEditSetupError: FilerNotice?
    let localCopyEngine: FilerTransferEngine = LocalCopyEngine()

    /// 展开 / 收起请求由窗口控制器实现。
    var onRequestShow: (() -> Void)?
    var onStatusMessage: ((FilerNotice) -> Void)?
    /// 模型状态变化（用于刷新状态栏条目）。
    var onStateChange: (() -> Void)?

    init(controller: WorkspaceController) {
        self.controller = controller
        backend = CoreRemoteBackend(core: controller.core)
        model = FilerModel(env: PlaceholderEnvironment(), prompter: prompter)
        // 先用占位环境创建 model 再换成自己：`self` 在初始化完成前不能传出。
        model.replaceEnvironment(self)
        model.onRequestShow = { [weak self] in self?.onRequestShow?() }
        model.onStatusMessage = { [weak self] in self?.onStatusMessage?($0) }
        model.setSnapshotProvider { [weak self] in self?.snapshot(of: $0) }
        do {
            remoteEdits = try RemoteEditManager(
                root: FileManager.default.temporaryDirectory.appendingPathComponent("Kanso-RemoteEdits"),
                environment: self, backend: backend, prompter: prompter
            )
        } catch {
            remoteEditSetupError = FilerNotice(text: error.localizedDescription, isError: true)
        }
        remoteEdits?.onChange = { [weak self] in self?.refreshEditStatus() }
        remoteEdits?.onNotice = { [weak self] notice in
            self?.onStatusMessage?(notice)
        }
        remoteEdits?.onSynced = { [weak self] session, directory in
            guard let self, self.model.tab?.session == session, self.model.directory == directory else { return }
            self.model.refresh(keepingNotice: true)
        }
        model.onOpenRemoteFile = { [weak self] entry, tab, application, choose in
            Task { await self?.remoteEdits?.open(entry, tab: tab, application: application, chooseApplication: choose) }
        }
        model.onCancelRemoteDownload = { [weak self] in
            if let session = self?.model.tab?.session { self?.remoteEdits?.cancelDownload(session: session) }
        }
        controller.onWillCloseSessions = { [weak self] sessions in
            guard let self, let edits = self.remoteEdits else { return }
            await self.prompter.showRetainedRemoteEdits(edits.close(sessions: sessions))
        }
        let previous = controller.onCoreEvent
        controller.onCoreEvent = { [weak self] event in
            previous?(event)
            self?.hub.handle(event)
        }
        controller.addObserver { [weak self] in self?.sync() }
        sync()
    }

    // MARK: 跟随

    func cleanPreviousEditsAfterWindowShown() {
        Task { [weak self] in
            guard let self else { return }
            do { try await remoteEdits?.cleanPreviousRuns() }
            catch {
                remoteEditSetupError = FilerNotice(text: error.localizedDescription, isError: true)
                refreshEditStatus()
            }
        }
    }

    /// 工作区变化：跟随活动 pane 的活动标签；已消失的标签清掉记忆。
    func sync() {
        let workspace = controller.workspace
        let current = Set(workspace.sidebarOrder)
        for gone in knownSessions.subtracting(current) {
            if let retained = remoteEdits?.close(sessions: [gone]), !retained.isEmpty {
                Task { await prompter.showRetainedRemoteEdits(retained) }
            }
            model.forget(session: gone)
            dualWorkspaces[gone] = nil
            engines[gone] = nil
        }
        knownSessions = current
        let visible = Set(workspace.allPanes.compactMap { workspace.activeTab(in: $0) })
        for (session, view) in dualWorkspaces {
            view.model.isVisible = visible.contains(session)
            if let tab = snapshot(of: session) { view.model.follow(tab) }
        }
        remoteEdits?.updateSessions(workspace.sidebarOrder.compactMap(snapshot(of:)))
        model.follow(workspace.activeSession.flatMap(snapshot(of:)))
        refreshEditStatus()
        onStateChange?()
    }

    func dualWorkspace(for session: SessionID) -> SftpDualView? {
        guard let tab = snapshot(of: session), tab.kind == .sftp else { return nil }
        if let view = dualWorkspaces[session] { return view }
        let view = SftpDualView(model: SftpDualModel(tab: tab, env: self, prompter: prompter), prompter: prompter)
        dualWorkspaces[session] = view
        return view
    }

    private func refreshEditStatus() {
        model.recentEditorApplications = remoteEdits?.recentApplications ?? []
        let status = model.tab.map { remoteEdits?.status(for: $0.session) } ?? nil
        model.showRemoteEditStatus(status?.0 ?? remoteEditSetupError, download: status?.1)
        if let notice = status?.0 { onStatusMessage?(notice) }
    }

    var hasRemoteEdits: Bool { remoteEdits?.edits.isEmpty == false }

    func shutdown() {
        if let remoteEdits { _ = remoteEdits.close(sessions: Array(knownSessions)) }
    }

    func snapshot(of session: SessionID) -> FilerTabSnapshot? {
        guard let item = controller.workspace.item(session) else { return nil }
        return FilerTabSnapshot(
            session: session, kind: item.kind, name: item.name, userHost: userHost(profileID: item.profileID),
            connection: item.connection, cwdHost: item.cwdHost
        )
    }

    private func userHost(profileID: String?) -> String? {
        guard let profileID else { return nil }
        if let cached = userHostCache[profileID] { return cached }
        for profile in controller.core.profiles() where profile.id == profileID {
            if case .ssh(let ssh) = profile.kind {
                let value = "\(ssh.target.user)@\(ssh.target.host)"
                userHostCache[profileID] = value
                return value
            }
        }
        return nil
    }

    // MARK: 入口动作

    /// ⌘J、终端右键菜单、面板内按钮共用。`session` 为 nil 时取当前活动标签。
    func locate(session: SessionID?) {
        let target = session ?? controller.workspace.activeSession
        Task { await model.locateWorkingDirectory(session: target) }
    }

    /// 命令面板：在 Filer 中定位某个文件（打开父目录并选中）。
    func reveal(path: String) {
        onRequestShow?()
        model.reveal(path: path)
    }

    private func makePromise(_ entry: FilerEntry) -> NSPasteboardWriting? {
        guard let tab = model.tab, tab.kind == .ssh else { return nil }
        deliveries.removeAll { $0.isFinished }
        // 任务锁定拖动开始时的会话与条目：之后切标签、再开始别的拖动都不会改写它。
        let delivery = FilePromiseDelivery(entry: entry, engine: transferEngine(for: tab)) { [weak self] status in
            self?.model.showExternalStatus(status)
        }
        deliveries.append(delivery)
        let delegate = FilerPromiseDelegate(delivery: delivery)
        let type = entry.isDirectory ? UTType.folder.identifier : (UTType(filenameExtension: (entry.name as NSString).pathExtension)?.identifier ?? UTType.data.identifier)
        let provider = FilerPromiseProvider(fileType: type, delegate: delegate)
        provider.retainedDelegate = delegate
        return provider
    }

    // MARK: FilerEnvironment

    func dataSource(for tab: FilerTabSnapshot) -> FilerDataSource {
        tab.kind == .local ? LocalFilerDataSource() : RemoteFilerDataSource(session: tab.session, backend: backend)
    }

    func transferEngine(for tab: FilerTabSnapshot) -> FilerTransferEngine {
        if let e = engines[tab.session] { return e }
        let e = RemoteTransferEngine(core: controller.core, session: tab.session, hub: hub)
        engines[tab.session] = e
        return e
    }

    func workingDirectory(session: SessionID) async throws -> String {
        try await controller.core.workingDirectory(sessionId: session)
    }

    func localHostNames() -> Set<String> {
        FilerModel.localHostNames
    }
}

/// `FilerModel` 构造时的占位环境（构造完成后换成协调者）。
@MainActor
private final class PlaceholderEnvironment: FilerEnvironment {
    func dataSource(for tab: FilerTabSnapshot) -> FilerDataSource { LocalFilerDataSource() }
    func transferEngine(for tab: FilerTabSnapshot) -> FilerTransferEngine { LocalCopyEngine() }
    var localCopyEngine: FilerTransferEngine { LocalCopyEngine() }
    func workingDirectory(session: SessionID) async throws -> String { throw FilerError(.disconnected) }
    func localHostNames() -> Set<String> { [] }
}
