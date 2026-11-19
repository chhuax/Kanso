import AppKit
import KansoBridge
import KansoUI
import XCTest
@testable import Kanso

/// 会话启动器里的 MCP 访问：每行与分组「…」菜单、从「关闭」调高前的确认、行尾标记。
/// 全部用内存会话库与替身（`AccessStub`、`StubDialogs`），不碰 core、钥匙串和真实的对话框。
@MainActor
final class SessionLauncherMcpTests: XCTestCase {
    /// 记录确认与错误提示，确认结果由测试决定。
    @MainActor
    private final class StubDialogs: SessionDialogs {
        var confirmResult = true
        private(set) var confirms: [(title: String, message: String)] = []
        private(set) var errors: [(title: String, message: String)] = []

        func confirm(title: String, message: String, confirmTitle: String, destructive: Bool) async -> Bool {
            confirms.append((title, message))
            return confirmResult
        }

        func promptText(title: String, message: String?, initial: String, confirmTitle: String) async -> String? { nil }

        func showError(title: String, message: String) { errors.append((title, message)) }
    }

    private struct Fixture {
        let store: MemoryStore
        let access: AccessStub
        let dialogs: StubDialogs
        let model: SessionLauncherModel
        let launcher: SessionLauncherViewController
        let tasks: Tasks
    }

    /// 收集 `withDialog` 起的任务，测试可以等它们做完。
    @MainActor
    private final class Tasks {
        var pending: [Task<Void, Never>] = []
        var withDialogCalls = 0
        func drain() async {
            for task in pending { await task.value }
            pending = []
        }
    }

