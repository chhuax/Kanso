import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

private final class SftpTestDefaults: UserDefaults, @unchecked Sendable {
    private var values: [String: Any] = [:]

    override func object(forKey key: String) -> Any? { values[key] }
    override func bool(forKey key: String) -> Bool { values[key] as? Bool ?? false }
    override func string(forKey key: String) -> String? { values[key] as? String }
    override func set(_ value: Any?, forKey key: String) { values[key] = value }
    override func set(_ value: Bool, forKey key: String) { values[key] = value }
    override func removeObject(forKey key: String) { values[key] = nil }
}

@MainActor
final class SftpDualTests: XCTestCase {
    private func make(connection: TabConnection = .connected) async -> (SftpDualModel, FakeEnv, FakePrompter) {
        let env = FakeEnv(), prompter = FakePrompter()
        let remote = FakeDataSource(remote: true), local = FakeDataSource(remote: false)
        remote.home = "/remote/home"
        local.home = "/local/home"
        for source in [remote, local] {
            source.dirs[source.home] = [.dir("docs", in: source.home), .file("notes.txt", in: source.home), .link("link", in: source.home)]
            source.dirs[source.home + "/docs"] = [.file("report.txt", in: source.home + "/docs")]
        }
        env.sources[1] = remote
        env.localSource = local
        let tab = FilerTabSnapshot(session: 1, kind: .sftp, name: "server", userHost: "demo@server.example.com", connection: connection)
        let model = SftpDualModel(tab: tab, env: env, prompter: prompter)
        await settle(model)
        return (model, env, prompter)
    }

    private func settle(_ model: SftpDualModel) async {
        await model.remote.waitUntilIdle()
        await model.local.waitUntilIdle()
    }

    func testFirstConnectionLoadsBothHomesAndIdentity() async {
        let (model, env, _) = await make()
        XCTAssertEqual(model.remote.directory, "/remote/home")
        XCTAssertEqual(model.local.directory, "/local/home")
        XCTAssertEqual(env.sources[1]?.homeCalls, 1)
        XCTAssertEqual(env.localSource?.homeCalls, 1)
        XCTAssertTrue(model.remote.source?.isRemote == true)
        XCTAssertFalse(model.local.source?.isRemote == true)
        XCTAssertEqual(model.remote.availability, .available)
    }

    func testNavigationAndFileActivationOnBothPanes() async throws {
        let (model, _, prompter) = await make()
        for pane in [model.remote, model.local] {
            let home = try XCTUnwrap(pane.directory)
            pane.activate(try XCTUnwrap(pane.entries.first { $0.name == "notes.txt" }))
            XCTAssertEqual(pane.directory, home)
            XCTAssertNil(pane.notice)
            pane.activate(try XCTUnwrap(pane.entries.first { $0.isDirectory }))
            await pane.waitUntilIdle()
            XCTAssertEqual(pane.directory, home + "/docs")
            pane.goUp()
            await pane.waitUntilIdle()
            XCTAssertEqual(pane.directory, home)
            pane.navigate(toInput: "docs")
            await pane.waitUntilIdle()
            XCTAssertEqual(pane.directory, home + "/docs")
            pane.goHome()
            await pane.waitUntilIdle()
            XCTAssertEqual(pane.directory, home)
        }
        XCTAssertTrue(prompter.opened.isEmpty)
    }

    func testHiddenWorkspaceKeepsBothLocationsAndSelections() async {
        let (model, env, _) = await make()
        model.remote.open(directory: "/remote/home/docs")
        model.local.open(directory: "/local/home/docs")
        await settle(model)
        model.remote.setSelection(paths: ["/remote/home/docs/report.txt"])
        model.local.setSelection(paths: ["/local/home/docs/report.txt"])
        let remoteReads = env.sources[1]?.lists, localReads = env.localSource?.lists
        model.isVisible = false
        model.follow(model.tab)
        model.isVisible = true
        await settle(model)
        XCTAssertEqual(model.remote.directory, "/remote/home/docs")
        XCTAssertEqual(model.local.directory, "/local/home/docs")
        XCTAssertEqual(model.remote.selectedPaths, ["/remote/home/docs/report.txt"])
        XCTAssertEqual(model.local.selectedPaths, ["/local/home/docs/report.txt"])
        XCTAssertEqual(env.sources[1]?.lists, remoteReads)
        XCTAssertEqual(env.localSource?.lists, localReads)
    }

