import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

/// 测试用确认者：记录请求，按预设答案返回，不弹任何模态框。
@MainActor
final class StubCloseConfirmer: CloseConfirmer {
    var answer: Bool
    private(set) var requests: [CloseConfirmationRequest] = []

    init(answer: Bool) { self.answer = answer }

    func confirm(_ request: CloseConfirmationRequest) async -> Bool {
        requests.append(request)
        return answer
    }
}

/// `WorkspaceController` + mock core（不启动真实 shell）。
@MainActor
final class WorkspaceControllerTests: XCTestCase {
    private var controllers: [WorkspaceController] = []

    private func makeController(answer: Bool = true) -> (WorkspaceController, StubCloseConfirmer, CoreEventDispatcher) {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let confirmer = StubCloseConfirmer(answer: answer)
        let controller = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: confirmer)
        controllers.append(controller)
        return (controller, confirmer, dispatcher)
    }

    override func tearDown() async throws {
        for c in controllers { c.shutdown() }
        controllers = []
    }

    func testEachSessionHasItsOwnFindBarInsideItsTerminalView() throws {
        let (c, _, _) = makeController()
        let a = try c.openLocalShell()
        let b = try c.split(axis: .vertical)
        let hostA = try XCTUnwrap(c.host(for: a)), hostB = try XCTUnwrap(c.host(for: b))
        XCTAssertFalse(hostA.findBar === hostB.findBar)
        // 装在终端视图里：跨分屏移动时随视图一起走，不需要单独搬。
        XCTAssertTrue(hostA.findBar.view.superview === hostA.view)
        XCTAssertFalse(hostA.findBar.isVisible)
    }

    func testOpenLocalShellAddsTabAndHost() throws {
        let (c, _, _) = makeController()
        let id = try c.openLocalShell()
        XCTAssertEqual(c.workspace.activeSession, id)
        XCTAssertEqual(c.workspace.item(id)?.kind, .local)
        XCTAssertNotNil(c.host(for: id))
    }

    func testSplitCreatesSecondPaneAndActivatesIt() throws {
        let (c, _, _) = makeController()
        let first = try c.openLocalShell()
        let second = try c.split(axis: .horizontal)
        XCTAssertNotEqual(first, second)
        XCTAssertEqual(c.workspace.allPanes.count, 2)
        XCTAssertEqual(c.workspace.activeSession, second)
        XCTAssertEqual(c.workspace.pane(of: second), c.workspace.activePane)
    }

    func testSplitOnEmptyWorkspaceDoesNotLeaveEmptyPane() throws {
        let (c, _, _) = makeController()
        let id = try c.split(axis: .horizontal)
        XCTAssertEqual(c.workspace.allPanes.count, 1)
        XCTAssertEqual(c.workspace.activeSession, id)
    }

    func testDuplicateAddsTabAfterActive() throws {
        let (c, _, _) = makeController()
        let first = try c.openLocalShell()
        let copy = try c.duplicateActiveTab()
        XCTAssertNotEqual(first, copy)
        XCTAssertEqual(c.workspace.tabs(in: c.workspace.activePane).map(\.id), [first, copy])
    }

    func testDuplicateWithoutActiveTabOpensLocalShell() throws {
        let (c, _, _) = makeController()
        let id = try c.duplicateActiveTab()
        XCTAssertEqual(c.workspace.activeSession, id)
    }

    func testRejectedConfirmationClosesNothing() async throws {
        let (c, confirmer, _) = makeController(answer: false)
        let a = try c.openLocalShell()
        let b = try c.openLocalShell()
        c.workspace.applyActivity(a, .commandRunning)
        c.workspace.applyActivity(b, .commandRunning)
        let closed = await c.requestClose([a, b])
        XCTAssertFalse(closed)
        XCTAssertEqual(confirmer.requests.count, 1)
        XCTAssertNotNil(c.workspace.item(a))
        XCTAssertNotNil(c.workspace.item(b))
        XCTAssertNotNil(c.host(for: a))
    }

    func testApprovedBatchCloseAsksOnlyOnce() async throws {
        let (c, confirmer, _) = makeController(answer: true)
        let a = try c.openLocalShell()
        let b = try c.openLocalShell()
        c.workspace.applyActivity(a, .commandRunning)
        c.workspace.applyActivity(b, .commandRunning)
        let closed = await c.requestClose([a, b])
        XCTAssertTrue(closed)
        XCTAssertEqual(confirmer.requests.count, 1)
        XCTAssertEqual(confirmer.requests.first?.titles.count, 2)
        XCTAssertNil(c.workspace.item(a))
        XCTAssertNil(c.workspace.item(b))
        XCTAssertNil(c.host(for: a))
    }

    func testIdleLocalSessionClosesWithoutAsking() async throws {
        let (c, confirmer, _) = makeController()
        let a = try c.openLocalShell()
        _ = await c.requestClose([a])
        XCTAssertEqual(confirmer.requests.count, 0)
        XCTAssertNil(c.workspace.item(a))
    }

    func testClosingLastTabLeavesEmptyPane() async throws {
        let (c, _, _) = makeController()
        let a = try c.openLocalShell()
        _ = await c.requestClose([a])
        XCTAssertNil(c.workspace.activeSession)
        XCTAssertEqual(c.workspace.allPanes.count, 1)
        XCTAssertTrue(c.workspace.tabs(in: c.workspace.activePane).isEmpty)
        // 空状态页：分屏区里没有任何终端视图，空状态页可见。
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 600, height: 400)
        container.reload()
        XCTAssertTrue(container.subviewsContainingEmptyStateVisible)
    }

    func testTerminalViewSurvivesMovingBetweenPanes() throws {
        let (c, _, _) = makeController()
        let first = try c.openLocalShell()
        let second = try c.split(axis: .horizontal)
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        container.reload()
        let view = try XCTUnwrap(c.host(for: first)?.view)
        let before = ObjectIdentifier(view)
        let targetPane = try XCTUnwrap(c.workspace.pane(of: second))
        XCTAssertTrue(c.workspace.moveTab(first, toPane: targetPane))
        container.reload()
        XCTAssertEqual(ObjectIdentifier(try XCTUnwrap(c.host(for: first)?.view)), before)
        XCTAssertTrue(c.host(for: first)?.view === view)
        XCTAssertNotNil(view.superview, "移动后活动标签的视图仍在分屏区里")
    }

    func testOnlyActiveTabViewsAreInHierarchy() throws {
        let (c, _, _) = makeController()
        let a = try c.openLocalShell()
        let b = try c.openLocalShell()
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        container.reload()
        XCTAssertNotNil(c.host(for: b)?.view.superview)
        XCTAssertNil(c.host(for: a)?.view.superview)
        c.workspace.activate(a)
        container.reload()
        XCTAssertNotNil(c.host(for: a)?.view.superview)
        XCTAssertNil(c.host(for: b)?.view.superview)
    }

    func testExitedSessionKeepsTabAsClosed() async throws {
        let (c, _, dispatcher) = makeController()
        let id = try c.openLocalShell()
        let done = expectation(description: "标签变为 closed")
        done.assertForOverFulfill = false
        c.addObserver {
            if case .closed = c.workspace.item(id)?.connection { done.fulfill() }
        }
        dispatcher.onEvent(event: .sessionStateChanged(session: id, state: .closed(reason: .exited(code: 2))))
        await fulfillment(of: [done], timeout: 5)
        guard case .closed(let reason) = c.workspace.item(id)?.connection else { return XCTFail("应为 closed") }
        XCTAssertTrue(reason?.contains("2") == true)
        XCTAssertNotNil(c.host(for: id), "已结束的会话保留终端视图（最后画面）")
        XCTAssertEqual(c.workspace.activeSession, id)
    }

    func testRenameSessionSyncsWorkspaceAndCore() throws {
        let (c, _, _) = makeController()
        let id = try c.openLocalShell()
        c.renameSession(id, to: "  构建  ")
        XCTAssertEqual(c.workspace.item(id)?.customTitle, "构建")
        XCTAssertEqual(c.core.sessionInfo(sessionId: id)?.customTitle, "构建")

        c.renameSession(id, to: "   ")
        XCTAssertNil(c.workspace.item(id)?.customTitle)
        XCTAssertNil(c.core.sessionInfo(sessionId: id)?.customTitle)
    }
}

extension PaneContainerView {
    /// 测试辅助：空状态页是否可见。
    var subviewsContainingEmptyStateVisible: Bool {
        subviews.contains { $0 is EmptyStateView && !$0.isHidden }
    }
}
