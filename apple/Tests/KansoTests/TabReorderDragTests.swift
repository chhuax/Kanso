import CoreGraphics
import KansoUI
import XCTest

final class TabReorderDragTests: XCTestCase {
    /// 5 行，每行高 40，从 y=0 起；id 为 1...5。
    private func makeRows() -> [(id: SessionID, minY: CGFloat, maxY: CGFloat)] {
        var rows: [(id: SessionID, minY: CGFloat, maxY: CGFloat)] = []
        for i in 0..<5 {
            let top = CGFloat(i * 40)
            rows.append((id: SessionID(i + 1), minY: top, maxY: top + 40))
        }
        return rows
    }

    private func drag(_ id: SessionID, startY: CGFloat) -> TabReorderDrag {
        TabReorderDrag(dragged: id, startY: startY, rows: makeRows())
    }

    func testSmallMovementDoesNotActivate() {
        var d = drag(2, startY: 60)
        d.update(pointerY: 65)
        d.update(pointerY: 55)
        XCTAssertFalse(d.isActive)
        XCTAssertNil(d.insertionIndex)
        XCTAssertNil(d.commitOrder())
    }

    func testActivatesBeyondThreshold() {
        var d = drag(2, startY: 60)
        d.update(pointerY: 65.5)
        XCTAssertTrue(d.isActive)
        XCTAssertNotNil(d.commitOrder())
    }

    func testStaysActiveAfterReturningToStart() {
        var d = drag(2, startY: 60)
        d.update(pointerY: 80)
        d.update(pointerY: 60)
        XCTAssertTrue(d.isActive)
        XCTAssertEqual(d.commitOrder(), [1, 2, 3, 4, 5])
    }

    func testDragDownPastOneMidpoint() {
        // 其他行中点：1→20, 3→100, 4→140, 5→180。拖行 2 到 y=105，越过 1 和 3。
        var d = drag(2, startY: 60)
        d.update(pointerY: 105)
        XCTAssertEqual(d.insertionIndex, 2)
        XCTAssertEqual(d.commitOrder(), [1, 3, 2, 4, 5])
    }

    func testDragToTopAndBottom() {
        var d = drag(3, startY: 100)
        d.update(pointerY: -50)
        XCTAssertEqual(d.insertionIndex, 0)
        XCTAssertEqual(d.commitOrder(), [3, 1, 2, 4, 5])
        d.update(pointerY: 999)
        XCTAssertEqual(d.insertionIndex, 4)
        XCTAssertEqual(d.commitOrder(), [1, 2, 4, 5, 3])
    }

    func testDraggedRowDoesNotParticipate() {
        // 拖第 1 行（中点 20）。若自身参与，指针在 y=30 时会算出 1 个（自身中点 20 < 30）；
        // 正确结果：其他行最小中点是 60，30 之前一个都没有 → 0，顺序不变。
        var d = drag(1, startY: 20)
        d.update(pointerY: 30)
        XCTAssertEqual(d.insertionIndex, 0)
        XCTAssertEqual(d.commitOrder(), [1, 2, 3, 4, 5])
        d.update(pointerY: 70)
        XCTAssertEqual(d.insertionIndex, 1)
        XCTAssertEqual(d.commitOrder(), [2, 1, 3, 4, 5])
    }
}
