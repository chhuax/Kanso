import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

/// 顶部标签条（ui-shell.md §2.5）：标签条自身的布局与横向拖拽，以及分屏区按 pane 摆放标签条。
@MainActor
final class TopTabStripTests: XCTestCase {
    private var controllers: [WorkspaceController] = []

    override func tearDown() async throws {
        for c in controllers { c.shutdown() }
        controllers = []
    }

    private func makeController() -> WorkspaceController {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let controller = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true))
        controllers.append(controller)
        return controller
    }

    private func models(_ ids: [SessionID]) -> [TabRowView.Model] {
        ids.enumerated().map { i, id in
            TabRowView.Model(
                item: TabItem(id: id, kind: .local, name: "t\(id)", connection: .connected), sidebarIndex: i, selection: .none
            )
        }
    }

    private func makeStrip(width: CGFloat, ids: [SessionID]) -> TabStripView {
        let strip = TabStripView(pane: PaneID(raw: 1))
        strip.frame = NSRect(x: 0, y: 0, width: width, height: TabStripView.height)
        strip.update(models(ids), activeTab: ids.first)
        strip.layoutSubtreeIfNeeded()
        return strip
    }

    // MARK: - 标签条

    func testChipsAreEqualWidthAndClampedBetweenMinAndMax() {
        let roomy = makeStrip(width: 1200, ids: [1, 2])
        XCTAssertEqual(roomy.chips.map(\.frame.width), [TabStripView.chipMaxWidth, TabStripView.chipMaxWidth])
        XCTAssertTrue(roomy.chips.allSatisfy { $0.style == .chip })

        let crowded = makeStrip(width: 300, ids: [1, 2, 3, 4, 5])
        let widths = Set(crowded.chips.map(\.frame.width))
        XCTAssertEqual(widths, [TabStripView.chipMinWidth], "放不下时缩到最小宽度后横向滚动，不再继续变窄")
        XCTAssertLessThan(crowded.chips[0].frame.minX, crowded.chips[1].frame.minX)
    }

    func testHorizontalDragPastMidpointReorders() {
        let strip = makeStrip(width: 1200, ids: [1, 2, 3])
        var moves: [(SessionID, Int)] = []
        strip.onMove = { moves.append(($0, $1)) }
        let start = strip.chip(for: 1)!.frame.midX
        strip.dragBegan(session: 1, pointerX: start)
        strip.dragMoved(pointerX: start + 5) // 恰好 5pt：不超过阈值
        XCTAssertFalse(strip.isDragActive)
        strip.dragMoved(pointerX: strip.chip(for: 2)!.frame.midX + 4)
        XCTAssertTrue(strip.isDragActive)
        strip.dragEnded()
        XCTAssertEqual(moves.count, 1)
        XCTAssertEqual(moves.first?.0, 1)
        XCTAssertEqual(moves.first?.1, 1)
    }

    func testCancelledDragRestoresLayoutAndKeepsOrder() {
        let strip = makeStrip(width: 1200, ids: [1, 2, 3])
        var moves: [(SessionID, Int)] = []
        strip.onMove = { moves.append(($0, $1)) }
        let home = strip.chips.map(\.frame.minX)
        strip.dragBegan(session: 1, pointerX: strip.chip(for: 1)!.frame.midX)
        strip.dragMoved(pointerX: strip.chip(for: 3)!.frame.midX + 4)
        XCTAssertEqual(strip.chip(for: 2)!.frame.minX, home[0], accuracy: 0.5, "其他标签让位")
        strip.cancelDrag()
        XCTAssertTrue(moves.isEmpty)
        XCTAssertEqual(strip.chips.map(\.frame.minX), home)
    }

    func testStructureChangeDuringDragIsDeferred() {
        let strip = makeStrip(width: 1200, ids: [1, 2])
        strip.dragBegan(session: 1, pointerX: strip.chip(for: 1)!.frame.midX)
        XCTAssertFalse(strip.update(models([1, 2, 3]), activeTab: 1))
        XCTAssertEqual(strip.chips.count, 2)
        strip.cancelDrag()
        XCTAssertTrue(strip.update(models([1, 2, 3]), activeTab: 1))
        XCTAssertEqual(strip.chips.map(\.model.item.id), [1, 2, 3])
    }

    // MARK: - 分屏区

    func testEachPaneGetsItsOwnStripWithOnlyItsTabs() throws {
        let c = makeController()
        let a = try c.openLocalShell()
        let b = try c.openLocalShell()
        let d = try c.split(axis: .horizontal)
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 1000, height: 400)
        container.showsTabStrips = true
        container.layoutSubtreeIfNeeded()

        let left = try XCTUnwrap(c.workspace.pane(of: a))
        let right = try XCTUnwrap(c.workspace.pane(of: d))
        let leftStrip = try XCTUnwrap(container.tabStrip(for: left))
        let rightStrip = try XCTUnwrap(container.tabStrip(for: right))
        XCTAssertEqual(leftStrip.chips.map(\.model.item.id), [a, b])
        XCTAssertEqual(rightStrip.chips.map(\.model.item.id), [d])
        XCTAssertEqual(rightStrip.chip(for: d)?.model.selection, .windowActive)
        XCTAssertEqual(leftStrip.chip(for: b)?.model.selection, .paneActive)

        // 终端在标签条下方，不被盖住。
        let view = try XCTUnwrap(c.host(for: d)?.view)
        XCTAssertEqual(view.frame.minY, rightStrip.frame.maxY, accuracy: 0.5)
        XCTAssertEqual(view.frame.height, 400 - TabStripView.height, accuracy: 0.5)
        XCTAssertLessThan(leftStrip.frame.maxX, rightStrip.frame.minX)
    }

    func testHidingStripsGivesTerminalTheFullPane() throws {
        let c = makeController()
        let a = try c.openLocalShell()
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        container.showsTabStrips = true
        container.layoutSubtreeIfNeeded()
        XCTAssertNotNil(container.tabStrip(for: c.workspace.activePane))

        container.showsTabStrips = false
        XCTAssertNil(container.tabStrip(for: c.workspace.activePane))
        XCTAssertEqual(try XCTUnwrap(c.host(for: a)?.view).frame.height, 400, accuracy: 0.5)
    }

    func testNoStripOverEmptyState() async throws {
        let c = makeController()
        let a = try c.openLocalShell()
        _ = await c.requestClose([a])
        let container = PaneContainerView(controller: c)
        container.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        container.showsTabStrips = true
        container.layoutSubtreeIfNeeded()
        XCTAssertNil(container.tabStrip(for: c.workspace.activePane), "唯一 pane 且没有标签时只显示空状态页")
    }

    // MARK: - 设置与菜单

    func testTabBarPositionRoundTripsAndFallsBackToLeft() throws {
        var appearance = AppearanceSettings()
        XCTAssertEqual(appearance.tabBarPosition, .left)
        appearance.tabBarPosition = .top
        let json = SettingsCodec.encode(appearance)
        XCTAssertTrue(json.contains(#""tabBarPosition":"top""#))
        XCTAssertEqual(SettingsCodec.decode(AppearanceSettings.self, from: json)?.tabBarPosition, .top)
        XCTAssertEqual(SettingsCodec.decode(AppearanceSettings.self, from: #"{"tabBarPosition":"bottom"}"#)?.tabBarPosition, .left)
        XCTAssertEqual(SettingsCodec.decode(AppearanceSettings.self, from: #"{"mode":"dark"}"#)?.tabBarPosition, .left)
    }

    func testHorizontalMenuSaysLeftAndRight() throws {
        let c = makeController()
        let a = try c.openLocalShell()
        let menu = TabContextMenu(controller: c)
        let horizontal = menu.makeMenu(for: a, horizontal: true).items.map(\.title)
        XCTAssertTrue(horizontal.contains(String(localized: "Close Tabs to the Left")))
        XCTAssertTrue(horizontal.contains(String(localized: "Close Tabs to the Right")))
        let vertical = menu.makeMenu(for: a, horizontal: false).items.map(\.title)
        XCTAssertTrue(vertical.contains(String(localized: "Close Tabs Above")))
    }
}
