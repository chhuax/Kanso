import KansoUI
import XCTest

@MainActor
final class WorkspaceTests: XCTestCase {
    private func tab(_ id: SessionID, kind: TabKind = .local, cwd: String? = nil) -> TabItem {
        TabItem(id: id, kind: kind, name: "t\(id)", cwd: cwd, connection: .connected)
    }

    private func ids(_ ws: Workspace, _ pane: PaneID) -> [SessionID] {
        ws.tabs(in: pane).map(\.id)
    }

    /// 三个左右分屏：p1=[1,2]，p2=[3,4,5]，p3=[6]，活动在 p3。
    private func makeThreePanes() -> (Workspace, [PaneID]) {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.addTab(tab(2))
        let p2 = ws.splitActivePane(axis: .horizontal, with: tab(3))
        ws.addTab(tab(4))
        ws.addTab(tab(5))
        let p3 = ws.splitActivePane(axis: .horizontal, with: tab(6))
        return (ws, [ws.allPanes[0], p2, p3])
    }

    func testInitialStateIsOneEmptyPane() {
        let ws = Workspace()
        XCTAssertEqual(ws.allPanes.count, 1)
        XCTAssertTrue(ws.tabs(in: ws.activePane).isEmpty)
        XCTAssertNil(ws.activeSession)
        XCTAssertFalse(ws.showsSectionHeaders)
    }

