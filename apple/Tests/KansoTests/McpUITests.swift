import AppKit
import KansoBridge
import KansoUI
import PlatformMac
import Security
import XCTest
@testable import Kanso

/// 内存钥匙串。查询字典的键与 `KeychainSecretStore.baseQuery` 一致。
private final class MemoryKeychain: KeychainClient, @unchecked Sendable {
    var items: [String: Data] = [:]
    /// 非 nil 时读取一律返回这个错误状态，模拟钥匙串授权被拒。
    var readFailure: OSStatus?
    /// 非 nil 时写入（add / update）一律返回这个错误状态。
    var writeFailure: OSStatus?

    func copyMatching(_ query: [String: Any]) -> (OSStatus, Data?) {
        if let readFailure { return (readFailure, nil) }
        guard let account = query[kSecAttrAccount as String] as? String, let data = items[account] else {
            return (errSecItemNotFound, nil)
        }
        return (errSecSuccess, data)
    }

    func update(_ query: [String: Any], _ attributes: [String: Any]) -> OSStatus {
        if let writeFailure { return writeFailure }
        guard let account = query[kSecAttrAccount as String] as? String, items[account] != nil else {
            return errSecItemNotFound
        }
        items[account] = attributes[kSecValueData as String] as? Data
        return errSecSuccess
    }

    func add(_ attributes: [String: Any]) -> OSStatus {
        if let writeFailure { return writeFailure }
        guard let account = attributes[kSecAttrAccount as String] as? String else { return errSecParam }
        items[account] = attributes[kSecValueData as String] as? Data
        return errSecSuccess
    }

    func delete(_ query: [String: Any]) -> OSStatus {
        if let account = query[kSecAttrAccount as String] as? String { items[account] = nil }
        return errSecSuccess
    }
}

/// 注入的计时器。记录尚未取消的延迟；`fire` 只执行没被取消的工作。
private final class TestClock {
    private struct Item {
        var delay: TimeInterval
        var work: @MainActor () -> Void
        var cancelled = false
    }

    private var items: [Item] = []
    var delays: [TimeInterval] { items.filter { !$0.cancelled }.map(\.delay) }

    func schedule(_ delay: TimeInterval, _ work: @escaping @MainActor () -> Void) -> () -> Void {
        items.append(Item(delay: delay, work: work))
        let index = items.count - 1
        return { [weak self] in self?.items[index].cancelled = true }
    }

    @MainActor
    func fire() {
        let due = items
        items.removeAll()
        for item in due where !item.cancelled { item.work() }
    }
}

/// 可控的访问级别存储：记录写入次数，可让读或写失败。
private final class AccessStub: McpAccessStoring, @unchecked Sendable {
    struct Failure: Error {}
    var levels: [String: FfiMcpAccess] = [:]
    var failReads = false
    var failWrites = false
    private(set) var writes: [(String, FfiMcpAccess)] = []

    func mcpAccess(profileId: String) throws -> FfiMcpAccess {
        if failReads { throw Failure() }
        return levels[profileId] ?? .off
    }

    func setMcpAccess(profileId: String, level: FfiMcpAccess) throws {
        if failWrites { throw Failure() }
        writes.append((profileId, level))
        levels[profileId] = level
    }
}

/// 转发给 core，但 `setCredential` 一律失败。
private final class FailingCredentialStore: SessionStore, @unchecked Sendable {
    struct Failure: Error {}
    let core: KansoCore
    init(core: KansoCore) { self.core = core }
    func profiles() -> [FfiProfile] { core.profiles() }
    func groups() -> [FfiGroup] { core.groups() }
    func saveProfile(profile: FfiProfile) throws { try core.saveProfile(profile: profile) }
    func deleteProfile(id: String) async throws { try await core.deleteProfile(id: id) }
    func saveGroup(group: FfiGroup) throws { try core.saveGroup(group: group) }
    func groupDeleteImpact(id: String) throws -> FfiDeleteImpact { try core.groupDeleteImpact(id: id) }
    func deleteGroup(id: String) async throws { try await core.deleteGroup(id: id) }
    func setCredential(profile: String, kind: FfiSecretKind, secret: String) async throws { throw Failure() }
}

