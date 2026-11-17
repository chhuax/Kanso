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

    func copyMatching(_ query: [String: Any]) -> (OSStatus, Data?) {
        guard let account = query[kSecAttrAccount as String] as? String, let data = items[account] else {
            return (errSecItemNotFound, nil)
        }
        return (errSecSuccess, data)
    }

    func update(_ query: [String: Any], _ attributes: [String: Any]) -> OSStatus {
        guard let account = query[kSecAttrAccount as String] as? String, items[account] != nil else {
            return errSecItemNotFound
        }
        items[account] = attributes[kSecValueData as String] as? Data
        return errSecSuccess
    }

    func add(_ attributes: [String: Any]) -> OSStatus {
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

    private func makeSettings(mcp: McpCoordinator, supported: Bool) -> SettingsWindowController {
        let settings = SettingsStore()
        let appearance = AppearanceController(settings: settings, apply: { _ in })
        let keybindings = KeybindingController(settings: settings, menuProvider: { nil })
        return SettingsWindowController(
            settings: settings, appearance: appearance, keybindings: keybindings,
            openURL: { _ in }, conflictPrompter: McpConflictStub(), autosaveName: nil,
            mcp: mcp, mcpSupported: supported
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
        let shown = makeSettings(mcp: mock, supported: true)
        XCTAssertEqual(shown.tabsInWindow, [.general, .display, .keyboard, .mcp])
        XCTAssertNotNil(shown.mcpViewController)

        let hidden = makeSettings(mcp: mock, supported: false)
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
        let page = makeSettings(mcp: real, supported: true)
        XCTAssertTrue(page.tabsInWindow.contains(.mcp))
        _ = page.mcpViewController?.view
        XCTAssertTrue(page.mcpViewController?.statusLabel.stringValue.contains(
            String(localized: "MCP is not available in this build.")
        ) == true)
        XCTAssertEqual(page.mcpViewController?.enabledRow.isChecked, true)
    }

    func testSwitchStartsAndStopsTheServer() async throws {
        let (coordinator, _, _) = makeCoordinator()
        let page = try XCTUnwrap(makeSettings(mcp: coordinator, supported: true).mcpViewController)
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
        XCTAssertNil(try coordinator.currentToken())
        XCTAssertTrue(keychain.items.isEmpty)

        try coordinator.setEnabled(true)
        await flush()
        let token = try XCTUnwrap(try coordinator.currentToken())
        XCTAssertEqual(token.count, McpClientToken.length)
        XCTAssertTrue(token.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) })
        XCTAssertEqual(Array(keychain.items.keys), [McpClientToken.account])
        XCTAssertEqual(McpClientToken.account, "mcp/client-token")
        XCTAssertFalse(McpClientToken.account.hasPrefix("profile/"))

        try coordinator.setEnabled(false)
        try coordinator.setEnabled(true)
        await flush()
        XCTAssertEqual(try coordinator.currentToken(), token)
        let idBefore = try XCTUnwrap(coordinator.clients.first?.id)

        try coordinator.regenerateToken()
        await flush()
        let regenerated = try XCTUnwrap(try coordinator.currentToken())
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

        let page = try XCTUnwrap(makeSettings(mcp: coordinator, supported: true).mcpViewController)
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

        try coordinator.server.debugActivity(sessionId: 7, tool: "kanso_terminal_read")
        await flush()
        XCTAssertEqual(coordinator.mark(for: 7)?.activityTool, "kanso_terminal_read")
        XCTAssertEqual(coordinator.mark(for: 7)?.activityClient, "Claude Code (mock)")

        try coordinator.server.debugActivity(sessionId: 7, tool: nil)
        await flush()
        XCTAssertEqual(clock.delays, [2])
        XCTAssertEqual(coordinator.mark(for: 7)?.activityTool, "kanso_terminal_read")

        try coordinator.server.debugActivity(sessionId: 7, tool: "kanso_terminal_write")
        await flush()
        XCTAssertTrue(clock.delays.isEmpty)
        XCTAssertEqual(coordinator.mark(for: 7)?.activityTool, "kanso_terminal_write")
        clock.fire()
        XCTAssertEqual(coordinator.mark(for: 7)?.activityTool, "kanso_terminal_write")

        try coordinator.server.debugActivity(sessionId: 7, tool: nil)
        await flush()
        XCTAssertEqual(clock.delays, [2])
        clock.fire()
        XCTAssertNil(coordinator.mark(for: 7)?.activityTool)
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
        let token = try XCTUnwrap(try shown.mcp.currentToken())
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
        let page = try XCTUnwrap(makeSettings(mcp: coordinator, supported: true).mcpViewController)
        var copied: [String] = []
        page.copyText = { copied.append($0) }
        _ = page.view

        page.copyClaudeButton.performClick(nil)
        let token = try XCTUnwrap(try coordinator.currentToken())
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

    private func tabModel(session: SessionID, mark: McpTabMark?) -> TabRowView.Model {
        TabRowView.Model(
            item: TabItem(id: session, kind: .local, name: "local", connection: .connected),
            sidebarIndex: 0,
            selection: .none,
            mcp: mark
        )
    }
}
