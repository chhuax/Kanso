import AppKit
import KansoBridge
@testable import TerminalKit
import XCTest

/// 坐标换算、候选框位置、拖动防粘、滚动累积。
@MainActor
final class InputGeometryTests: XCTestCase {
    // MARK: 命中测试（纯几何）

    private let geometry = TerminalHitGeometry(
        originX: 50, originY: 10, cellWidth: 8, cellHeight: 16, cols: 10, rows: 5
    )

    func testCellAndSide() {
        // 第 2 列（x 66...74）的左半 / 右半，第 1 行（y 26...42）。
        XCTAssertEqual(geometry.viewportPoint(x: 67, y: 30), ViewportPointValue(col: 2, row: 1, side: .left))
        XCTAssertEqual(geometry.viewportPoint(x: 73, y: 30), ViewportPointValue(col: 2, row: 1, side: .right))
        // 恰在半格分界线上归右半。
        XCTAssertEqual(geometry.viewportPoint(x: 70, y: 10), ViewportPointValue(col: 2, row: 0, side: .right))
    }

    func testGutterAndPaddingClampToFirstColumnLeftSide() {
        XCTAssertEqual(geometry.viewportPoint(x: 5, y: 12), ViewportPointValue(col: 0, row: 0, side: .left))
        XCTAssertEqual(geometry.viewportPoint(x: 49.9, y: 12), ViewportPointValue(col: 0, row: 0, side: .left))
    }

    func testOutsideRightAndBottomClampToLastCell() {
        XCTAssertEqual(geometry.viewportPoint(x: 500, y: 500), ViewportPointValue(col: 9, row: 4, side: .right))
        XCTAssertEqual(geometry.viewportPoint(x: 60, y: -20), ViewportPointValue(col: 1, row: 0, side: .left))
    }

    func testInvalidGeometryReturnsNil() {
        var empty = geometry
        empty.cols = 0
        XCTAssertNil(empty.viewportPoint(x: 60, y: 20))
    }