    private func makeFixture(supportsMcp: Bool = true, levels: [String: FfiMcpAccess] = [:]) -> Fixture {
        let store = MemoryStore(
            profiles: [
                SessionFixtures.ssh("1", "alpha", group: "g"),
                SessionFixtures.ssh("2", "beta", group: "g"),
                SessionFixtures.local("3", "loose"),
            ],
            groups: [FfiGroup(id: "g", name: "Prod")]
        )
        let access = AccessStub()
        access.levels = levels
        let dialogs = StubDialogs()
        let tasks = Tasks()
        let model = SessionLauncherModel(store: store, mcpAccess: supportsMcp ? access : nil)
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { _ in }, edit: { _ in }, dismiss: {}, openSettings: {},
            withDialog: { work in
                tasks.withDialogCalls += 1
                tasks.pending.append(Task { @MainActor in await work() })
            }
        )
        let launcher = SessionLauncherViewController(model: model, dialogs: dialogs, actions: actions)
        _ = launcher.view
        return Fixture(store: store, access: access, dialogs: dialogs, model: model, launcher: launcher, tasks: tasks)
    }

    private func profile(_ f: Fixture, _ id: String) throws -> FfiProfile {
        try XCTUnwrap(f.model.profile(id: id))
    }

    private func submenu(_ menu: NSMenu, titled title: String) -> NSMenu? {
        menu.items.first { $0.title == title }?.submenu
    }

    private func pick(_ menu: NSMenu, _ title: String) throws {
        let index = try XCTUnwrap(menu.items.firstIndex { $0.title == title }, "没有菜单项 \(title)")
        menu.performActionForItem(at: index)
    }

    private var accessMenuTitle: String { String(localized: "MCP Access") }

    // MARK: - 菜单

    func testProfileMenuHasMcpAccessSubmenuWithTheCurrentLevelChecked() throws {
        let f = makeFixture(levels: ["1": .readOnly])
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        let sub = try XCTUnwrap(submenu(menu, titled: accessMenuTitle))
        XCTAssertEqual(sub.items.map(\.title), McpText.accessLevels.map(McpText.accessTitle))
        XCTAssertEqual(sub.items.map { $0.state == .on }, [false, true, false])

        let off = f.launcher.makeProfileMenu(try profile(f, "2"))
        XCTAssertEqual(submenu(off, titled: accessMenuTitle)?.items.map { $0.state == .on }, [true, false, false])
    }

    func testGroupMenuHasWholeGroupItems() throws {
        let f = makeFixture()
        let menu = f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod"))
        let sub = try XCTUnwrap(submenu(menu, titled: accessMenuTitle))
        XCTAssertEqual(
            sub.items.map(\.title),
            McpText.accessLevels.map { String(localized: "Set Whole Group to \(McpText.accessTitle($0))") }
        )
    }

    func testMenusHaveNoMcpEntriesWhenMcpIsNotSupported() throws {
        let f = makeFixture(supportsMcp: false, levels: ["1": .full])
        XCTAssertNil(submenu(f.launcher.makeProfileMenu(try profile(f, "1")), titled: accessMenuTitle))
        XCTAssertNil(submenu(f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod")), titled: accessMenuTitle))
        XCTAssertFalse(f.model.supportsMcpAccess)
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .off)
        XCTAssertEqual(f.access.batchReads, 0, "不支持 MCP 时不读授权")
    }

    // MARK: - 从「关闭」调高要确认

    func testRaisingFromOffAsksFirstAndWritesOnConfirm() async throws {
        let f = makeFixture()
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(menu, titled: accessMenuTitle)), McpText.accessTitle(.readOnly))
        XCTAssertEqual(f.tasks.withDialogCalls, 1, "确认框是 sheet，要走 withDialog")
        XCTAssertTrue(f.access.writes.isEmpty, "确认之前不能写")
        await f.tasks.drain()

        XCTAssertEqual(f.dialogs.confirms.count, 1)
        XCTAssertEqual(f.dialogs.confirms.first?.title, McpText.raiseConfirmTitle(plural: false))
        XCTAssertEqual(f.dialogs.confirms.first?.message, McpText.sensitiveNotice)
        XCTAssertEqual(f.access.writes.map(\.0), ["1"])
        XCTAssertEqual(f.access.writes.first?.1, .readOnly)
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .readOnly, "写完后启动器的快照已刷新")
    }

    func testCancellingTheConfirmationWritesNothing() async throws {
        let f = makeFixture()
        f.dialogs.confirmResult = false
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(menu, titled: accessMenuTitle)), McpText.accessTitle(.full))
        await f.tasks.drain()
        XCTAssertEqual(f.dialogs.confirms.count, 1)
        XCTAssertTrue(f.access.writes.isEmpty)
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .off)
    }

    func testChangingBetweenNonOffLevelsOrLoweringDoesNotAsk() async throws {
        let f = makeFixture(levels: ["1": .readOnly])
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        let sub = try XCTUnwrap(submenu(menu, titled: accessMenuTitle))
        try pick(sub, McpText.accessTitle(.full))
        XCTAssertEqual(f.tasks.withDialogCalls, 0)
        XCTAssertEqual(f.access.writes.first?.1, .full)

        let again = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(again, titled: accessMenuTitle)), McpText.accessTitle(.off))
        XCTAssertEqual(f.tasks.withDialogCalls, 0)
        XCTAssertEqual(f.access.writes.last?.1, .off)
        XCTAssertTrue(f.dialogs.confirms.isEmpty)
    }

    func testSettingTheSameOffLevelDoesNotAsk() throws {
        let f = makeFixture()
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(menu, titled: accessMenuTitle)), McpText.accessTitle(.off))
        XCTAssertEqual(f.tasks.withDialogCalls, 0)
    }

    // MARK: - 整组

    func testWholeGroupWritesEveryMemberAfterOneConfirmation() async throws {
        let f = makeFixture()
        let menu = f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod"))
        try pick(
            try XCTUnwrap(submenu(menu, titled: accessMenuTitle)),
            String(localized: "Set Whole Group to \(McpText.accessTitle(.full))")
        )
        await f.tasks.drain()
        XCTAssertEqual(f.dialogs.confirms.count, 1)
        XCTAssertEqual(f.dialogs.confirms.first?.title, McpText.raiseConfirmTitle(plural: true))
        XCTAssertEqual(Set(f.access.writes.map(\.0)), ["1", "2"], "只写组内会话，不碰未分组的")
        XCTAssertTrue(f.access.writes.allSatisfy { $0.1 == .full })
    }

    func testWholeGroupAsksWhenOnlySomeMembersAreOff() async throws {
        let f = makeFixture(levels: ["1": .readOnly])
        let menu = f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod"))
        try pick(
            try XCTUnwrap(submenu(menu, titled: accessMenuTitle)),
            String(localized: "Set Whole Group to \(McpText.accessTitle(.full))")
        )
        await f.tasks.drain()
        XCTAssertEqual(f.dialogs.confirms.count, 1)
        XCTAssertEqual(Set(f.access.writes.map(\.0)), ["1", "2"])
    }

    func testWholeGroupToOffNeedsNoConfirmationAndIgnoresTheSearchFilter() async throws {
        let f = makeFixture(levels: ["1": .full, "2": .readOnly])
        // 搜索只命中 alpha：整组仍要作用于整个分组。
        f.model.query = "alpha"
        XCTAssertEqual(f.model.visibleProfiles.map(\.id), ["1"])
        let menu = f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod"))
        try pick(
            try XCTUnwrap(submenu(menu, titled: accessMenuTitle)),
            String(localized: "Set Whole Group to \(McpText.accessTitle(.off))")
        )
        XCTAssertEqual(f.tasks.withDialogCalls, 0)
        XCTAssertEqual(Set(f.access.writes.map(\.0)), ["1", "2"])
    }

    func testPartialFailureKeepsSuccessesAndReportsOnce() async throws {
        let f = makeFixture()
        f.access.failWrites = true
        let result = f.model.setMcpAccess(["1", "2"], to: .readOnly)
        XCTAssertEqual(result.total, 2)
        XCTAssertEqual(result.failed, 2)
        XCTAssertNotNil(result.firstError)

        // 经界面入口：全部失败时只弹一次汇总错误。
        let menu = f.launcher.makeGroupMenu(FfiGroup(id: "g", name: "Prod"))
        try pick(
            try XCTUnwrap(submenu(menu, titled: accessMenuTitle)),
            String(localized: "Set Whole Group to \(McpText.accessTitle(.readOnly))")
        )
        await f.tasks.drain()
        XCTAssertEqual(f.dialogs.errors.count, 1)
        XCTAssertEqual(f.dialogs.errors.first?.title, String(localized: "Could not change MCP access"))
    }

    func testFailedSingleWriteShowsTheReason() async throws {
        let f = makeFixture(levels: ["1": .readOnly])
        f.access.failWrites = true
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(menu, titled: accessMenuTitle)), McpText.accessTitle(.full))
        XCTAssertEqual(f.tasks.withDialogCalls, 1, "不需要确认的路径写入失败，错误框也要走 withDialog")
        await f.tasks.drain()
        XCTAssertEqual(f.dialogs.errors.count, 1)
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .readOnly, "写失败后级别保持原样")
    }

    func testSuccessfulWriteWithoutConfirmationDoesNotTouchDialogs() async throws {
        let f = makeFixture(levels: ["1": .readOnly])
        let menu = f.launcher.makeProfileMenu(try profile(f, "1"))
        try pick(try XCTUnwrap(submenu(menu, titled: accessMenuTitle)), McpText.accessTitle(.full))
        XCTAssertEqual(f.tasks.withDialogCalls, 0, "成功时直接写入，不关浮层")
        XCTAssertTrue(f.dialogs.errors.isEmpty)
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .full)
    }

    // MARK: - 标记

    func testBadgeAppearsOnlyOnSessionsThatAreNotOff() throws {
        let f = makeFixture(levels: ["1": .readOnly, "2": .full])
        let off = LauncherProfileRowView(profile: try profile(f, "3"))
        off.mcpLevel = f.model.mcpLevel(of: "3")
        XCTAssertFalse(off.isMcpBadgeVisible)
        XCTAssertNil(off.mcpBadgeToolTip)

        let read = LauncherProfileRowView(profile: try profile(f, "1"))
        read.mcpLevel = f.model.mcpLevel(of: "1")
        XCTAssertTrue(read.isMcpBadgeVisible)
        XCTAssertEqual(read.mcpBadgeToolTip, McpText.badgeTooltip(.readOnly))

        let full = LauncherProfileRowView(profile: try profile(f, "2"))
        full.mcpLevel = .full
        XCTAssertEqual(full.mcpBadgeToolTip, String(localized: "MCP: \(McpText.accessTitle(.full))"))

        full.mcpLevel = .off
        XCTAssertFalse(full.isMcpBadgeVisible)
    }

    func testClickingTheBadgeHitsTheRowNotTheBadge() throws {
        let f = makeFixture(levels: ["1": .readOnly])
        let row = LauncherProfileRowView(profile: try profile(f, "1"))
        row.mcpLevel = .readOnly
        row.frame = NSRect(x: 0, y: 0, width: 360, height: 44)
        row.layoutSubtreeIfNeeded()
        let badge = row.mcpBadgeViewForTesting
        XCTAssertFalse(badge.isHidden)
        // hitTest 的点在父视图坐标系；行放进容器里，容器坐标即行坐标。
        let container = NSView(frame: row.frame)
        container.addSubview(row)
        let center = NSPoint(x: badge.frame.midX, y: badge.frame.midY)
        let hit = container.hitTest(center)
        // 落点可以是行本身或行里的高亮底板（它不处理 mouseDown，事件沿响应链交给行）；不能是标记。
        XCTAssertTrue(hit === row || hit?.isDescendant(of: row) == true, "hit = \(String(describing: hit))")
        XCTAssertFalse(hit === badge)
        XCTAssertNil(badge.hitTest(center))
    }

    func testModelReadsAllLevelsInOneBatchAndRefreshesOnLibraryChange() throws {
        let f = makeFixture(levels: ["1": .readOnly])
        XCTAssertEqual(f.access.batchReads, 1)
        XCTAssertEqual(f.access.singleReads, 0, "不能逐行查库")
        XCTAssertEqual(f.model.mcpLevel(of: "1"), .readOnly)

        // 编辑对话框等别处改了级别并发出库变更通知：启动器快照随之刷新。
        f.access.levels["3"] = .full
        f.store.notifyLibraryChanged()
        XCTAssertEqual(f.model.mcpLevel(of: "3"), .full)
        XCTAssertEqual(f.access.singleReads, 0)
    }

    func testRaiseDetection() {
        let f = makeFixture(levels: ["1": .full])
        XCTAssertFalse(f.model.raisesMcpAccessFromOff(["1"], to: .readOnly))
        XCTAssertTrue(f.model.raisesMcpAccessFromOff(["1", "2"], to: .readOnly))
        XCTAssertFalse(f.model.raisesMcpAccessFromOff(["1", "2"], to: .off))
        XCTAssertEqual(f.model.profileIDs(inGroup: "g"), ["1", "2"])
    }

    func testSharedWordingIsUsedByTheEditorHint() {
        XCTAssertFalse(McpText.sensitiveNotice.isEmpty)
        XCTAssertNotEqual(McpText.raiseConfirmTitle(plural: true), McpText.raiseConfirmTitle(plural: false))
    }
}
