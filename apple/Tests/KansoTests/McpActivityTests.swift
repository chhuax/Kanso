import AppKit
import KansoBridge
import KansoUI
import PlatformMac
import XCTest
@testable import Kanso

/// MCP 活动记录窗口。core 的 FFI 没有写入审计的入口，所以记录由注入的 `readPage` 提供；
/// 窗口位置记忆名一律传 nil，不写真实的 `UserDefaults.standard`；协调器用 mock core 与内存钥匙串。
@MainActor
final class McpActivityTests: XCTestCase {
    private func makeCoordinator(clock: TestClock) -> McpCoordinator {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        return McpCoordinator(
            core: core,
            makeServer: { core, delegate in KansoMcpServer.newMock(core: core, delegate: delegate) },
            tokens: KeychainSecretStore(service: "app.kanso.mcp.activity.test", client: MemoryKeychain()),
            supported: true,
            bundleId: "app.kanso.mcp.activity.test",
            scheduleAfter: clock.schedule
        )
    }

    private func makeWindow(clock: TestClock = TestClock()) -> (McpActivityWindowController, McpCoordinator) {
        let coordinator = makeCoordinator(clock: clock)
        let controller = McpActivityWindowController(mcp: coordinator, autosaveName: nil)
        _ = controller.content.view
        addTeardownBlock { @MainActor in controller.window?.orderOut(nil) }
        return (controller, coordinator)
    }

    private func record(_ id: Int64, client: String = "Claude Code", profileId: String? = nil, error: String? = nil) -> FfiMcpAuditRecord {
        FfiMcpAuditRecord(
            id: id, atMs: 1_700_000_000_000 + id, client: client, mcpSession: "s", tool: "kanso_exec",
            profileId: profileId, sessionId: nil, args: "ls", errorCode: error, durationMs: 1, bytes: 0
        )
    }

    /// 倒序（id 大的在前）的记录，模拟库里共有 `total` 条，按 `beforeId` 与 `limit` 分页。
    private func pager(total: Int, reads: Reads) -> (FfiMcpAuditFilter) throws -> [FfiMcpAuditRecord] {
        { filter in
            reads.filters.append(filter)
            let all = stride(from: total, through: 1, by: -1).map { Int64($0) }
            let page = all.filter { id in filter.beforeId.map { id < $0 } ?? true }.prefix(Int(filter.limit))
            return page.map { self.record($0) }
        }
    }

    private final class Reads {
        var filters: [FfiMcpAuditFilter] = []
    }

    private func show(_ controller: McpActivityWindowController) {
        controller.present()
        XCTAssertEqual(controller.window?.isVisible, true)
    }

    // MARK: - 窗口外观

    func testWindowIsTitledResizableWithTheIntroLine() {
        let (controller, _) = makeWindow()
        XCTAssertEqual(controller.window?.title, String(localized: "MCP Activity"))
        XCTAssertTrue(controller.window?.styleMask.contains(.resizable) == true)
        XCTAssertEqual(
            controller.content.introLabel.stringValue,
            String(localized: "Every action an AI tool takes through MCP is recorded here. Records stay on this Mac for 30 days.")
        )
    }

    func testEmptyLibraryShowsNoRecordsAndHidesMore() {
        let (controller, _) = makeWindow()
        let content = controller.content
        XCTAssertNil(content.auditError)
        XCTAssertEqual(content.displayedRecordCount, 0)
        XCTAssertTrue(content.moreButton.isHidden)
        XCTAssertTrue(content.newActivityButton.isHidden)
    }

    func testRecordsAreRenderedWithTheSharedHeadPartsAndTheErrorCode() {
        let (controller, _) = makeWindow()
        let content = controller.content
        content.readPage = { _ in [self.record(2, error: "NOT_AUTHORIZED"), self.record(1)] }
        content.refreshFromStart()
        XCTAssertEqual(content.displayedRecordCount, 2)
        let texts = allLabels(in: content.view).map(\.stringValue)
        let head = McpText.auditHeadParts(record(2)).joined(separator: " · ") + " · NOT_AUTHORIZED"
        XCTAssertTrue(texts.contains(head), "\(texts)")
    }