    func testConnectingClosedFailureAndReconnectUseSessionState() async {
        let (model, env, _) = await make(connection: .connecting)
        let view = SftpDualView(model: model)
        XCTAssertEqual(env.sources[1]?.homeCalls, 0)
        XCTAssertEqual(env.localSource?.homeCalls, 0)
        XCTAssertEqual(view.remotePanel.stateText, String(localized: "Connecting…"))
        XCTAssertEqual(view.localPanel.stateText, String(localized: "Connecting…"))
        var tab = model.tab
        tab.connection = .connected
        model.follow(tab)
        await settle(model)
        model.remote.open(directory: "/remote/home/docs")
        model.local.open(directory: "/local/home/docs")
        await settle(model)
        tab.connection = .closed(reason: "Authentication failed")
        model.follow(tab)
        XCTAssertEqual(view.remotePanel.stateText, "Authentication failed")
        XCTAssertEqual(view.localPanel.stateText, "Authentication failed")
        XCTAssertFalse(model.remote.canWrite)
        XCTAssertFalse(model.local.canWrite)
        tab.connection = .closed(reason: nil)
        model.follow(tab)
        XCTAssertEqual(model.connectionText, String(localized: "Session ended"))
        tab.connection = .connecting
        model.follow(tab)
        tab.connection = .connected
        model.follow(tab)
        await settle(model)
        XCTAssertEqual(model.remote.directory, "/remote/home")
        XCTAssertEqual(model.local.directory, "/local/home")
        XCTAssertEqual(env.sources[1]?.homeCalls, 2)
        XCTAssertEqual(env.localSource?.homeCalls, 2)
        XCTAssertTrue(model.remote.selectedPaths.isEmpty)
        XCTAssertTrue(model.local.selectedPaths.isEmpty)
    }

    func testDropDirectionAndReceiverRefresh() async throws {
        let (model, env, _) = await make()
        let download = SftpDualDrag(session: 1, side: .remote, path: "/remote/home/notes.txt")
        let upload = SftpDualDrag(session: 1, side: .local, path: "/local/home/docs")
        let remoteReads = env.sources[1]?.lists.count
        XCTAssertTrue(model.performDrop([download], onto: .local))
        await model.remote.waitForBatch()
        await settle(model)
        XCTAssertEqual(env.engine.starts.last?.item.kind, .download)
        XCTAssertEqual(env.engine.starts.last?.item.source, download.path)
        XCTAssertEqual(env.engine.starts.last?.item.destination, "/local/home")
        XCTAssertEqual(env.sources[1]?.lists.count, remoteReads)
        XCTAssertEqual(env.localSource?.lists.count, 2)
        XCTAssertTrue(model.performDrop([upload], onto: .remote))
        await model.remote.waitForBatch()
        await settle(model)
        XCTAssertEqual(env.engine.starts.last?.item.kind, .upload)
        XCTAssertEqual(env.engine.starts.last?.item.source, upload.path)
        XCTAssertEqual(env.engine.starts.last?.item.destination, "/remote/home")
        XCTAssertEqual(env.engine.starts.last?.item.isDirectory, true)
        XCTAssertEqual(env.localSource?.lists.count, 2)
        XCTAssertEqual(model.dropVerdict([upload], onto: .local), .ignore)
        XCTAssertFalse(model.performDrop([download], onto: .remote))
        XCTAssertFalse(model.performDrop([.init(session: 2, side: .local, path: upload.path)], onto: .remote))
        XCTAssertFalse(model.performDrop([.init(session: 1, side: .local, path: "/missing")], onto: .remote))
    }

