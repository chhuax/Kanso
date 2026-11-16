import CoreGraphics
import Foundation

/// 分屏（pane）的稳定标识。由 `Workspace` 单调分配，与像素、位置无关。
public struct PaneID: Hashable, Sendable, Comparable {
    public let raw: Int

    public init(raw: Int) {
        self.raw = raw
    }

    public static func < (lhs: PaneID, rhs: PaneID) -> Bool {
        lhs.raw < rhs.raw
    }
}

/// 拆分方向。
public enum SplitAxis: Sendable, Equatable {
    /// 左右排列（行）。
    case horizontal
    /// 上下排列（列）。
    case vertical
}

/// 布局树节点：叶子是 pane，内部节点按比例把空间分给子节点。
///
/// 外部只应通过 `SplitLayout` 的变更方法修改，这样规范形式才由同一处保证。
public indirect enum LayoutNode: Equatable, Sendable {
    case pane(PaneID)
    case split(SplitAxis, [LayoutChild])
}

/// 内部节点的一个子节点，`ratio` 是占父节点的份额。
public struct LayoutChild: Equatable, Sendable {
    public var node: LayoutNode
    public var ratio: Double

    public init(node: LayoutNode, ratio: Double) {
        self.node = node
        self.ratio = ratio
    }
}

/// 一条可拖动的分隔条（几何结果，供视图层命中测试与换算）。
public struct LayoutDivider: Equatable, Sendable {
    /// 从根到所属 split 的子节点下标序列（根本身是 split 时为 `[]`）。
    public var splitPath: [Int]
    /// 分隔条位于子节点 `index` 与 `index + 1` 之间。
    public var index: Int
    public var axis: SplitAxis
    public var rect: CGRect
    /// 所属 split 沿 axis 方向扣除全部分隔条后的可分配长度。
    /// 视图层用它把像素位移换算成 `moveDivider` 的 delta（delta = 像素 / parentLength）。
    public var parentLength: CGFloat

    public init(splitPath: [Int], index: Int, axis: SplitAxis, rect: CGRect, parentLength: CGFloat) {
        self.splitPath = splitPath
        self.index = index
        self.axis = axis
        self.rect = rect
        self.parentLength = parentLength
    }
}

/// 分屏布局树（值类型，与像素无关）。
///
/// 不变式（每次变更后都重新建立）：
/// - 同一 split 下子节点比例之和为 1；
/// - split 至少有 2 个子节点，单子节点的 split 被消掉；
/// - 子 split 与父 split 同方向时并入父节点；
/// - 根可以是单个 pane。
public struct SplitLayout: Equatable, Sendable {
    public private(set) var root: LayoutNode

    public init(single: PaneID) {
        root = .pane(single)
    }

    /// 阅读顺序：深度优先、按子节点数组顺序（行从左到右，列从上到下）。
    public var panes: [PaneID] {
        Self.collect(root)
    }

    public func contains(_ pane: PaneID) -> Bool {
        panes.contains(pane)
    }

    // MARK: - 结构变更

    /// 在 `pane` 后面（右边/下边）拆出 `newPane`，占 `pane` 原份额的一半。
    /// `pane` 不存在或 `newPane` 已存在时返回 false 且不改动。
    @discardableResult
    public mutating func split(_ pane: PaneID, axis: SplitAxis, newPane: PaneID) -> Bool {
        guard !contains(newPane) else { return false }
        if case let .pane(p) = root {
            guard p == pane else { return false }
            root = Self.pair(pane, newPane, axis: axis)
            return true
        }
        guard let replaced = Self.inserting(root, target: pane, axis: axis, new: newPane) else {
            return false
        }
        root = replaced.normalized()
        return true
    }

    /// 删除叶子，它的份额按兄弟现有比例成比例分给兄弟。
    /// 找不到，或它是最后一个 pane 时返回 false 且不改动。
    @discardableResult
    public mutating func remove(_ pane: PaneID) -> Bool {
        guard let replaced = Self.removing(root, target: pane) else { return false }
        root = replaced.normalized()
        return true
    }