    func testViewMouseEventUsesRendererLayoutIncludingGutter() throws {
        let (view, window) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        let g = try XCTUnwrap(view.hitGeometry())
        XCTAssertGreaterThan(g.originX, 0, "默认有时间戳栏")
        // 单元格 (3, 2) 的右半，换成窗口坐标（原点在左下）。
        let x = g.originX + 3.75 * g.cellWidth
        let yTop = g.originY + 2.25 * g.cellHeight
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: NSPoint(x: x, y: view.bounds.height - yTop), clickCount: 2))
        XCTAssertEqual(sink.mice, [TerminalMouseEvent(
            kind: .down, button: .left, point: ViewportPointValue(col: 3, row: 2, side: .right), clickCount: 2
        )])
        _ = window
    }

    // MARK: firstRect

    func testFirstRectFollowsCursorAndMarkedTextOffset() throws {
        let (view, window) = try InputFixtures.makeView()
        let source = MockFrameSource(cols: 100, rows: 30)
        view.frameSource = source
        view.pullFrameIfNeeded()
        let cursor = try XCTUnwrap(view.cursorCell)
        let g = try XCTUnwrap(view.hitGeometry())
        let none = NSRange(location: NSNotFound, length: 0)

        // 窗口无边框且 contentView 铺满：屏幕坐标 = 窗口 frame 原点 + 视图坐标。
        func expected(extraCols: Int) -> NSRect {
            let x = window.frame.minX + g.originX + Double(cursor.col + extraCols) * g.cellWidth
            let y = window.frame.minY + view.bounds.height - (g.originY + Double(cursor.row + 1) * g.cellHeight)
            return NSRect(x: x, y: y, width: g.cellWidth, height: g.cellHeight)
        }

        let plain = view.firstRect(forCharacterRange: NSRange(location: 0, length: 0), actualRange: nil)
        assertRect(plain, expected(extraCols: 0))

        // 组字 "你好a"：宽字符各占 2 列。起点在光标；字符位置 2（"你好" 之后）偏移 4 列。
        view.setMarkedText("你好a", selectedRange: NSRange(location: 3, length: 0), replacementRange: none)
        assertRect(view.firstRect(forCharacterRange: NSRange(location: 0, length: 3), actualRange: nil), expected(extraCols: 0))
        assertRect(view.firstRect(forCharacterRange: NSRange(location: 2, length: 0), actualRange: nil), expected(extraCols: 4))
        // 候选框以组字覆盖层的左边缘为准，覆盖层也显示出来了。
        XCTAssertFalse(view.markedOverlay.isHidden)
        XCTAssertEqual(view.markedOverlay.frame.width, 5 * g.cellWidth, accuracy: 0.01)
        view.lineHeight = 1.8
        let spaced = try XCTUnwrap(view.hitGeometry())
        XCTAssertGreaterThan(spaced.cellHeight, g.cellHeight)
        XCTAssertEqual(view.markedOverlay.frame.height, spaced.cellHeight, accuracy: 0.01)
        let candidate = view.firstRect(forCharacterRange: NSRange(location: 2, length: 0), actualRange: nil)
        XCTAssertEqual(candidate.height, spaced.cellHeight, accuracy: 0.01)
        XCTAssertEqual(candidate.minY, window.frame.minY + view.markedOverlay.frame.minY, accuracy: 0.01)
        view.unmarkText()
        XCTAssertTrue(view.markedOverlay.isHidden)
    }

    private func assertRect(_ actual: NSRect, _ expected: NSRect, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(actual.minX, expected.minX, accuracy: 0.01, file: file, line: line)
        XCTAssertEqual(actual.minY, expected.minY, accuracy: 0.01, file: file, line: line)
        XCTAssertEqual(actual.width, expected.width, accuracy: 0.01, file: file, line: line)
        XCTAssertEqual(actual.height, expected.height, accuracy: 0.01, file: file, line: line)
    }

    // MARK: 拖动防粘

    func testMissingMouseUpIsSynthesizedBeforeNextDown() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        let g = try XCTUnwrap(view.hitGeometry())
        func at(col: Double, row: Double) -> NSPoint {
            NSPoint(x: g.originX + (col + 0.2) * g.cellWidth, y: view.bounds.height - (g.originY + (row + 0.5) * g.cellHeight))
        }
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1)))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: at(col: 4, row: 1)))
        // 缺少 mouseUp，直接来了新的 Down。
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 7, row: 2)))
        XCTAssertEqual(sink.mice.map(\.kind), [.down, .drag, .up, .down])
        // 补发的 Up 落在最后已知位置。
        XCTAssertEqual(sink.mice[2].point, ViewportPointValue(col: 4, row: 1, side: .left))
        // 正常序列不会多发 Up。
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 7, row: 2)))
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 0, row: 0)))
        XCTAssertEqual(sink.mice.map(\.kind), [.down, .drag, .up, .down, .up, .down])
    }

    func testLosingFirstResponderEndsDrag() throws {
        let (view, window) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        XCTAssertTrue(window.makeFirstResponder(view))
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: NSPoint(x: 300, y: 200)))
        XCTAssertTrue(window.makeFirstResponder(nil))
        XCTAssertEqual(sink.mice.map(\.kind), [.down, .up])
    }

    func testRightAndMiddleButtons() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: NSPoint(x: 300, y: 200)))
        view.rightMouseUp(with: InputFixtures.mouse(.rightMouseUp, at: NSPoint(x: 300, y: 200)))
        XCTAssertEqual(sink.mice.map(\.button), [.right, .right])
        XCTAssertEqual(sink.mice.map(\.kind), [.down, .up])
    }

    func testFocusIsDeduplicated() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        view.setFocus(true)
        view.setFocus(true)
        view.setFocus(false)
        XCTAssertEqual(sink.events, [.focus(true), .focus(false)])
    }

    // MARK: 滚动累积

    func testAccumulatorEmitsOnlyWhenFullLineReached() {
        var acc = ScrollAccumulator()
        XCTAssertEqual(acc.add(delta: 5, unit: 16), 0)
        XCTAssertEqual(acc.add(delta: 5, unit: 16), 0)
        XCTAssertEqual(acc.add(delta: 5, unit: 16), 0)
        XCTAssertEqual(acc.add(delta: 5, unit: 16), 1, "累计 20 像素，满一行")
        XCTAssertEqual(acc.add(delta: 12, unit: 16), 1, "余量 4 + 12 = 16")
        XCTAssertEqual(acc.add(delta: 40, unit: 16), 2)
        // 反向：丢弃同向余量，再累积。
        XCTAssertEqual(acc.add(delta: -10, unit: 16), 0)
        XCTAssertEqual(acc.add(delta: -10, unit: 16), -1)
    }

    func testPreciseScrollEventsAccumulateInView() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        let g = try XCTUnwrap(view.hitGeometry())
        let step = ceil(g.cellHeight / 4)
        for _ in 0..<3 { view.scrollWheel(with: try scrollEvent(pixels: step)) }
        XCTAssertTrue(sink.events.isEmpty, "不足一个单元格高度不发")
        view.scrollWheel(with: try scrollEvent(pixels: step))
        XCTAssertEqual(sink.events, [.scroll(lines: 1, mods: [])])
    }

    func testWheelLinesAreNotAccumulatedAsPixels() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        view.scrollWheel(with: try scrollEvent(lines: 3))
        XCTAssertEqual(sink.events, [.scroll(lines: 3, mods: [])])
    }

    private func scrollEvent(pixels: Double) throws -> NSEvent {
        let cg = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: 0, wheel2: 0, wheel3: 0))
        cg.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
        cg.setDoubleValueField(.scrollWheelEventPointDeltaAxis1, value: pixels)
        return try XCTUnwrap(NSEvent(cgEvent: cg))
    }

    private func scrollEvent(lines: Int64) throws -> NSEvent {
        let cg = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: Int32(lines), wheel2: 0, wheel3: 0))
        return try XCTUnwrap(NSEvent(cgEvent: cg))
    }
}