    func testAddTabInsertsAfterActive() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.addTab(tab(2))
        ws.addTab(tab(3))
        XCTAssertEqual(ids(ws, ws.activePane), [1, 2, 3])
        ws.activate(1)
        ws.addTab(tab(4))
        XCTAssertEqual(ids(ws, ws.activePane), [1, 4, 2, 3])
        XCTAssertEqual(ws.activeSession, 4)
        ws.addTab(tab(5), activate: false)
        XCTAssertEqual(ids(ws, ws.activePane), [1, 4, 5, 2, 3])
        XCTAssertEqual(ws.activeSession, 4)
    }

    func testAddDuplicateOrUnknownPaneIgnored() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.addTab(tab(1))
        ws.addTab(tab(2), in: PaneID(raw: 99))
        XCTAssertEqual(ids(ws, ws.activePane), [1])
        XCTAssertNil(ws.item(2))
    }

    func testRemoveTabPicksNextThenPrevious() {
        let ws = Workspace()
        for i in 1...3 { ws.addTab(tab(SessionID(i))) }
        ws.activate(2)
        ws.removeTab(2)
        XCTAssertEqual(ws.activeSession, 3)
        ws.removeTab(3)
        XCTAssertEqual(ws.activeSession, 1)
        // 删除非活动标签不改变活动标签。
        ws.addTab(tab(4))
        ws.activate(1)
        ws.removeTab(4)
        XCTAssertEqual(ws.activeSession, 1)
    }

    func testRemoveLastTabInOnlyPaneKeepsEmptyPane() {
        let ws = Workspace()
        ws.addTab(tab(1))
        XCTAssertTrue(ws.removeTab(1))
        XCTAssertEqual(ws.allPanes.count, 1)
        XCTAssertNil(ws.activeSession)
        XCTAssertNil(ws.item(1))
    }

    func testEmptyPaneCollapsesAndActivePaneMigrates() {
        let ws = Workspace()
        ws.addTab(tab(1))
        let p1 = ws.activePane
        let p2 = ws.splitActivePane(axis: .horizontal, with: tab(2))
        let p3 = ws.splitActivePane(axis: .horizontal, with: tab(3))
        XCTAssertEqual(ws.allPanes, [p1, p2, p3])
        ws.activatePane(p2)
        ws.removeTab(2)
        XCTAssertEqual(ws.allPanes, [p1, p3])
        XCTAssertEqual(ws.activePane, p1) // 阅读顺序中的前一个
        // 第一个 pane 被收起时取新的第一个。
        ws.removeTab(1)
        XCTAssertEqual(ws.allPanes, [p3])
        XCTAssertEqual(ws.activePane, p3)
        XCTAssertEqual(ws.activeSession, 3)
    }

    func testCollapseOfInactivePaneKeepsActivePane() {
        let (ws, panes) = makeThreePanes()
        ws.activatePane(panes[2])
        ws.removeTab(1)
        ws.removeTab(2)
        XCTAssertEqual(ws.allPanes, [panes[1], panes[2]])
        XCTAssertEqual(ws.activePane, panes[2])
    }

    func testSplitSidebarOrderAndSections() {
        let (ws, panes) = makeThreePanes()
        XCTAssertEqual(ws.sidebarOrder, [1, 2, 3, 4, 5, 6])
        XCTAssertTrue(ws.showsSectionHeaders)
        let sections = ws.sidebarSections
        XCTAssertEqual(sections.map(\.titleIndex), [1, 2, 3])
        XCTAssertEqual(sections.map(\.pane), panes)
        XCTAssertEqual(sections[1].tabs.map(\.id), [3, 4, 5])
        XCTAssertEqual(ws.activePane, panes[2])
        XCTAssertEqual(ws.activeSession, 6)
    }

    func testSelectTabAtSidebarIndex() {
        let (ws, panes) = makeThreePanes()
        XCTAssertTrue(ws.selectTab(atSidebarIndex: 3))
        XCTAssertEqual(ws.activeSession, 4)
        XCTAssertEqual(ws.activePane, panes[1])
        XCTAssertFalse(ws.selectTab(atSidebarIndex: 6))
        XCTAssertFalse(ws.selectTab(atSidebarIndex: -1))
        XCTAssertEqual(ws.activeSession, 4)
    }

    func testCycling() {
        let (ws, panes) = makeThreePanes()
        ws.activatePane(panes[1]) // 活动标签 5（最后加入的）
        ws.selectNextTabInPane()
        XCTAssertEqual(ws.activeSession, 3) // 循环到开头
        ws.selectPreviousTabInPane()
        XCTAssertEqual(ws.activeSession, 5) // 循环到末尾
        ws.selectPreviousTabInPane()
        XCTAssertEqual(ws.activeSession, 4)
        ws.selectNextPane()
        XCTAssertEqual(ws.activePane, panes[2])
        ws.selectNextPane()
        XCTAssertEqual(ws.activePane, panes[0])
        ws.selectPreviousPane()
        XCTAssertEqual(ws.activePane, panes[2])
    }

    func testCloseTargetsStayInGroup() {
        let (ws, _) = makeThreePanes()
        XCTAssertEqual(ws.closeTargets(.below, relativeTo: 3), [4, 5])
        XCTAssertEqual(ws.closeTargets(.below, relativeTo: 5), [])
        XCTAssertEqual(ws.closeTargets(.above, relativeTo: 5), [3, 4])
        XCTAssertEqual(ws.closeTargets(.others, relativeTo: 4), [3, 5])
        XCTAssertEqual(ws.closeTargets(.all, relativeTo: 4), [3, 4, 5])
        XCTAssertEqual(ws.closeTargets(.below, relativeTo: 1), [2])
        XCTAssertEqual(ws.closeTargets(.all, relativeTo: 999), [])
    }

    func testMoveTabWithinPane() {
        let ws = Workspace()
        for i in 1...4 { ws.addTab(tab(SessionID(i))) }
        XCTAssertTrue(ws.moveTab(1, toIndex: 2))
        XCTAssertEqual(ids(ws, ws.activePane), [2, 3, 1, 4])
        ws.moveTab(1, toIndex: 100)
        XCTAssertEqual(ids(ws, ws.activePane), [2, 3, 4, 1])
        ws.moveTab(1, toIndex: -5)
        XCTAssertEqual(ids(ws, ws.activePane), [1, 2, 3, 4])
        XCTAssertFalse(ws.moveTab(99, toIndex: 0))
    }

    func testMoveTabAcrossPanes() {
        let ws = Workspace()
        ws.addTab(tab(1))
        let p1 = ws.activePane
        let p2 = ws.splitActivePane(axis: .horizontal, with: tab(2))
        XCTAssertTrue(ws.moveTab(1, toPane: p2, at: 0))
        XCTAssertEqual(ids(ws, p2), [1, 2])
        XCTAssertEqual(ws.allPanes, [p2]) // 源组变空被收起
        XCTAssertEqual(ws.activePane, p2)
        XCTAssertEqual(ws.activeSession, 1)
        XCTAssertNil(ws.layout.panes.first { $0 == p1 })
        XCTAssertNotNil(ws.item(1)) // 会话没有被重建/丢弃
    }

    func testMoveTabAcrossPanesKeepsSourceWhenNotEmpty() {
        let (ws, panes) = makeThreePanes()
        XCTAssertTrue(ws.moveTab(4, toPane: panes[0]))
        XCTAssertEqual(ids(ws, panes[0]), [1, 2, 4])
        XCTAssertEqual(ids(ws, panes[1]), [3, 5])
        XCTAssertEqual(ws.activeTab(in: panes[1]), 5) // 被移走的 4 不是源组活动标签（5），源组活动标签不变
        XCTAssertEqual(ws.activeSession, 4)
        XCTAssertFalse(ws.moveTab(4, toPane: PaneID(raw: 99)))
    }

    func testUnreadDone() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.addTab(tab(2)) // 活动 2
        ws.applyActivity(1, .commandRunning)
        ws.applyActivity(1, .idle)
        XCTAssertEqual(ws.item(1)?.unreadDone, true)
        ws.applyActivity(1, .commandRunning)
        ws.applyActivity(1, .idle)
        XCTAssertEqual(ws.item(1)?.unreadDone, true) // 一直保留
        ws.activate(1)
        XCTAssertEqual(ws.item(1)?.unreadDone, false)

        // 活动标签自身不置位。
        ws.applyActivity(1, .agentWorking(name: "x"))
        ws.applyActivity(1, .agentIdle(name: "x"))
        XCTAssertEqual(ws.item(1)?.unreadDone, false)

        // 闲 -> 闲、忙 -> 忙不置位。
        ws.applyActivity(2, .idle)
        ws.applyActivity(2, .agentIdle(name: "x"))
        ws.applyActivity(2, .commandRunning)
        ws.applyActivity(2, .agentWorking(name: "x"))
        XCTAssertEqual(ws.item(2)?.unreadDone, false)
    }

    func testActivatePaneClearsUnread() {
        let (ws, panes) = makeThreePanes()
        // 5 是 panes[1] 的活动标签。
        ws.applyActivity(5, .commandRunning)
        ws.applyActivity(5, .idle)
        XCTAssertEqual(ws.item(5)?.unreadDone, true)
        ws.activatePane(panes[1])
        XCTAssertEqual(ws.item(5)?.unreadDone, false)
    }

    func testApplyTransferNeverNegative() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.applyTransfer(1, started: false)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 0)
        ws.applyTransfer(1, started: true)
        ws.applyTransfer(1, started: true)
        ws.applyTransfer(1, started: false)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 1)
    }

    func testApplyLocationAndConnection() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.applyLocation(1, cwd: "/x", cwdHost: "h", gitBranch: "main")
        XCTAssertEqual(ws.item(1)?.cwd, "/x")
        XCTAssertEqual(ws.item(1)?.cwdHost, "h")
        XCTAssertEqual(ws.item(1)?.gitBranch, "main")
        ws.applyConnection(1, .closed(reason: "boom"))
        XCTAssertEqual(ws.item(1)?.connection, .closed(reason: "boom"))
    }

    func testUnknownIdsAreNoOps() {
        let ws = Workspace()
        ws.addTab(tab(1))
        XCTAssertFalse(ws.activate(9))
        XCTAssertFalse(ws.activatePane(PaneID(raw: 99)))
        XCTAssertFalse(ws.removeTab(9))
        XCTAssertFalse(ws.moveTab(9, toIndex: 0))
        XCTAssertFalse(ws.moveTab(9, toPane: ws.activePane))
        XCTAssertFalse(ws.applyActivity(9, .idle))
        XCTAssertFalse(ws.applyConnection(9, .connected))
        XCTAssertFalse(ws.applyLocation(9, cwd: nil, cwdHost: nil, gitBranch: nil))
        XCTAssertFalse(ws.applyTransfer(9, started: true))
        XCTAssertNil(ws.item(9))
        XCTAssertNil(ws.activeTab(in: PaneID(raw: 99)))
        XCTAssertTrue(ws.tabs(in: PaneID(raw: 99)).isEmpty)
        XCTAssertNil(ws.closeConfirmation(for: [9]))
        XCTAssertEqual(ws.activeSession, 1)
    }

    func testSplitWithDuplicateIdDoesNothing() {
        let ws = Workspace()
        ws.addTab(tab(1))
        let p = ws.splitActivePane(axis: .vertical, with: tab(1))
        XCTAssertEqual(p, ws.activePane)
        XCTAssertEqual(ws.allPanes.count, 1)
    }

    func testSplitOfEmptyPaneFillsItInstead() {
        let ws = Workspace()
        let only = ws.activePane
        let p = ws.splitActivePane(axis: .horizontal, with: tab(1))
        XCTAssertEqual(p, only)
        XCTAssertEqual(ws.allPanes, [only])
        XCTAssertEqual(ws.activeSession, 1)
    }

    func testFilteredSidebarKeepsOrderAndState() {
        let ws = Workspace()
        ws.addTab(tab(1, cwd: "/a/proj"))
        let p2 = ws.splitActivePane(axis: .horizontal, with: tab(2, kind: .ssh, cwd: "/a/proj"))
        ws.addTab(tab(3, cwd: "/b"), in: p2)
        let f = ws.filteredSidebar("PROJ")
        XCTAssertEqual(f.map { $0.tabs.map(\.id) }, [[1], []])
        XCTAssertEqual(ws.filteredSidebar("").map { $0.tabs.map(\.id) }, [[1], [2, 3]])
        XCTAssertEqual(ws.sidebarOrder, [1, 2, 3])
        XCTAssertEqual(ws.activePane, p2)
    }

    // MARK: closeConfirmation

    private func confirmWS() -> Workspace {
        let ws = Workspace()
        ws.addTab(tab(1)) // 本地空闲
        ws.addTab(tab(2)) // 本地运行命令
        ws.applyActivity(2, .commandRunning)
        ws.addTab(tab(3)) // 本地 AI 运行
        ws.applyActivity(3, .agentWorking(name: "x"))
        ws.addTab(tab(4, kind: .ssh))
        ws.addTab(TabItem(id: 5, kind: .sftp, name: "s", connection: .connecting))
        ws.addTab(TabItem(id: 6, kind: .ssh, name: "dead", connection: .closed(reason: nil)))
        ws.addTab(tab(7)) // 本地空闲但有传输
        ws.applyTransfer(7, started: true)
        ws.addTab(TabItem(id: 8, kind: .sftp, name: "dead", connection: .closed(reason: "x"), activeTransfers: 2))
        return ws
    }

    func testCloseConfirmationBranches() {
        let ws = confirmWS()
        XCTAssertNil(ws.closeConfirmation(for: []))
        XCTAssertNil(ws.closeConfirmation(for: [1]))
        XCTAssertNil(ws.closeConfirmation(for: [6]))
        XCTAssertNil(ws.closeConfirmation(for: [8])) // 已断开，即使计数残留
        XCTAssertEqual(ws.closeConfirmation(for: [2]), CloseConfirmation(sessions: [2], interruptsTransfers: false))
        XCTAssertEqual(ws.closeConfirmation(for: [3]), CloseConfirmation(sessions: [3], interruptsTransfers: false))
        XCTAssertEqual(ws.closeConfirmation(for: [4]), CloseConfirmation(sessions: [4], interruptsTransfers: false))
        XCTAssertEqual(ws.closeConfirmation(for: [5]), CloseConfirmation(sessions: [5], interruptsTransfers: false))
        XCTAssertEqual(ws.closeConfirmation(for: [7]), CloseConfirmation(sessions: [7], interruptsTransfers: true))
        XCTAssertEqual(
            ws.closeConfirmation(for: [1, 4, 6, 7, 2]),
            CloseConfirmation(sessions: [4, 7, 2], interruptsTransfers: true)
        )
    }
}