    /// 拖动分隔条。
    ///
    /// - `delta` 是占该 split 总尺寸的份额，正数表示往右/下。
    /// - 只改 i、i+1 两个子节点，二者都不小于 `minFraction`；超出时夹到边界。
    /// - 若某一侧本来就小于 `minFraction`（例如窗口被缩得很小），不会被强行拉大，
    ///   只允许朝让它变大的方向移动，避免一次拖动造成跳变。
    /// - 路径或下标无效时什么都不做。
    public mutating func moveDivider(splitPath: [Int], dividerIndex: Int, delta: Double, minFraction: Double) {
        _ = Self.mutateSplit(&root, path: splitPath[...]) { children in
            guard dividerIndex >= 0, dividerIndex + 1 < children.count else { return }
            let a = children[dividerIndex].ratio
            let b = children[dividerIndex + 1].ratio
            let total = a + b
            let lower = min(minFraction, a)
            let upper = max(total - minFraction, a)
            let newA = min(max(a + delta, lower), upper)
            children[dividerIndex].ratio = newA
            children[dividerIndex + 1].ratio = total - newA
        }
    }

    // MARK: - 几何

    /// 各 pane 的矩形。rect 原点在左上（y 向下），行从左到右，列从上到下。
    ///
    /// 分隔条厚度先从可分配长度里扣除；各子矩形用累积比例计算并取整到整点，
    /// 最后一个子节点吃掉剩余。不能改成各自 round(ratio * 长度)：舍入误差会累积成缝隙或重叠。
    public func frames(in rect: CGRect, dividerThickness: CGFloat) -> [PaneID: CGRect] {
        var result: [PaneID: CGRect] = [:]
        Self.layout(root, rect: rect, thickness: dividerThickness, path: []) { pane, r in
            result[pane] = r
        } divider: { _ in }
        return result
    }

    /// 所有分隔条的几何信息，与 `frames` 使用同一套切分结果。
    public func dividers(in rect: CGRect, dividerThickness: CGFloat) -> [LayoutDivider] {
        var result: [LayoutDivider] = []
        Self.layout(root, rect: rect, thickness: dividerThickness, path: []) { _, _ in } divider: {
            result.append($0)
        }
        return result
    }

    // MARK: - 实现

    private static func collect(_ node: LayoutNode) -> [PaneID] {
        switch node {
        case let .pane(p): [p]
        case let .split(_, children): children.flatMap { collect($0.node) }
        }
    }

    private static func pair(_ pane: PaneID, _ new: PaneID, axis: SplitAxis) -> LayoutNode {
        .split(axis, [
            LayoutChild(node: .pane(pane), ratio: 0.5),
            LayoutChild(node: .pane(new), ratio: 0.5),
        ])
    }

    private static func inserting(_ node: LayoutNode, target: PaneID, axis: SplitAxis, new: PaneID) -> LayoutNode? {
        guard case .split(let a, var children) = node else { return nil }
        for i in children.indices {
            if case let .pane(p) = children[i].node, p == target {
                if a == axis {
                    // 同方向：作为同级插入，二者平分原份额，不产生嵌套。
                    let half = children[i].ratio / 2
                    children[i].ratio = half
                    children.insert(LayoutChild(node: .pane(new), ratio: half), at: i + 1)
                } else {
                    children[i].node = pair(target, new, axis: axis)
                }
                return .split(a, children)
            }
            if let replaced = inserting(children[i].node, target: target, axis: axis, new: new) {
                children[i].node = replaced
                return .split(a, children)
            }
        }
        return nil
    }

    private static func removing(_ node: LayoutNode, target: PaneID) -> LayoutNode? {
        guard case .split(let a, var children) = node else { return nil }
        if let i = children.firstIndex(where: { $0.node == .pane(target) }) {
            // 不在此处分配份额：留给 normalized() 按剩余兄弟的现有比例成比例归一。
            children.remove(at: i)
            return .split(a, children)
        }
        for i in children.indices {
            if let replaced = removing(children[i].node, target: target) {
                children[i].node = replaced
                return .split(a, children)
            }
        }
        return nil
    }