    func testTransferButtonsAndDropsShareD4ConflictRunner() async {
        let (model, env, prompter) = await make()
        env.engine.script["notes.txt"] = [.failed(message: "exists", alreadyExists: true), .completed(skipped: 0)]
        prompter.conflictAnswers = [.init(choice: .overwrite, applyToRest: true)]
        model.local.setSelection(paths: ["/local/home/notes.txt"])
        XCTAssertTrue(model.canTransfer(from: .local))
        XCTAssertTrue(model.transfer(from: .local))
        await model.remote.waitForBatch()
        await settle(model)
        XCTAssertEqual(env.engine.starts.map(\.policy), [.fail, .overwrite])
        XCTAssertEqual(prompter.conflictRequests.count, 1)
        XCTAssertEqual(prompter.conflictRequests.first?.kind, .upload)
        XCTAssertTrue(prompter.conflictRequests.first?.destinationDescription.contains("/remote/home") == true)
        XCTAssertNil(prompter.saveURL)
        let panel = FilerPanelView(model: model.remote)
        XCTAssertNil(panel.debugMenu(row: 0))
        XCTAssertEqual(panel.visibleRowCount, model.remote.visibleEntries.count)
    }

    func testDualRenameUsesSharedValidationAndPrefillsDialog() async throws {
        let (model, env, _) = await make()
        model.remote.setSelection(paths: ["/remote/home/notes.txt"])
        model.remote.beginRename()
        let dialog = FilerNameDialog(model: model.remote)
        XCTAssertEqual(dialog.field.stringValue, "notes.txt")
        let invalidClosed = await model.remote.commitEditing(name: "../escape", viaBlur: false)
        XCTAssertFalse(invalidClosed)
        XCTAssertTrue(env.sources[1]?.ops.isEmpty == true)
        let unchangedClosed = await model.remote.commitEditing(name: "notes.txt", viaBlur: false)
        XCTAssertTrue(unchangedClosed)
        XCTAssertTrue(env.sources[1]?.ops.isEmpty == true)
        model.remote.beginRename()
        _ = await model.remote.commitEditing(name: "renamed.txt", viaBlur: false)
        XCTAssertEqual(env.sources[1]?.ops, ["rename /remote/home/notes.txt -> /remote/home/renamed.txt"])
    }

    func testAuxiliaryPanelsLoadOnExpansionAndReuseTheirViews() throws {
        let window = try MainWindowController(useMockCore: true, defaults: SftpTestDefaults(),
                                              settings: SettingsStore.attached(to: MemorySettingsBackend()))
        defer { window.controller.shutdown(); window.window?.orderOut(nil) }
        window.filer.model.replaceEnvironment(FakeEnv())
        let root = try XCTUnwrap(window.window?.contentView)
        func panels(in view: NSView) -> [NSView] {
            let own = view is SenderPanel || view is FilerPanelView || view is GitPanelView ? [view] : []
            return own + view.subviews.flatMap { panels(in: $0) }
        }
        XCTAssertTrue(panels(in: root).isEmpty)
        window.setSenderVisible(true)
        let sender = try XCTUnwrap(panels(in: root).first as? SenderPanel)
        XCTAssertTrue(window.window?.firstResponder === sender.editor)
        window.setFilerCollapsed(false)
        window.setGitCollapsed(false)
        let loaded = Set(panels(in: root).map(ObjectIdentifier.init))
        XCTAssertEqual(loaded.count, 3)
        window.setSenderVisible(false)
        window.setFilerCollapsed(true)
        window.setGitCollapsed(true)
        window.setSenderVisible(true)
        window.setFilerCollapsed(false)
        window.setGitCollapsed(false)
        XCTAssertEqual(Set(panels(in: root).map(ObjectIdentifier.init)), loaded)
    }