    private func allLabels(in view: NSView) -> [NSTextField] {
        (view.subviews.compactMap { $0 as? NSTextField }) + view.subviews.flatMap { allLabels(in: $0) }
    }

    // MARK: - 读失败、筛选、翻页

    func testReadFailureShowsTheErrorInsteadOfAnEmptyList() {
        struct Boom: Error {}
        let (controller, _) = makeWindow()
        let content = controller.content
        content.readPage = { _ in throw Boom() }
        content.refreshFromStart()
        XCTAssertNotNil(content.auditError)
        XCTAssertTrue(content.moreButton.isHidden)
    }

    func testMoreLoadsTheNextPageWithTheCommittedFilter() {
        let (controller, _) = makeWindow()
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 25, reads: reads)
        content.clientField.stringValue = "abc"
        content.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification))
        XCTAssertEqual(content.displayedRecordCount, 20)
        XCTAssertFalse(content.moreButton.isHidden)

        // 输入框里又改了字但没提交：翻页必须沿用已提交的条件。
        content.clientField.stringValue = "xyz"
        content.moreButton.performClick(nil)
        XCTAssertEqual(reads.filters.last?.client, "abc")
        XCTAssertEqual(reads.filters.last?.beforeId, 6)
        XCTAssertEqual(content.displayedRecordCount, 25)
        XCTAssertTrue(content.moreButton.isHidden)
        XCTAssertTrue(content.hasPagedBeyondFirstPage)
    }

    func testErrorsOnlyAndSessionFiltersResetToTheFirstPage() throws {
        let (controller, coordinator) = makeWindow()
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        content.errorsOnlyRow.toggle()
        XCTAssertEqual(reads.filters.last?.errorsOnly, true)
        XCTAssertNil(reads.filters.last?.beforeId)

        let web = try XCTUnwrap(coordinator.core.profiles().first { $0.name == "web-01" })
        let index = try XCTUnwrap(content.sessionPopup.titles.firstIndex(of: web.name))
        content.sessionPopup.userSelect(at: index)
        XCTAssertEqual(reads.filters.last?.profileId, web.id)
    }

    func testSessionFilterFallsBackToAllSessionsAfterTheSessionIsDeleted() async throws {
        let (controller, coordinator) = makeWindow()
        let content = controller.content
        let web = try XCTUnwrap(coordinator.core.profiles().first { $0.name == "web-01" })
        let index = try XCTUnwrap(content.sessionPopup.titles.firstIndex(of: web.name))
        content.sessionPopup.userSelect(at: index)
        XCTAssertEqual(content.sessionPopup.indexOfSelectedItem, index)

        try await coordinator.core.deleteProfile(id: web.id)
        content.refreshFromStart()
        XCTAssertEqual(content.sessionPopup.indexOfSelectedItem, 0)
        XCTAssertEqual(content.sessionPopup.selectedTitle, String(localized: "All sessions"))
    }

    // MARK: - 清空

    func testClearNeedsConfirmation() async {
        let (controller, _) = makeWindow()
        let content = controller.content
        var clears = 0
        content.clearRecords = { clears += 1 }
        var asked = 0
        content.confirmClear = { asked += 1; return false }
        await content.clear()
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(clears, 0, "取消确认不能清空")

        content.confirmClear = { asked += 1; return true }
        await content.clear()
        XCTAssertEqual(clears, 1)
        XCTAssertNil(content.auditError)
    }

    func testClearFailureIsShownAndKeepsTheWindowUsable() async {
        struct Boom: Error {}
        let (controller, _) = makeWindow()
        let content = controller.content
        content.clearRecords = { throw Boom() }
        content.confirmClear = { true }
        await content.clear()
        XCTAssertNotNil(content.auditError)
    }

    func testClearButtonGoesThroughTheConfirmation() async {
        let (controller, _) = makeWindow()
        let content = controller.content
        let asked = expectation(description: "confirm")
        content.confirmClear = { asked.fulfill(); return false }
        content.clearButton.performClick(nil)
        await fulfillment(of: [asked], timeout: 2)
    }

    // MARK: - Esc

    func testEscInTheClientFilterClosesTheWindow() {
        let (controller, _) = makeWindow()
        let content = controller.content
        var closed = 0
        content.onClose = { closed += 1 }
        let editor = NSTextView()
        XCTAssertFalse(content.control(content.clientField.textField, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        XCTAssertEqual(closed, 0)
        XCTAssertTrue(content.control(content.clientField.textField, textView: editor, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertEqual(closed, 1)
        content.cancelOperation(nil)
        XCTAssertEqual(closed, 2)
    }

    // MARK: - 自动刷新与节流

    private func emitAudit(_ coordinator: McpCoordinator) {
        var batch = McpDelegateBridge.Batch()
        batch.audit = true
        coordinator.apply(batch)
    }

    func testAuditChangeReloadsTheFirstPageWhenNotPaged() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        content.refreshFromStart()
        show(controller)
        let base = reads.filters.count

        emitAudit(coordinator)
        XCTAssertEqual(reads.filters.count, base, "合并窗口内先不读库")
        clock.fire()
        XCTAssertEqual(reads.filters.count, base + 1)
        XCTAssertNil(reads.filters.last?.beforeId, "未翻页时重载第一页")
        XCTAssertTrue(content.newActivityButton.isHidden)
    }

    func testAuditChangeKeepsTheCurrentFilterWhenReloading() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        content.errorsOnlyRow.toggle()
        show(controller)
        emitAudit(coordinator)
        clock.fire()
        XCTAssertEqual(reads.filters.last?.errorsOnly, true)
    }

    func testAuditChangeAfterPagingOnlyOffersShowLatest() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 25, reads: reads)
        // 先显示再翻页：显示窗口本身会从第一页重载。
        show(controller)
        content.moreButton.performClick(nil)
        XCTAssertEqual(content.displayedRecordCount, 25)
        let base = reads.filters.count

        emitAudit(coordinator)
        clock.fire()
        XCTAssertEqual(reads.filters.count, base, "翻过页后不重载，已加载的页不能被丢掉")
        XCTAssertEqual(content.displayedRecordCount, 25)
        XCTAssertFalse(content.newActivityButton.isHidden)

        content.newActivityButton.performClick(nil)
        XCTAssertEqual(content.displayedRecordCount, 20, "点击后回到第一页")
        XCTAssertTrue(content.newActivityButton.isHidden)
        XCTAssertFalse(content.hasPagedBeyondFirstPage)
    }

    func testBurstOfAuditChangesIsCoalescedIntoOneQuery() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        show(controller)
        let base = reads.filters.count

        for _ in 0..<5 { emitAudit(coordinator) }
        XCTAssertEqual(clock.delays, [McpActivityWindowController.auditCoalesceDelay], "同一时间窗口只排一次")
        clock.fire()
        XCTAssertEqual(reads.filters.count, base + 1)

        // 处理完之后新的变化可以再排一次。
        emitAudit(coordinator)
        XCTAssertEqual(clock.delays.count, 1)
        clock.fire()
        XCTAssertEqual(reads.filters.count, base + 2)
    }

    func testAuditChangesAreIgnoredWhileTheWindowIsNotVisible() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let reads = Reads()
        controller.content.readPage = pager(total: 3, reads: reads)
        XCTAssertNotEqual(controller.window?.isVisible, true)
        emitAudit(coordinator)
        XCTAssertTrue(clock.delays.isEmpty)
        XCTAssertTrue(reads.filters.isEmpty)
    }

    func testPresentReloadsFromTheFirstPageAndIsIdempotentWhenVisible() {
        let (controller, _) = makeWindow()
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 25, reads: reads)
        controller.present()
        XCTAssertEqual(content.displayedRecordCount, 20, "窗口关闭期间的事件没处理过，打开时整体重载")
        content.moreButton.performClick(nil)
        let before = reads.filters.count
        controller.present()
        XCTAssertEqual(reads.filters.count, before, "窗口已经可见时再次打开只前置，不丢掉翻页进度")
        XCTAssertEqual(content.displayedRecordCount, 25)
    }

    func testPresentKeepsAQueuedAuditRefreshWhenTheWindowIsAlreadyVisible() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        show(controller)
        emitAudit(coordinator)
        XCTAssertEqual(clock.delays.count, 1)

        content.readPage = pager(total: 5, reads: reads)
        controller.present()
        XCTAssertEqual(clock.delays.count, 1, "窗口已可见时前置不能取消排队的刷新")
        clock.fire()
        XCTAssertEqual(content.displayedRecordCount, 5, "触发计时后新记录被加载")
    }

    // MARK: - 筛选与会话数据

    func testAuditRefreshUsesTheCommittedFilterNotTheUncommittedText() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        content.clientField.stringValue = "abc"
        content.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification))
        show(controller)
        // present() 的整体重载会提交控件里的当前条件，先让已提交条件稳定为 abc。
        XCTAssertEqual(reads.filters.last?.client, "abc")

        content.clientField.stringValue = "xyz"
        emitAudit(coordinator)
        clock.fire()
        XCTAssertEqual(reads.filters.last?.client, "abc", "未提交的文字不能借自动刷新生效")

        content.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification))
        XCTAssertEqual(reads.filters.last?.client, "xyz", "提交之后才生效")
    }

    func testAuditRefreshDoesNotReadTheSessionList() {
        let clock = TestClock()
        let (controller, coordinator) = makeWindow(clock: clock)
        let content = controller.content
        var profileReads = 0, accessReads = 0
        let core = coordinator.core
        content.readProfiles = { profileReads += 1; return core.profiles() }
        content.readAccesses = { accessReads += 1; return core.mcpAccesses() }
        content.readPage = pager(total: 3, reads: Reads())
        show(controller)
        let profileBase = profileReads, accessBase = accessReads
        XCTAssertGreaterThan(profileBase, 0, "整体重载会读会话列表")

        emitAudit(coordinator)
        clock.fire()
        XCTAssertEqual(profileReads, profileBase, ".audit 刷新不碰会话下拉框")
        XCTAssertEqual(accessReads, accessBase)

        NotificationCenter.default.post(name: .sessionLibraryDidChange, object: core)
        XCTAssertEqual(profileReads, profileBase + 1, "会话库变化时刷新")
        XCTAssertEqual(accessReads, accessBase + 1)
    }

    func testSessionLibraryChangeFallsBackToAllSessionsWhenTheSelectedOneIsGone() throws {
        let (controller, coordinator) = makeWindow()
        let content = controller.content
        let reads = Reads()
        content.readPage = pager(total: 3, reads: reads)
        let web = try XCTUnwrap(coordinator.core.profiles().first { $0.name == "web-01" })
        let index = try XCTUnwrap(content.sessionPopup.titles.firstIndex(of: web.name))
        content.sessionPopup.userSelect(at: index)
        XCTAssertEqual(reads.filters.last?.profileId, web.id)

        content.readProfiles = { coordinator.core.profiles().filter { $0.id != web.id } }
        NotificationCenter.default.post(name: .sessionLibraryDidChange, object: coordinator.core)
        XCTAssertEqual(content.sessionPopup.indexOfSelectedItem, 0)
        XCTAssertNil(reads.filters.last?.profileId, "筛选随之回到全部会话并重查")
    }

    func testFailedRecordOfAnUnauthorizedSessionIsLabelled() throws {
        let (controller, coordinator) = makeWindow()
        let content = controller.content
        let core = coordinator.core
        let web = try XCTUnwrap(core.profiles().first { $0.name == "web-01" })
        try core.setMcpAccess(profileId: web.id, level: .off)
        content.readPage = { _ in
            [self.record(2, profileId: web.id, error: "NOT_AUTHORIZED"), self.record(1, profileId: web.id)]
        }
        content.refreshFromStart()
        let texts = allLabels(in: content.view).map(\.stringValue)
        XCTAssertTrue(texts.contains { $0.contains(String(localized: "Unauthorized session")) && $0.contains("NOT_AUTHORIZED") }, "\(texts)")
        XCTAssertTrue(texts.contains { $0.hasSuffix("· web-01") }, "成功的记录照常显示会话名：\(texts)")
    }
}