private final class McpConflictStub: KeybindingConflictPrompter {
    func confirmReplace(chord: KeyChord, existing: KeyCommand, new: KeyCommand, window: NSWindow?) async -> Bool {
        false
    }
}

@MainActor
final class McpUITests: XCTestCase {
    private func flush() async {
        let done = expectation(description: "main")
        DispatchQueue.main.async { done.fulfill() }
        await fulfillment(of: [done], timeout: 2)
    }

    private func makeCoordinator(
        useMockServer: Bool = true,
        supported: Bool = true,
        keychain: MemoryKeychain = MemoryKeychain(),
        schedule: TestClock = TestClock()
    ) -> (McpCoordinator, MemoryKeychain, TestClock) {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        let coordinator = McpCoordinator(
            core: core,
            useMockServer: useMockServer,
            tokens: KeychainSecretStore(service: "app.kanso.mcp.test", client: keychain),
            supported: supported,
            bundleId: "app.kanso.mcp.test",
            scheduleAfter: schedule.schedule
        )
        return (coordinator, keychain, schedule)
    }

    private func makeSettings(mcp: McpCoordinator?) -> SettingsWindowController {
        let settings = SettingsStore()
        let appearance = AppearanceController(settings: settings, apply: { _ in })
        let keybindings = KeybindingController(settings: settings, menuProvider: { nil })
        return SettingsWindowController(
            settings: settings, appearance: appearance, keybindings: keybindings,
            openURL: { _ in }, conflictPrompter: McpConflictStub(), autosaveName: nil,
            mcp: mcp
        )
    }

