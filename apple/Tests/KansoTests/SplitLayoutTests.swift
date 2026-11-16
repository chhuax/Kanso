import CoreGraphics
import KansoUI
import XCTest

final class SplitLayoutTests: XCTestCase {
    private let p1 = PaneID(raw: 1), p2 = PaneID(raw: 2), p3 = PaneID(raw: 3), p4 = PaneID(raw: 4)

    private func ratios(_ node: LayoutNode) -> [Double] {
        guard case let .split(_, c) = node else { return [] }
        return c.map(\.ratio)
    }

    /// 递归校验：每个 split 至少 2 子节点、比例和为 1、无同方向嵌套。
    private func assertCanonical(_ node: LayoutNode, parentAxis: SplitAxis? = nil, file: StaticString = #filePath, line: UInt = #line) {
        guard case let .split(axis, children) = node else { return }
        XCTAssertGreaterThanOrEqual(children.count, 2, file: file, line: line)
        XCTAssertNotEqual(axis, parentAxis, file: file, line: line)
        XCTAssertEqual(children.reduce(0) { $0 + $1.ratio }, 1, accuracy: 1e-9, file: file, line: line)
        for c in children { assertCanonical(c.node, parentAxis: axis, file: file, line: line) }
    }

    func testSameAxisSplitsAreSiblings() {
        var l = SplitLayout(single: p1)
        XCTAssertTrue(l.split(p1, axis: .horizontal, newPane: p2))
        XCTAssertTrue(l.split(p2, axis: .horizontal, newPane: p3))
        guard case let .split(axis, children) = l.root else { return XCTFail() }
        XCTAssertEqual(axis, .horizontal)
        XCTAssertEqual(children.count, 3)
        XCTAssertEqual(ratios(l.root)[0], 0.5, accuracy: 1e-9)
        XCTAssertEqual(ratios(l.root)[1], 0.25, accuracy: 1e-9)
        XCTAssertEqual(ratios(l.root)[2], 0.25, accuracy: 1e-9)
        XCTAssertEqual(l.panes, [p1, p2, p3])
        assertCanonical(l.root)
    }