    /// Git 面板每次展开都恢复到上次宽度（首次为默认宽度），不会被分栏压到最小宽度。
    func testGitPanelReopensAtRememberedWidth() throws {
        // 模拟已经用过的用户：旧实现只在这个标记为 false 时设一次宽度，之后交给分栏分配，面板被压到最小宽度。
        let defaults = SftpTestDefaults()
        defaults.set(true, forKey: "kanso.git.widthInitialized")
        let window = try MainWindowController(useMockCore: true, defaults: defaults,
                                              settings: SettingsStore.attached(to: MemorySettingsBackend()))
        defer { window.controller.shutdown(); window.window?.orderOut(nil) }
        window.window?.setContentSize(NSSize(width: 1600, height: 800))
        func gitWidth() throws -> CGFloat {
            let root = try XCTUnwrap(window.window?.contentView)
            func find(_ view: NSView) -> GitPanelView? {
                (view as? GitPanelView) ?? view.subviews.lazy.compactMap(find).first
            }
            return try XCTUnwrap(find(root)?.superview).frame.width
        }
        window.setGitCollapsed(false)
        XCTAssertEqual(try gitWidth(), MainWindowController.gitDefaultWidth, accuracy: 2)
        window.setGitCollapsed(true)
        window.setGitCollapsed(false)
        XCTAssertEqual(try gitWidth(), MainWindowController.gitDefaultWidth, accuracy: 2)
    }

    func testVisibleSenderIsRestoredDuringWindowCreation() throws {
        let defaults = SftpTestDefaults()
        defaults.set(true, forKey: "kanso.sender.visible")
        let window = try MainWindowController(useMockCore: true, defaults: defaults,
                                              settings: SettingsStore.attached(to: MemorySettingsBackend()))
        defer { window.controller.shutdown(); window.window?.orderOut(nil) }
        func containsSender(_ view: NSView) -> Bool {
            view is SenderPanel || view.subviews.contains(where: containsSender)
        }
        XCTAssertTrue(containsSender(try XCTUnwrap(window.window?.contentView)))
        XCTAssertTrue(window.senderModel.isVisible)
    }

    func testSftpTabHidesAuxiliaryAndRestoresStatusAndShortcut() async throws {
        let window = try MainWindowController(useMockCore: true, defaults: SftpTestDefaults(),
                                              settings: SettingsStore.attached(to: MemorySettingsBackend()))
        defer { window.controller.shutdown(); window.window?.orderOut(nil) }
        let controller = window.controller
        controller.workspace.addTab(TabItem(id: 101, kind: .ssh, name: "shell", connection: .connecting))
        controller.refresh()
        window.setFilerCollapsed(false)
        controller.workspace.addTab(TabItem(id: 102, kind: .sftp, name: "files", connection: .connecting))
        controller.refresh()
        XCTAssertTrue(window.isFilerCollapsed)
        XCTAssertEqual(window.filer.model.availability, .unavailableSftp)
        XCTAssertFalse(window.isFilerStatusEnabled)
        XCTAssertNotNil(window.filer.model.unavailableReason)
        let view = try XCTUnwrap(window.filer.dualWorkspace(for: 102))
        XCTAssertNotNil(view.superview)
        XCTAssertTrue(view === window.filer.dualWorkspace(for: 102))
        window.kansoToggleFiler(nil)
        window.clickFilerStatusItem()
        XCTAssertTrue(window.isFilerCollapsed)
        _ = controller.workspace.activate(101)
        controller.refresh()
        XCTAssertNil(view.superview)
        XCTAssertFalse(window.isFilerCollapsed)
        XCTAssertTrue(window.isFilerStatusEnabled)
        window.kansoToggleFiler(nil)
        XCTAssertTrue(window.isFilerCollapsed)
        _ = controller.workspace.activate(102)
        controller.refresh()
        _ = controller.workspace.activate(101)
        controller.refresh()
        XCTAssertTrue(window.isFilerCollapsed)
    }
}
