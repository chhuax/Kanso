import CoreGraphics

/// 标签拖拽排序状态机（§2.4）。
///
/// 视图层使用约定：
/// - 鼠标按下时创建；**新的 mouseDown 之前先把旧值置 nil**，残留状态不能阻塞后续操作。
/// - 只有正常的左键松开才调用 `commitOrder()` 并 `Workspace.moveTab`。
/// - 丢失松开事件、被打断、触控板滚动等取消场景：直接丢弃这个值，不调用 `commitOrder`。
/// - 只在同一个分屏组内排序，`rows` 只传同组标签。
public struct TabReorderDrag: Sendable {
    /// 纵向移动超过这个距离才激活，避免普通点击误触发排序。
    public static let activationDistance: CGFloat = 5

    public private(set) var isActive = false

    private let dragged: SessionID
    private let startY: CGFloat
    /// 其他行（不含被拖行）的中点，按显示顺序，begin 时快照。
    private let otherMidpoints: [(id: SessionID, midY: CGFloat)]
    private var pointerY: CGFloat

    /// `rows` 是同组所有标签按显示顺序（含被拖行），y 向下。
    ///
    /// 中点用 begin 时快照的其他行位置计算，被拖动的行本身不参与：
    /// 列表因让位而移动时基准不变，不会越拖越偏。
    public init(dragged: SessionID, startY: CGFloat, rows: [(id: SessionID, minY: CGFloat, maxY: CGFloat)]) {
        self.dragged = dragged
        self.startY = startY
        pointerY = startY
        otherMidpoints = rows.filter { $0.id != dragged }.map { ($0.id, ($0.minY + $0.maxY) / 2) }
    }

    /// 更新指针位置。一旦激活就保持激活，即使指针回到起点附近（用户可能是想拖回原位）。
    public mutating func update(pointerY: CGFloat) {
        self.pointerY = pointerY
        if !isActive, abs(pointerY - startY) > Self.activationDistance {
            isActive = true
        }
    }

    /// 插入位置：其他行中中点 < 指针的行数。未激活为 nil。
    public var insertionIndex: Int? {
        guard isActive else { return nil }
        return otherMidpoints.filter { $0.midY < pointerY }.count
    }

    /// 新顺序。未激活（普通点击）返回 nil。
    public func commitOrder() -> [SessionID]? {
        guard let index = insertionIndex else { return nil }
        var ids = otherMidpoints.map(\.id)
        ids.insert(dragged, at: min(index, ids.count))
        return ids
    }
}