    private func makeWindow(
        supported: Bool, keychain: MemoryKeychain = MemoryKeychain()
    ) throws -> (MainWindowController, UserDefaults) {
        let name = "kanso.mcp.ui.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        let window = try MainWindowController(
            useMockCore: true, defaults: defaults, keychain: keychain, mcpSupported: supported
        )
        return (window, defaults)
    }

    private func profile(_ core: KansoCore, named name: String) throws -> FfiProfile {
        try XCTUnwrap(core.profiles().first { $0.name == name })
    }

    func testPageAppearsOnlyWhenMcpIsSupported() async throws {
        let (mock, _, _) = makeCoordinator()
        let shown = makeSettings(mcp: mock)
        XCTAssertEqual(shown.tabsInWindow, [.general, .display, .keyboard, .mcp])
        XCTAssertNotNil(shown.mcpViewController)

        let (unsupported, _, _) = makeCoordinator(supported: false)
        let hidden = makeSettings(mcp: unsupported)
        XCTAssertEqual(hidden.tabsInWindow, [.general, .display, .keyboard])
        XCTAssertNil(hidden.mcpViewController)
        hidden.select(.mcp)
        XCTAssertNotEqual(hidden.selectedTab, .mcp)

        let (real, _, _) = makeCoordinator(useMockServer: false)
        try real.setEnabled(true)
        await flush()
        guard case .unavailable(.notSupported) = real.status else {
            return XCTFail("真实服务应报本构建不支持，实际 \(real.status)")
        }
        let page = makeSettings(mcp: real)
        XCTAssertTrue(page.tabsInWindow.contains(.mcp))
        _ = page.mcpViewController?.view
        XCTAssertTrue(page.mcpViewController?.statusLabel.stringValue.contains(
            String(localized: "MCP is not available in this build.")
        ) == true)
        XCTAssertEqual(page.mcpViewController?.enabledRow.isChecked, true)
    }

    func testSwitchStartsAndStopsTheServer() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        XCTAssertFalse(page.enabledRow.isChecked)
        XCTAssertFalse(coordinator.core.mcpEnabled())

        page.enabledRow.toggle()
        await flush()
        XCTAssertTrue(coordinator.core.mcpEnabled())
        XCTAssertTrue(page.enabledRow.isChecked)
        guard case .running = coordinator.status else {
            return XCTFail("开启后应在运行，实际 \(coordinator.status)")
        }
        XCTAssertFalse(coordinator.clients.isEmpty)

        page.enabledRow.toggle()
        await flush()
        XCTAssertFalse(coordinator.core.mcpEnabled())
        XCTAssertFalse(page.enabledRow.isChecked)
        guard case .stopped = coordinator.status else {
            return XCTFail("关闭后应停止，实际 \(coordinator.status)")
        }
        XCTAssertTrue(coordinator.clients.isEmpty)
    }

    func testTokenIsGeneratedOnceAndRegeneratedThroughTheInjectedKeychain() async throws {
        let keychain = MemoryKeychain()
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        XCTAssertNil(coordinator.cachedToken)
        XCTAssertTrue(keychain.items.isEmpty)

        try coordinator.setEnabled(true)
        await flush()
        let token = try XCTUnwrap(coordinator.cachedToken)
        XCTAssertEqual(token.count, McpClientToken.length)
        XCTAssertTrue(token.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) })
        XCTAssertEqual(Array(keychain.items.keys), [McpClientToken.account])
        XCTAssertEqual(McpClientToken.account, "mcp/client-token")
        XCTAssertFalse(McpClientToken.account.hasPrefix("profile/"))

        try coordinator.setEnabled(false)
        try coordinator.setEnabled(true)
        await flush()
        XCTAssertEqual(coordinator.cachedToken, token)
        let idBefore = try XCTUnwrap(coordinator.clients.first?.id)

        try coordinator.regenerateToken()
        await flush()
        let regenerated = try XCTUnwrap(coordinator.cachedToken)
        XCTAssertNotEqual(regenerated, token)
        XCTAssertTrue(regenerated.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) })
        XCTAssertNotEqual(coordinator.clients.first?.id, idBefore)
    }

    func testSessionAccessIsReadAndWrittenPerProfileAndPerGroup() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let core = coordinator.core
        let web = try profile(core, named: "web-01")
        let outsider = try profile(core, named: "legacy-switch")
        XCTAssertEqual(try core.mcpAccess(profileId: web.id), .off)
        XCTAssertEqual(try core.mcpAccess(profileId: outsider.id), .off)

        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        XCTAssertTrue(page.sensitiveNotice.isHidden)

        page.levelPopup(for: web.id)?.userSelect(at: McpText.accessIndex(.readOnly))
        XCTAssertEqual(try core.mcpAccess(profileId: web.id), .readOnly)
        XCTAssertEqual(try core.mcpAccess(profileId: outsider.id), .off)
        XCTAssertFalse(page.sensitiveNotice.isHidden)

        let group = try XCTUnwrap(core.groups().first { $0.name == "生产" })
        page.groupPopup(for: group.id)?.userSelect(at: McpText.accessIndex(.full) + 1)
        XCTAssertEqual(try core.mcpAccess(profileId: web.id), .full)
        XCTAssertEqual(try core.mcpAccess(profileId: outsider.id), .off)
        XCTAssertEqual(page.groupPopup(for: group.id)?.indexOfSelectedItem, 0)

        let editing = SessionEditorModel(store: core, editing: web, showsMcpAccess: true, mcpAccessStore: core)
        XCTAssertEqual(editing.mcpAccess, .full)
        editing.mcpAccess = .readOnly
        let saved = await editing.save()
        XCTAssertEqual(saved, .saved)
        await flush()
        XCTAssertEqual(try core.mcpAccess(profileId: web.id), .readOnly)
        XCTAssertEqual(page.levelPopup(for: web.id)?.indexOfSelectedItem, McpText.accessIndex(.readOnly))

        let hidden = SessionEditorModel(store: core, editing: web, showsMcpAccess: false)
        XCTAssertFalse(hidden.isVisible(.mcpAccess))
        hidden.mcpAccess = .full
        let hiddenSave = await hidden.save()
        XCTAssertEqual(hiddenSave, .saved)
        XCTAssertEqual(try core.mcpAccess(profileId: web.id), .readOnly)
        let hiddenEditor = SessionEditorViewController(model: hidden)
        _ = hiddenEditor.view
        XCTAssertTrue(hiddenEditor.mcpPopup.isHiddenOrHasHiddenAncestor)

        let created = SessionEditorModel(store: core, showsMcpAccess: true, mcpAccessStore: core)
        XCTAssertEqual(created.mcpAccess, .off)
        created.name = "mcp-new"
        let createdSave = await created.save()
        XCTAssertEqual(createdSave, .saved)
        XCTAssertEqual(try core.mcpAccess(profileId: created.profileID), .off)
        let shownEditor = SessionEditorViewController(model: created)
        _ = shownEditor.view
        XCTAssertFalse(shownEditor.mcpPopup.isHiddenOrHasHiddenAncestor)
        XCTAssertEqual(shownEditor.mcpPopup.indexOfSelectedItem, McpText.accessIndex(.off))
    }

    func testActivityBadgeStaysUntilTheScheduledTwoSeconds() async throws {
        let clock = TestClock()
        let (coordinator, _, _) = makeCoordinator(schedule: clock)
        try coordinator.setEnabled(true)
        await flush()
        let local = try profile(coordinator.core, named: "Local")
        let session = try await coordinator.server.debugOpenTab(profileId: local.id)
        await flush()

        try coordinator.server.debugActivity(sessionId: session, tool: "kanso_terminal_read")
        await flush()
        XCTAssertEqual(coordinator.mark(for: session)?.activityTool, "kanso_terminal_read")
        XCTAssertEqual(coordinator.mark(for: session)?.activityClient, "Claude Code (mock)")

        try coordinator.server.debugActivity(sessionId: session, tool: nil)
        await flush()
        XCTAssertEqual(clock.delays, [2])
        XCTAssertEqual(coordinator.mark(for: session)?.activityTool, "kanso_terminal_read")

        try coordinator.server.debugActivity(sessionId: session, tool: "kanso_terminal_write")
        await flush()
        XCTAssertTrue(clock.delays.isEmpty)
        XCTAssertEqual(coordinator.mark(for: session)?.activityTool, "kanso_terminal_write")
        clock.fire()
        XCTAssertEqual(coordinator.mark(for: session)?.activityTool, "kanso_terminal_write")

        try coordinator.server.debugActivity(sessionId: session, tool: nil)
        await flush()
        XCTAssertEqual(clock.delays, [2])
        clock.fire()
        XCTAssertNil(coordinator.mark(for: session)?.activityTool)
    }

    func testReleasingASessionRemovesOnlyTheOpenedMark() async throws {
        let (coordinator, _, _) = makeCoordinator()
        try coordinator.setEnabled(true)
        await flush()
        let local = try profile(coordinator.core, named: "Local")
        let session = try await coordinator.server.debugOpenTab(profileId: local.id)
        await flush()
        XCTAssertEqual(coordinator.mark(for: session)?.openedByClient, "Claude Code (mock)")

        try coordinator.server.debugActivity(sessionId: session, tool: "kanso_terminal_read")
        await flush()
        let client = try XCTUnwrap(coordinator.clients.first)
        coordinator.server.disconnect(clientId: client.id)
        await flush()

        XCTAssertNil(coordinator.mark(for: session)?.openedByClient)
        XCTAssertEqual(coordinator.mark(for: session)?.activityTool, "kanso_terminal_read")

        let row = TabRowView(model: tabModel(session: session, mark: coordinator.mark(for: session)))
        XCTAssertFalse(row.showsMcpOpenedMark)
        row.model.mcp = McpTabMark(openedByClient: "", activityClient: nil, activityTool: nil)
        XCTAssertTrue(row.showsMcpOpenedMark)
        let unknown = String(localized: "Opened by MCP · \(String(localized: "Unknown client"))")
        XCTAssertTrue(row.toolTip?.contains(unknown) == true)
    }

    func testStatusBarIndicatorFollowsTheClientCountOnlyWhenSupported() async throws {
        let bar = StatusBarView()
        XCTAssertFalse(bar.isMcpIndicatorVisible)
        bar.setMcpClientCount(2)
        XCTAssertTrue(bar.isMcpIndicatorVisible)
        var clicked = false
        bar.onOpenMcp = { clicked = true }
        bar.clickMcpIndicator()
        XCTAssertTrue(clicked)
        bar.setMcpClientCount(0)
        XCTAssertFalse(bar.isMcpIndicatorVisible)

        let (shown, defaults) = try makeWindow(supported: true)
        XCTAssertFalse(shown.isMcpIndicatorVisible)
        try shown.mcp.setEnabled(true)
        await flush()
        XCTAssertFalse(shown.mcp.clients.isEmpty)
        XCTAssertTrue(shown.isMcpIndicatorVisible)
        let token = try XCTUnwrap(shown.mcp.cachedToken)
        let stored = defaults.dictionaryRepresentation().values.map { "\($0)" }.joined(separator: "\n")
        XCTAssertFalse(stored.contains(token))
        try shown.mcp.setEnabled(false)
        await flush()
        XCTAssertFalse(shown.isMcpIndicatorVisible)

        let (hidden, _) = try makeWindow(supported: false)
        try hidden.mcp.setEnabled(true)
        await flush()
        XCTAssertFalse(hidden.mcp.clients.isEmpty)
        XCTAssertFalse(hidden.isMcpIndicatorVisible)
    }

    func testOpeningATabDoesNotStealFocus() async throws {
        let (window, _) = try makeWindow(supported: true)
        window.showWindow(nil)
        await flush()
        let before = window.controller.workspace.activeSession
        let responder = window.window?.firstResponder
        XCTAssertNotNil(before)

        try window.mcp.setEnabled(true)
        await flush()
        let local = try profile(window.mcp.core, named: "Local")
        let opened = try await window.mcp.server.debugOpenTab(profileId: local.id)
        await flush()

        let workspace = window.controller.workspace
        XCTAssertEqual(workspace.activeSession, before)
        XCTAssertNotEqual(opened, before)
        XCTAssertEqual(workspace.pane(of: opened), workspace.activePane)
        XCTAssertNotEqual(workspace.activeTab(in: workspace.activePane), opened)
        XCTAssertNotNil(workspace.item(opened))
        XCTAssertEqual(window.window?.firstResponder, responder)
        XCTAssertEqual(window.mcp.mark(for: opened)?.openedByClient, "Claude Code (mock)")
    }

    func testCopiedConfigurationKeepsTheLeadingSpace() throws {
        let (coordinator, _, _) = makeCoordinator()
        try coordinator.setEnabled(true)
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        var copied: [String] = []
        page.copyText = { copied.append($0) }
        _ = page.view

        page.copyClaudeButton.performClick(nil)
        let token = try XCTUnwrap(coordinator.cachedToken)
        let claude = mcpClientConfig(kind: .claudeCode, helperPath: mcpHelperPath(), token: token)
        XCTAssertEqual(copied, [claude])
        XCTAssertTrue(claude.hasPrefix(" "))

        page.copyCodexButton.performClick(nil)
        let codex = mcpClientConfig(kind: .codex, helperPath: mcpHelperPath(), token: token)
        XCTAssertEqual(copied.last, codex)
        XCTAssertTrue(codex.hasPrefix(" "))
    }

    func testDeletedSessionUsesTheSpecifiedTitle() {
        XCTAssertEqual(
            McpText.auditSessionTitle(profileId: "missing", names: [:]),
            String(localized: "Deleted session")
        )
        XCTAssertNil(McpText.auditSessionTitle(profileId: nil, names: [:]))
        XCTAssertEqual(
            mcpHelperPath(),
            Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/kanso-mcp").path
        )
    }

    // MARK: - 审查修复

    func testStartupReadsTheKeychainInTheBackgroundThenStarts() async throws {
        let keychain = MemoryKeychain()
        keychain.items[McpClientToken.account] = Data("stored-token".utf8)
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        try coordinator.core.setMcpEnabled(enabled: true)

        coordinator.startIfEnabled()
        XCTAssertNil(coordinator.cachedToken, "init 返回时不应已同步读完钥匙串")
        await coordinator.startupTask?.value
        await flush()
        XCTAssertEqual(coordinator.cachedToken, "stored-token")
        XCTAssertNil(coordinator.startError)
        guard case .running = coordinator.status else {
            return XCTFail("应已启动，实际 \(coordinator.status)")
        }
    }

    func testStartupFailureIsRecordedAndShownOnThePage() async throws {
        let keychain = MemoryKeychain()
        keychain.readFailure = errSecAuthFailed
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        try coordinator.core.setMcpEnabled(enabled: true)
        coordinator.startIfEnabled()
        await coordinator.startupTask?.value
        await flush()

        XCTAssertNotNil(coordinator.startError)
        guard case .stopped = coordinator.status else {
            return XCTFail("没有启动，状态应为停止，实际 \(coordinator.status)")
        }
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        XCTAssertTrue(page.statusLabel.stringValue.contains(
            String(localized: "MCP is turned on, but the service did not start.")
        ))
        XCTAssertFalse(page.copyTokenButton.isEnabled)
    }

    func testPageBuildDoesNotTouchTheKeychainSynchronously() throws {
        let keychain = MemoryKeychain()
        keychain.items[McpClientToken.account] = Data("stored-token".utf8)
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        XCTAssertNil(coordinator.cachedToken)
        XCTAssertFalse(page.copyTokenButton.isEnabled)
    }

    func testEditorOnlyWritesAccessWhenTheUserChangedIt() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let core = coordinator.core
        let web = try profile(core, named: "web-01")
        let stub = AccessStub()
        stub.levels[web.id] = .readOnly

        let untouched = SessionEditorModel(store: core, editing: web, showsMcpAccess: true, mcpAccessStore: stub)
        XCTAssertEqual(untouched.mcpAccess, .readOnly)
        // 打开后别处把级别改成了 full；没动过这个字段的保存不能用旧值盖回去。
        stub.levels[web.id] = .full
        let saved = await untouched.save()
        XCTAssertEqual(saved, .saved)
        XCTAssertTrue(stub.writes.isEmpty)
        XCTAssertEqual(stub.levels[web.id], .full)

        let edited = SessionEditorModel(store: core, editing: web, showsMcpAccess: true, mcpAccessStore: stub)
        edited.mcpAccess = .off
        let editedSave = await edited.save()
        XCTAssertEqual(editedSave, .saved)
        XCTAssertEqual(stub.writes.count, 1)
        XCTAssertEqual(stub.levels[web.id], .off)
    }

    func testEditorDisablesTheFieldWhenReadingTheAccessFails() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let core = coordinator.core
        let web = try profile(core, named: "web-01")
        let stub = AccessStub()
        stub.failReads = true
        let model = SessionEditorModel(store: core, editing: web, showsMcpAccess: true, mcpAccessStore: stub)
        XCTAssertFalse(model.isMcpAccessEnabled)
        XCTAssertNotNil(model.mcpAccessLoadError)
        let outcome = await model.save()
        XCTAssertEqual(outcome, .saved)
        XCTAssertTrue(stub.writes.isEmpty)

        let editor = SessionEditorViewController(model: model)
        _ = editor.view
        XCTAssertFalse(editor.mcpPopup.isEnabled)
        XCTAssertFalse(editor.mcpHint.isHidden)
        XCTAssertTrue(editor.mcpHint.stringValue.contains(model.mcpAccessLoadError ?? "?"))
    }

    func testSavingStillNotifiesWhenOnlyTheAccessWriteFails() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let core = coordinator.core
        let web = try profile(core, named: "web-01")
        let stub = AccessStub()
        stub.failWrites = true
        let model = SessionEditorModel(store: core, editing: web, showsMcpAccess: true, mcpAccessStore: stub)
        model.mcpAccess = .full
        var notified = 0
        let token = NotificationCenter.default.addObserver(
            forName: .sessionLibraryDidChange, object: core, queue: nil
        ) { _ in notified += 1 }
        defer { NotificationCenter.default.removeObserver(token) }

        let outcome = await model.save()
        XCTAssertEqual(outcome, .savedButMcpAccessFailed)
        XCTAssertEqual(notified, 1)
        XCTAssertNotNil(model.errorText)
    }

    func testFailedAccessWriteRestoresThePopupAndShowsTheError() throws {
        let (coordinator, _, _) = makeCoordinator()
        let web = try profile(coordinator.core, named: "web-01")
        let stub = AccessStub()
        stub.levels[web.id] = .readOnly
        let page = McpSettingsViewController(mcp: coordinator, accessStore: stub)
        _ = page.view
        XCTAssertEqual(page.levelPopup(for: web.id)?.indexOfSelectedItem, McpText.accessIndex(.readOnly))
        XCTAssertTrue(page.errorLabel.isHidden)

        stub.failWrites = true
        page.levelPopup(for: web.id)?.userSelect(at: McpText.accessIndex(.full))
        XCTAssertEqual(page.levelPopup(for: web.id)?.indexOfSelectedItem, McpText.accessIndex(.readOnly))
        XCTAssertFalse(page.errorLabel.isHidden)
        XCTAssertFalse(page.errorLabel.stringValue.isEmpty)
    }

    func testFailedRegenerateShowsTheErrorAndKeepsTheOldToken() throws {
        let keychain = MemoryKeychain()
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        try coordinator.setEnabled(true)
        let old = try XCTUnwrap(coordinator.cachedToken)
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        keychain.writeFailure = errSecAuthFailed
        page.regenerateButton.performClick(nil)
        XCTAssertEqual(coordinator.cachedToken, old)
        XCTAssertFalse(page.errorLabel.isHidden)
        XCTAssertFalse(page.errorLabel.stringValue.isEmpty)
    }

    func testPageWithoutCoordinatorHasNoMcpTab() {
        let none = makeSettings(mcp: nil)
        XCTAssertEqual(none.tabsInWindow, [.general, .display, .keyboard])
        XCTAssertNil(none.mcpViewController)
        none.select(.mcp)
        XCTAssertNotEqual(none.selectedTab, .mcp)
    }

    func testPageScrollsAndStaysWithinTheMaximumHeight() throws {
        let (coordinator, _, _) = makeCoordinator()
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        let scroll = try XCTUnwrap(page.view.subviews.compactMap { $0 as? NSScrollView }.first)
        XCTAssertNotNil(scroll.documentView)
        page.view.layoutSubtreeIfNeeded()
        XCTAssertLessThanOrEqual(page.view.fittingSize.height, McpSettingsViewController.maxPageHeight)
        let documentHeight = scroll.documentView?.fittingSize.height ?? 0
        XCTAssertEqual(
            page.view.fittingSize.height, min(documentHeight, McpSettingsViewController.maxPageHeight), accuracy: 1
        )
    }

    func testAuditArgsSummaryIsTruncatedForDisplay() {
        XCTAssertEqual(McpText.argsSummary("ls -la"), "ls -la")
        let long = String(repeating: "a", count: 200)
        XCTAssertEqual(McpText.argsSummary(long).count, 81)
        XCTAssertTrue(McpText.argsSummary(long).hasSuffix("…"))
        XCTAssertEqual(McpText.argsSummary("a\nb"), "a b")
    }

    func testOpenedEventForAGoneSessionIsSkipped() async throws {
        let (coordinator, _, _) = makeCoordinator()
        try coordinator.setEnabled(true)
        await flush()
        let client = try XCTUnwrap(coordinator.clients.first)
        var placed: [UInt64] = []
        coordinator.onPlaceSession = { placed.append($0) }

        var batch = McpDelegateBridge.Batch()
        batch.opened = [(987_654, client)]
        coordinator.apply(batch)
        XCTAssertTrue(placed.isEmpty)
        XCTAssertNil(coordinator.mark(for: 987_654))
    }

    func testClosingATabClearsItsMcpState() async throws {
        let clock = TestClock()
        let (window, _) = try makeWindow(supported: true)
        window.mcp.scheduleAfter = clock.schedule
        window.showWindow(nil)
        try window.mcp.setEnabled(true)
        await flush()
        let local = try profile(window.mcp.core, named: "Local")
        let opened = try await window.mcp.server.debugOpenTab(profileId: local.id)
        await flush()
        try window.mcp.server.debugActivity(sessionId: opened, tool: "kanso_terminal_read")
        await flush()
        try window.mcp.server.debugActivity(sessionId: opened, tool: nil)
        await flush()
        XCTAssertNotNil(window.mcp.mark(for: opened)?.openedByClient)
        XCTAssertEqual(clock.delays, [2])

        await window.controller.requestClose([opened])
        XCTAssertNil(window.mcp.mark(for: opened))
        XCTAssertTrue(clock.delays.isEmpty, "待执行的清除计时应一并取消")
    }

    // MARK: - 第二轮审查修复

    func testToggleFailureShowsTheErrorAndSurvivesRefreshes() throws {
        let keychain = MemoryKeychain()
        keychain.readFailure = errSecAuthFailed
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        let page = try XCTUnwrap(makeSettings(mcp: coordinator).mcpViewController)
        _ = page.view
        page.enabledRow.toggle()
        XCTAssertFalse(page.errorLabel.isHidden)
        XCTAssertFalse(page.errorLabel.stringValue.isEmpty)
        XCTAssertFalse(page.enabledRow.isChecked)
        page.viewWillAppear()
        XCTAssertFalse(page.errorLabel.isHidden, "后续刷新不能清掉错误")
    }

    func testStartFailureOnEnableIsRecorded() async throws {
        let (real, _, _) = makeCoordinator(useMockServer: false)
        try real.setEnabled(true)
        await flush()
        XCTAssertNotNil(real.startError)
    }

    func testUserActionDuringStartupInvalidatesTheBackgroundResult() async throws {
        let keychain = MemoryKeychain()
        keychain.items[McpClientToken.account] = Data("old-token".utf8)
        let (coordinator, _, _) = makeCoordinator(keychain: keychain)
        try coordinator.core.setMcpEnabled(enabled: true)
        coordinator.startIfEnabled()
        // 后台读取还没回到主线程时，用户重新生成了令牌。
        try coordinator.regenerateToken()
        let regenerated = try XCTUnwrap(coordinator.cachedToken)
        await coordinator.startupTask?.value
        await flush()
        XCTAssertEqual(coordinator.cachedToken, regenerated)
        XCTAssertNotEqual(regenerated, "old-token")
        guard case .stopped = coordinator.status else {
            return XCTFail("作废的启动不应再 start，实际 \(coordinator.status)")
        }
    }

    func testGrantRowWithUnreadableLevelIsDisabledAndDoesNotRaiseTheNotice() throws {
        let (coordinator, _, _) = makeCoordinator()
        let web = try profile(coordinator.core, named: "web-01")
        let stub = AccessStub()
        stub.failReads = true
        let page = McpSettingsViewController(mcp: coordinator, accessStore: stub)
        _ = page.view
        XCTAssertEqual(page.levelPopup(for: web.id)?.isEnabled, false)
        XCTAssertNotNil(page.levelErrorLabels[web.id])
        // 读不到原级别时，批量设置不能把它当作「关闭」而弹敏感信息提示。
        let group = coordinator.core.groups().first { $0.name == "生产" }
        page.groupPopup(for: group?.id)?.userSelect(at: McpText.accessIndex(.full) + 1)
        XCTAssertTrue(page.sensitiveNotice.isHidden)
    }

    func testLateActivityForAClosedSessionIsIgnored() async throws {
        let (coordinator, _, _) = makeCoordinator()
        try coordinator.setEnabled(true)
        await flush()
        var batch = McpDelegateBridge.Batch()
        batch.activities = [(987_654, FfiMcpActivity(clientId: 1, clientName: "x", tool: "t"))]
        coordinator.apply(batch)
        XCTAssertNil(coordinator.mark(for: 987_654))
    }

    func testForgettingAnUnmarkedSessionDoesNotEmitChange() async throws {
        let (coordinator, _, _) = makeCoordinator()
        var changes = 0
        coordinator.addChangeHandler { changes += 1 }
        coordinator.forget(session: 42)
        XCTAssertEqual(changes, 0)
    }

    func testArgsSummaryFoldsAllLineBreaksAndTabs() {
        XCTAssertEqual(McpText.argsSummary("a\r\nb\rc\td\ne"), "a b c d e")
    }

    func testBothCredentialAndAccessFailuresAreReported() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let stub = AccessStub()
        stub.failWrites = true
        let model = SessionEditorModel(
            store: FailingCredentialStore(core: coordinator.core), showsMcpAccess: true, mcpAccessStore: stub
        )
        model.name = "both-fail"
        model.kind = .ssh
        model.host = "example.com"
        model.user = "root"
        model.auth = .password
        model.password = "secret"
        model.mcpAccess = .full
        let outcome = await model.save()
        XCTAssertEqual(outcome, .savedButCredentialFailed)
        let text = model.errorText ?? ""
        // 用带占位符的本地化键取前缀，才能在中文环境下也对得上。
        let credentialPrefix = String(localized: "The session was saved, but its credential could not be stored: \("@@")")
            .replacingOccurrences(of: "@@", with: "")
        let accessPrefix = String(localized: "The session was saved, but its MCP access level could not be saved: \("@@")")
            .replacingOccurrences(of: "@@", with: "")
        XCTAssertTrue(text.contains(credentialPrefix))
        XCTAssertTrue(text.contains(accessPrefix))
    }

    private func tabModel(session: SessionID, mark: McpTabMark?) -> TabRowView.Model {
        TabRowView.Model(
            item: TabItem(id: session, kind: .local, name: "local", connection: .connected),
            sidebarIndex: 0,
            selection: .none,
            mcp: mark
        )
    }
}