    func testDifferentAxisNests() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.split(p2, axis: .vertical, newPane: p3)
        guard case let .split(_, c) = l.root, case let .split(a, inner) = c[1].node else { return XCTFail() }
        XCTAssertEqual(a, .vertical)
        XCTAssertEqual(inner.map(\.ratio), [0.5, 0.5])
        XCTAssertEqual(l.panes, [p1, p2, p3])
        assertCanonical(l.root)
    }

    func testSplitMissingOrDuplicateDoesNothing() {
        var l = SplitLayout(single: p1)
        let before = l
        XCTAssertFalse(l.split(p2, axis: .horizontal, newPane: p3))
        XCTAssertFalse(l.split(p1, axis: .horizontal, newPane: p1))
        XCTAssertEqual(l, before)
        l.split(p1, axis: .horizontal, newPane: p2)
        let two = l
        XCTAssertFalse(l.split(p3, axis: .vertical, newPane: p4))
        XCTAssertEqual(l, two)
    }

    func testRemoveDistributesProportionally() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2) // .5 .5
        l.split(p2, axis: .horizontal, newPane: p3) // .5 .25 .25
        XCTAssertTrue(l.remove(p3))
        // 剩 .5 / .25 → 2/3, 1/3
        XCTAssertEqual(ratios(l.root)[0], 2.0 / 3, accuracy: 1e-9)
        XCTAssertEqual(ratios(l.root)[1], 1.0 / 3, accuracy: 1e-9)
        assertCanonical(l.root)
    }

    func testRemoveCollapsesSingleChildAndMergesSameAxis() {
        // 行 [p1, 列[p2, p3]]，删 p3 → 列只剩 p2，顶替为行的第二个子节点。
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.split(p2, axis: .vertical, newPane: p3)
        XCTAssertTrue(l.remove(p3))
        guard case let .split(.horizontal, c) = l.root else { return XCTFail() }
        XCTAssertEqual(c.map(\.node), [.pane(p1), .pane(p2)])
        XCTAssertEqual(c.map(\.ratio), [0.5, 0.5])

        // 行 [p1, 列[p2, 行[p3, p4]]]，删 p2 → 列只剩行[p3,p4]，与外层同方向并入。
        var m = SplitLayout(single: p1)
        m.split(p1, axis: .horizontal, newPane: p2)
        m.split(p2, axis: .vertical, newPane: p3)
        m.split(p3, axis: .horizontal, newPane: p4)
        XCTAssertTrue(m.remove(p2))
        guard case let .split(.horizontal, mc) = m.root else { return XCTFail() }
        XCTAssertEqual(mc.count, 3)
        XCTAssertEqual(m.panes, [p1, p3, p4])
        assertCanonical(m.root)
    }

    func testRemoveLastOrMissingFails() {
        var l = SplitLayout(single: p1)
        XCTAssertFalse(l.remove(p1))
        XCTAssertFalse(l.remove(p2))
        l.split(p1, axis: .vertical, newPane: p2)
        XCTAssertTrue(l.remove(p1))
        XCTAssertEqual(l.root, .pane(p2))
        XCTAssertFalse(l.remove(p2))
    }

    func testRatiosStaySumOneAfterManyOps() {
        var l = SplitLayout(single: PaneID(raw: 0))
        for i in 1...12 {
            l.split(PaneID(raw: i - 1), axis: i % 3 == 0 ? .vertical : .horizontal, newPane: PaneID(raw: i))
            assertCanonical(l.root)
        }
        for i in stride(from: 0, through: 10, by: 2) {
            l.remove(PaneID(raw: i))
            assertCanonical(l.root)
        }
    }

    func testMoveDividerClamp() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: 0.1, minFraction: 0.1)
        XCTAssertEqual(ratios(l.root), [0.6, 0.4])
        // 往右拖过头：右侧夹到最小值。
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: 0.9, minFraction: 0.1)
        XCTAssertEqual(ratios(l.root)[0], 0.9, accuracy: 1e-9)
        XCTAssertEqual(ratios(l.root)[1], 0.1, accuracy: 1e-9)
        // 右侧已到最小值，继续往右不动；往左可以。
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: 0.2, minFraction: 0.1)
        XCTAssertEqual(ratios(l.root)[1], 0.1, accuracy: 1e-9)
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: -0.3, minFraction: 0.1)
        XCTAssertEqual(ratios(l.root)[0], 0.6, accuracy: 1e-9)
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: -5, minFraction: 0.1)
        XCTAssertEqual(ratios(l.root)[0], 0.1, accuracy: 1e-9)
    }

    func testMoveDividerBothAtMinimumDoesNothing() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: 0.1, minFraction: 0.5)
        XCTAssertEqual(ratios(l.root), [0.5, 0.5])
        l.moveDivider(splitPath: [], dividerIndex: 0, delta: -0.1, minFraction: 0.5)
        XCTAssertEqual(ratios(l.root), [0.5, 0.5])
    }

    func testMoveDividerInvalidPathDoesNothing() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        let before = l
        l.moveDivider(splitPath: [0], dividerIndex: 0, delta: 0.1, minFraction: 0.1) // 子节点是叶子
        l.moveDivider(splitPath: [5], dividerIndex: 0, delta: 0.1, minFraction: 0.1)
        l.moveDivider(splitPath: [], dividerIndex: 1, delta: 0.1, minFraction: 0.1)
        l.moveDivider(splitPath: [], dividerIndex: -1, delta: 0.1, minFraction: 0.1)
        XCTAssertEqual(l, before)
        var single = SplitLayout(single: p1)
        single.moveDivider(splitPath: [], dividerIndex: 0, delta: 0.1, minFraction: 0.1)
        XCTAssertEqual(single.root, .pane(p1))
    }

    func testMoveDividerNestedPath() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.split(p2, axis: .vertical, newPane: p3)
        l.moveDivider(splitPath: [1], dividerIndex: 0, delta: 0.2, minFraction: 0.1)
        guard case let .split(_, c) = l.root else { return XCTFail() }
        XCTAssertEqual(ratios(c[1].node)[0], 0.7, accuracy: 1e-9)
        assertCanonical(l.root)
    }

    func testFramesCoverRectWithoutGapsOrOverlap() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.split(p2, axis: .horizontal, newPane: p3)
        l.split(p3, axis: .vertical, newPane: p4)
        for width in [100.0, 101.0, 333.0, 1000.0] {
            let rect = CGRect(x: 10, y: 20, width: width, height: 200)
            let f = l.frames(in: rect, dividerThickness: 1)
            XCTAssertEqual(Set(f.keys), [p1, p2, p3, p4])
            // 行：p1、p2、列[p3,p4] 依次紧挨，间隔恰为分隔条厚度，首尾对齐 rect。
            XCTAssertEqual(f[p1]!.minX, rect.minX)
            XCTAssertEqual(f[p2]!.minX, f[p1]!.maxX + 1)
            XCTAssertEqual(f[p3]!.minX, f[p2]!.maxX + 1)
            XCTAssertEqual(f[p3]!.maxX, rect.maxX)
            XCTAssertEqual(f[p4]!.maxX, rect.maxX)
            XCTAssertEqual(f[p3]!.maxY + 1, f[p4]!.minY)
            XCTAssertEqual(f[p4]!.maxY, rect.maxY)
            XCTAssertEqual(f[p1]!.height, rect.height)
            for r in f.values {
                XCTAssertEqual(r.minX, r.minX.rounded())
                XCTAssertEqual(r.width, r.width.rounded())
            }
            let rects = Array(f.values)
            for i in rects.indices {
                for j in rects.indices where i < j {
                    XCTAssertTrue(rects[i].intersection(rects[j]).isNull || rects[i].intersection(rects[j]).isEmpty)
                }
            }
        }
    }

    func testDividers() {
        var l = SplitLayout(single: p1)
        l.split(p1, axis: .horizontal, newPane: p2)
        l.split(p2, axis: .vertical, newPane: p3)
        let rect = CGRect(x: 0, y: 0, width: 201, height: 101)
        let d = l.dividers(in: rect, dividerThickness: 1)
        XCTAssertEqual(d.count, 2)
        let root = d.first { $0.splitPath == [] }!
        XCTAssertEqual(root.axis, .horizontal)
        XCTAssertEqual(root.index, 0)
        XCTAssertEqual(root.parentLength, 200)
        XCTAssertEqual(root.rect, CGRect(x: 100, y: 0, width: 1, height: 101))
        let inner = d.first { $0.splitPath == [1] }!
        XCTAssertEqual(inner.axis, .vertical)
        XCTAssertEqual(inner.parentLength, 100)
        XCTAssertEqual(inner.rect, CGRect(x: 101, y: 50, width: 100, height: 1))
        XCTAssertTrue(SplitLayout(single: p1).dividers(in: rect, dividerThickness: 1).isEmpty)
        XCTAssertEqual(SplitLayout(single: p1).frames(in: rect, dividerThickness: 1), [p1: rect])
    }
}
