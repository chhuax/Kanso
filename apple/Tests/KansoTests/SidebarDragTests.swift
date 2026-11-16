import AppKit
import KansoUI
@testable import Kanso
import XCTest

/// 侧栏拖拽排序（§2.4）：直接驱动 `SidebarListView` 的拖拽处理方法。
@MainActor
final class SidebarDragTests: XCTestCase {
    private var list: SidebarListView!
    private var moves: [(SessionID, Int)] = []

    override func setUp() async throws {
        moves = []
        list = SidebarListView(frame: NSRect(x: 0, y: 0, width: 240, height: 400))
        list.onMove = { [unowned self] id, index in moves.append((id, index)) }
        list.update([section(ids: [1, 2, 3])])
        list.layoutSubtreeIfNeeded()
    }

    private func section(ids: [SessionID]) -> SidebarListView.Section {
        let rows = ids.enumerated().map { i, id in
            TabRowView.Model(
                item: TabItem(id: id, kind: .local, name: "t\(id)", connection: .connected),
                sidebarIndex: i, selection: .none
            )
        }
        return SidebarListView.Section(pane: PaneID(raw: 1), titleIndex: 1, showsHeader: false, isActivePane: true, rows: rows)
    }

    /// 第 i 行的垂直中点（home 位置）。
    private func midY(_ id: SessionID) -> CGFloat {
        let row = list.rowView(for: id)!
        return row.frame.midY
    }

    func testSmallMovementDoesNotReorder() {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(1) + 5) // 恰好 5pt：不超过阈值
        XCTAssertFalse(list.isDragActive)
        list.dragEnded()
        XCTAssertTrue(moves.isEmpty)
        XCTAssertFalse(list.isDragTracking)
    }

    func testStructuralChangesReuseSurvivingRows() {
        let first = list.rowView(for: 1)!
        let second = list.rowView(for: 2)!
        let third = list.rowView(for: 3)!
        XCTAssertTrue(list.update([section(ids: [3, 1, 4])]))
        XCTAssertTrue(list.rowView(for: 1) === first)
        XCTAssertTrue(list.rowView(for: 3) === third)
        XCTAssertNil(second.superview)
        XCTAssertEqual(list.rowViews.map { $0.model.item.id }, [3, 1, 4])
        XCTAssertEqual(first.model.sidebarIndex, 1)
        XCTAssertLessThan(third.frame.minY, first.frame.minY)
        XCTAssertTrue(list.update([section(ids: [1, 3, 4, 5])]))
        XCTAssertTrue(list.rowView(for: 1) === first)
        XCTAssertTrue(list.rowView(for: 3) === third)
    }

    func testDragPastMidpointReorders() {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(2) + 4) // 越过第 2 行的中点，没到第 3 行
        XCTAssertTrue(list.isDragActive)
        list.dragEnded()
        XCTAssertEqual(moves.count, 1)
        XCTAssertEqual(moves.first?.0, 1)
        XCTAssertEqual(moves.first?.1, 1)
    }

    func testDraggedRowFollowsPointerAndOthersMakeRoom() {
        let home1 = list.rowView(for: 1)!.frame.minY
        let home2 = list.rowView(for: 2)!.frame.minY
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(1) + 20)
        XCTAssertEqual(list.rowView(for: 1)!.frame.minY, home1 + 20, accuracy: 0.5)
        list.dragMoved(pointerY: midY(2) + 4)
        XCTAssertEqual(list.rowView(for: 2)!.frame.minY, home1, accuracy: 0.5, "第 2 行让到第 1 行原位")
        _ = home2
        list.cancelDrag()
        XCTAssertEqual(list.rowView(for: 1)!.frame.minY, home1, accuracy: 0.5, "取消后恢复原位")
        XCTAssertEqual(list.rowView(for: 2)!.frame.minY, home2, accuracy: 0.5)
    }

    func testScrollWheelCancelsDragWithoutReordering() throws {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(3))
        XCTAssertTrue(list.isDragActive)
        let cg = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: 3, wheel2: 0, wheel3: 0))
        let event = try XCTUnwrap(NSEvent(cgEvent: cg))
        list.scrollWheel(with: event)
        XCTAssertFalse(list.isDragTracking)
        list.dragEnded() // 之后即使收到松开，也不应排序
        XCTAssertTrue(moves.isEmpty)
        XCTAssertEqual(list.rowView(for: 1)!.frame.minY, SidebarListView.verticalInset, accuracy: 0.5)
    }

    func testNewMouseDownDiscardsStaleDrag() {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(3))
        XCTAssertTrue(list.isDragActive)
        // 丢失了上一次的松开事件，直接来了新的按下。
        list.dragBegan(session: 2, pointerY: midY(2))
        XCTAssertFalse(list.isDragActive)
        list.dragEnded()
        XCTAssertTrue(moves.isEmpty)
    }

    func testStructuralRefreshIsDeferredWhileDragging() {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(1) + 20)
        XCTAssertFalse(list.update([section(ids: [1, 2])]), "结构变化的重建要推迟")
        XCTAssertEqual(list.rowViews.count, 3)
        list.cancelDrag()
        XCTAssertTrue(list.update([section(ids: [1, 2])]))
        XCTAssertEqual(list.rowViews.count, 2)
    }

    func testNonStructuralRefreshKeepsDrag() {
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(1) + 20)
        var s = section(ids: [1, 2, 3])
        s.rows[1].item.name = "renamed"
        XCTAssertTrue(list.update([s]))
        XCTAssertTrue(list.isDragActive)
    }

    func testReorderDisabledWhileFiltering() {
        list.allowsReorder = false
        list.dragBegan(session: 1, pointerY: midY(1))
        list.dragMoved(pointerY: midY(3))
        list.dragEnded()
        XCTAssertTrue(moves.isEmpty)
    }
}