    private static func mutateSplit(
        _ node: inout LayoutNode,
        path: ArraySlice<Int>,
        _ body: (inout [LayoutChild]) -> Void
    ) -> Bool {
        guard case .split(let axis, var children) = node else { return false }
        if let head = path.first {
            guard children.indices.contains(head) else { return false }
            let ok = mutateSplit(&children[head].node, path: path.dropFirst(), body)
            if ok { node = .split(axis, children) }
            return ok
        }
        body(&children)
        node = .split(axis, children)
        return true
    }

    /// 沿 axis 方向切出各子节点的 [start, end] 区间（绝对坐标）。
    private static func spans(
        _ children: [LayoutChild], origin: CGFloat, length: CGFloat, thickness: CGFloat
    ) -> (spans: [(start: CGFloat, end: CGFloat)], available: CGFloat) {
        let n = children.count
        let available = max(0, length - thickness * CGFloat(n - 1))
        var result: [(CGFloat, CGFloat)] = []
        var cumulative = 0.0
        var previousEnd = origin
        for (k, child) in children.enumerated() {
            cumulative += child.ratio
            let start = k == 0 ? origin : previousEnd + thickness
            var end = k == n - 1
                ? origin + length
                : origin + (CGFloat(cumulative) * available).rounded() + CGFloat(k) * thickness
            end = max(end, start)
            result.append((start, end))
            previousEnd = end
        }
        return (result, available)
    }

    private static func layout(
        _ node: LayoutNode,
        rect: CGRect,
        thickness: CGFloat,
        path: [Int],
        pane: (PaneID, CGRect) -> Void,
        divider: (LayoutDivider) -> Void
    ) {
        switch node {
        case let .pane(p):
            pane(p, rect)
        case let .split(axis, children):
            let horizontal = axis == .horizontal
            let (cuts, available) = spans(
                children,
                origin: horizontal ? rect.minX : rect.minY,
                length: horizontal ? rect.width : rect.height,
                thickness: thickness
            )
            for (k, child) in children.enumerated() {
                let (s, e) = cuts[k]
                let childRect = horizontal
                    ? CGRect(x: s, y: rect.minY, width: e - s, height: rect.height)
                    : CGRect(x: rect.minX, y: s, width: rect.width, height: e - s)
                layout(child.node, rect: childRect, thickness: thickness, path: path + [k], pane: pane, divider: divider)
                if k < children.count - 1 {
                    let gap = cuts[k + 1].start - e
                    let r = horizontal
                        ? CGRect(x: e, y: rect.minY, width: gap, height: rect.height)
                        : CGRect(x: rect.minX, y: e, width: rect.width, height: gap)
                    divider(LayoutDivider(splitPath: path, index: k, axis: axis, rect: r, parentLength: available))
                }
            }
        }
    }
}

extension LayoutNode {
    /// 重建规范形式（自底向上）：并入同方向子 split、消掉单子节点 split、重新归一比例。
    /// 每次变更后都归一，避免浮点误差在多次操作后累积漂移。
    fileprivate func normalized() -> LayoutNode {
        guard case .split(let axis, let children) = self else { return self }
        var out: [LayoutChild] = []
        for child in children {
            let n = child.node.normalized()
            if case let .split(a, grand) = n, a == axis {
                for g in grand {
                    out.append(LayoutChild(node: g.node, ratio: g.ratio * child.ratio))
                }
            } else {
                out.append(LayoutChild(node: n, ratio: child.ratio))
            }
        }
        if out.count == 1 { return out[0].node }
        if out.isEmpty { return self }
        let sum = out.reduce(0.0) { $0 + max($1.ratio, 0) }
        for i in out.indices {
            out[i].ratio = sum > 0 ? max(out[i].ratio, 0) / sum : 1.0 / Double(out.count)
        }
        return .split(axis, out)
    }
}
