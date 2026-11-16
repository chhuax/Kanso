import KansoBridge

/// 终端帧的来源。语义与 core 的 `take_frame` 一致：**只有变脏时才返回新帧**。
///
/// 渲染器按 display link 节拍调用 `takeFrame`；core 一侧在终端由干净变脏时通过
/// `TerminalDirty` 事件通知 UI，UI 再调用 `TerminalView.setNeedsFrame()`。
/// 真实实现由会话层（I3）接 core FFI，这里只定义契约。
@MainActor
public protocol FrameSource: AnyObject {
    /// 取最新一帧；自上次取走后没有变化时返回 nil。
    ///
    /// - Parameter previous: 视图当前持有的上一帧，供实现复用缓冲区。调用方保证：传入后不再读取它，
    ///   实现若不复用则忽略即可。
    func takeFrame(reusing previous: FrameSnapshot?) -> FrameSnapshot?
}

/// 用 FFI 的 mock 帧实现的 `FrameSource`：`advance()` 把内部序号加一并置脏，
/// 下一次 `takeFrame` 返回新序号的帧（mock 内容随序号整体上移一行）。
@MainActor
public final class MockFrameSource: FrameSource {
    public private(set) var seq: UInt64
    public private(set) var cols: Int
    public private(set) var rows: Int
    private var dirty = true

    public init(cols: Int = 100, rows: Int = 30, seq: UInt64 = 0) {
        self.cols = cols
        self.rows = rows
        self.seq = seq
    }

    /// 序号加一并置脏。调用方随后应调用 `TerminalView.setNeedsFrame()`（对应 core 的 `TerminalDirty`）。
    public func advance() {
        seq += 1
        dirty = true
    }

    /// 改变帧的网格尺寸（对应 core 的 resize），并置脏。
    public func resize(cols: Int, rows: Int) {
        guard cols != self.cols || rows != self.rows else { return }
        self.cols = cols
        self.rows = rows
        dirty = true
    }

    public func takeFrame(reusing previous: FrameSnapshot?) -> FrameSnapshot? {
        guard dirty else { return nil }
        dirty = false
        return FrameSnapshot.mock(cols: cols, rows: rows, seq: seq)
    }
}
