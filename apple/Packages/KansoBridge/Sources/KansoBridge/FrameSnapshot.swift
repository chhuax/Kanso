import CKansoFFI

/// 单元格颜色编码的 Swift 侧解码结果（docs/frame-abi.md §4）。
///
/// 帧里只存"默认 / 索引 / RGB / 语义类别"，不存最终像素色；由渲染器按当前调色板解析，
/// 这样切换深浅色主题只需换调色板重画。
public enum FrameColor: Equatable, Sendable {
    case defaultColor
    case indexed(UInt8)
    case rgb(UInt8, UInt8, UInt8)
    /// 语义着色类别号，只会出现在前景色上。
    case semantic(UInt8)

    /// 解码 `KansoCell.fg` / `bg` 的 u32 编码。
    /// 遇到未知标签一律按默认色处理（ABI 要求），所以这里不能改成 fatalError。
    public init(raw: UInt32) {
        let payload = kanso_color_payload(raw)
        switch kanso_color_tag(raw) {
        case UInt32(KANSO_COLOR_TAG_INDEXED):
            self = .indexed(UInt8(truncatingIfNeeded: payload))
        case UInt32(KANSO_COLOR_TAG_RGB):
            self = .rgb(
                UInt8(truncatingIfNeeded: payload >> 16),
                UInt8(truncatingIfNeeded: payload >> 8),
                UInt8(truncatingIfNeeded: payload)
            )
        case UInt32(KANSO_COLOR_TAG_SEMANTIC):
            self = .semantic(UInt8(truncatingIfNeeded: payload))
        default:
            self = .defaultColor
        }
    }
}

/// 帧级标志（`KansoFrameHeader.flags`）。
public struct FrameFlags: OptionSet, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let altScreen = FrameFlags(rawValue: UInt32(KANSO_FRAME_ALT_SCREEN))
    public static let cursorVisible = FrameFlags(rawValue: UInt32(KANSO_FRAME_CURSOR_VISIBLE))
    public static let cursorBlink = FrameFlags(rawValue: UInt32(KANSO_FRAME_CURSOR_BLINK))
    public static let mouseReporting = FrameFlags(rawValue: UInt32(KANSO_FRAME_MOUSE_REPORTING))
    public static let focused = FrameFlags(rawValue: UInt32(KANSO_FRAME_FOCUSED))
}

/// 行级标志（`KansoLineMeta.flags`）。
public struct LineFlags: OptionSet, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let wrapped = LineFlags(rawValue: UInt32(KANSO_LINE_WRAPPED))
    public static let prompt = LineFlags(rawValue: UInt32(KANSO_LINE_PROMPT))
    public static let cursor = LineFlags(rawValue: UInt32(KANSO_LINE_CURSOR))
    public static let blank = LineFlags(rawValue: UInt32(KANSO_LINE_BLANK))
    public static let dirty = LineFlags(rawValue: UInt32(KANSO_LINE_DIRTY))
    public static let firstPrompt = LineFlags(rawValue: UInt32(KANSO_LINE_FIRST_PROMPT))
}

/// 单元格标志（`KansoCell.flags`，不含下划线样式位，样式见 `FrameCell.underlineStyle`）。
public struct CellFlags: OptionSet, Sendable {
    public let rawValue: UInt16
    public init(rawValue: UInt16) { self.rawValue = rawValue }

    public static let bold = CellFlags(rawValue: UInt16(KANSO_CELL_BOLD))
    public static let dim = CellFlags(rawValue: UInt16(KANSO_CELL_DIM))
    public static let italic = CellFlags(rawValue: UInt16(KANSO_CELL_ITALIC))
    public static let strike = CellFlags(rawValue: UInt16(KANSO_CELL_STRIKE))
    public static let inverse = CellFlags(rawValue: UInt16(KANSO_CELL_INVERSE))
    public static let hidden = CellFlags(rawValue: UInt16(KANSO_CELL_HIDDEN))
    /// 宽字符前半。
    public static let wide = CellFlags(rawValue: UInt16(KANSO_CELL_WIDE))
    /// 宽字符后半占位格。
    public static let wideSpacer = CellFlags(rawValue: UInt16(KANSO_CELL_WIDE_SPACER))
    public static let selected = CellFlags(rawValue: UInt16(KANSO_CELL_SELECTED))
    public static let match = CellFlags(rawValue: UInt16(KANSO_CELL_MATCH))
    /// 当前搜索匹配项（同时带 `match`）。
    public static let matchCurrent = CellFlags(rawValue: UInt16(KANSO_CELL_MATCH_CURRENT))
    public static let blink = CellFlags(rawValue: UInt16(KANSO_CELL_BLINK))
    public static let chip = CellFlags(rawValue: UInt16(KANSO_CELL_CHIP))
}

/// 光标形状（`KansoFrameHeader.cursor_shape`）。
public enum CursorShape: Sendable {
    case block, underline, bar, hollowBlock

    init(raw: UInt8) {
        switch UInt32(raw) {
        case UInt32(KANSO_CURSOR_UNDERLINE): self = .underline
        case UInt32(KANSO_CURSOR_BAR): self = .bar
        case UInt32(KANSO_CURSOR_HOLLOW_BLOCK): self = .hollowBlock
        default: self = .block
        }
    }
}

/// 光标位置（视口内的列、行）与形状。
public struct FrameCursor: Sendable {
    public let col: Int
    public let row: Int
    public let shape: CursorShape
}

/// 一行的元数据。
public struct FrameLineMeta: Sendable {
    public let absLine: UInt64
    /// 首次写出时的 Unix 毫秒；0 表示无。
    public let timestampMs: Int64
    public let flags: LineFlags
}

/// 一个单元格的值拷贝。
public struct FrameCell: Sendable {
    /// Unicode 标量；0 表示空单元格。
    public let ch: UInt32
    public let fg: FrameColor
    public let bg: FrameColor
    public let flags: CellFlags
    /// 下划线样式：0 无、1 单线、2 双线、3 波浪、4 点线、5 虚线。
    public let underlineStyle: UInt8
    /// 0 表示无附加信息；n 表示第 n 个 CellExtra。
    public let extra: UInt16
}

/// Rust 持有的一帧终端快照的只读句柄。
///
/// - 帧缓冲区在本对象存活期间只读，Rust 不会修改它；`deinit` 归还：mock 帧调用 `kanso_frame_destroy`，
///   来自 core 的帧（`init?(coreFrame:)`）则释放对 `FrameHandle` 的强引用，由 Rust 侧回收缓冲区。
/// - 不声明 `Sendable`：对象本身只在创建它的线程（目前是主线程）使用。ABI 允许多线程读取，
///   但需要跨线程时应由调用方保证"销毁在所有读取结束之后"，届时再显式设计共享方式，
///   不要为了让编译器通过就加 `@unchecked Sendable`。
/// - 访问器对越界的 `row` / `col` 直接 `precondition` 失败，因为底层 C 访问函数不做边界检查。
public final class FrameSnapshot {
    /// 手写 C ABI 创建的 mock 帧句柄；来自 core 的帧为 nil。
    private let handle: OpaquePointer?
    /// 来自 core 的帧：持有它就是保证缓冲区存活，必须与 `header` 同生命周期，不能为了"没用到"而删掉。
    private let coreFrame: FrameHandle?
    private let header: UnsafePointer<KansoFrameHeader>
    public private(set) lazy var commandBlocks: [FfiCommandBlock] = coreFrame?.commandBlocks() ?? []

    /// 来自 core 的帧句柄；mock 帧为 nil。供 `takeFrame(reuse:)` 把上一帧交还给 core 复用。
    ///
    /// 交还之后（且 core 返回了新帧），本对象及其所有访问器都不得再使用：缓冲区已被 core 回收。
    public var coreFrameHandle: FrameHandle? { coreFrame }

    /// 帧序号，同一终端单调递增。
    public var seq: UInt64 { header.pointee.seq }
    public var cols: Int { Int(header.pointee.cols) }
    public var rows: Int { Int(header.pointee.rows) }
    public var flags: FrameFlags { FrameFlags(rawValue: header.pointee.flags) }
    /// 显示行号 = `absLine - lineNumberBase + 1`。
    public var lineNumberBase: UInt64 { header.pointee.line_number_base }

    public var cursor: FrameCursor {
        let h = header.pointee
        return FrameCursor(
            col: Int(h.cursor_col),
            row: Int(h.cursor_row),
            shape: CursorShape(raw: h.cursor_shape)
        )
    }

    /// 创建确定性的 mock 帧；Rust 侧创建失败或帧头校验不通过时返回 nil。
    public static func mock(cols: Int, rows: Int, seq: UInt64) -> FrameSnapshot? {
        guard let cols = UInt16(exactly: cols), let rows = UInt16(exactly: rows),
              let handle = kanso_mock_frame_create(cols, rows, seq)
        else { return nil }
        return FrameSnapshot(adopting: handle)
    }

    /// 创建"满屏彩色文字"的 mock 帧（每个单元格都是可见字符、前景为 RGB 渐变），
    /// 仅用于渲染器性能测试；创建失败时返回 nil。
    public static func mockDense(cols: Int, rows: Int, seq: UInt64) -> FrameSnapshot? {
        guard let cols = UInt16(exactly: cols), let rows = UInt16(exactly: rows),
              let handle = kanso_mock_frame_create_dense(cols, rows, seq)
        else { return nil }
        return FrameSnapshot(adopting: handle)
    }

    /// 校验帧头：魔数、版本，以及 header_size 不小于自己已知的帧头（docs/frame-abi.md §5）。
    private static func isValid(_ header: UnsafePointer<KansoFrameHeader>) -> Bool {
        header.pointee.magic == UInt32(KANSO_FRAME_MAGIC)
            && header.pointee.version == UInt16(KANSO_FRAME_VERSION)
            && Int(header.pointee.header_size) >= MemoryLayout<KansoFrameHeader>.size
    }

    /// 接管 Rust 返回的句柄；校验失败时负责销毁，避免泄漏。
    private init?(adopting handle: OpaquePointer) {
        guard let header = kanso_frame_header(handle), Self.isValid(header) else {
            kanso_frame_destroy(handle)
            return nil
        }
        self.handle = handle
        self.coreFrame = nil
        self.header = header
    }

    /// 基于 core 返回的 `FrameHandle` 构造：按 `headerAddress()` 直接读 Rust 持有的缓冲区，零拷贝。
    ///
    /// 句柄已被回收（地址为 0）、地址不对齐、长度不足一个帧头或帧头校验失败时返回 nil。
    /// 生命周期约定见 docs/frame-abi.md §8：本对象强引用句柄，期间地址有效、内容不变；
    /// 把句柄交还 `KansoCore.takeFrame(reuse:)` 之后不得再读本对象。
    public init?(coreFrame frame: FrameHandle) {
        let address = frame.headerAddress()
        guard address != 0, address % 8 == 0,
              frame.byteLen() >= UInt64(MemoryLayout<KansoFrameHeader>.size),
              let header = UnsafePointer<KansoFrameHeader>(bitPattern: UInt(address)),
              Self.isValid(header)
        else { return nil }
        self.handle = nil
        self.coreFrame = frame
        self.header = header
    }

    deinit {
        if let handle { kanso_frame_destroy(handle) }
    }

    public func lineMeta(row: Int) -> FrameLineMeta {
        precondition(row >= 0 && row < rows, "row 越界")
        let m = kanso_frame_line_meta(header, UInt16(row)).pointee
        return FrameLineMeta(
            absLine: m.abs_line,
            timestampMs: m.timestamp_ms,
            flags: LineFlags(rawValue: m.flags)
        )
    }

    public func cell(row: Int, col: Int) -> FrameCell {
        let c = rawCell(row: row, col: col).pointee
        let style = (UInt32(c.flags) & UInt32(KANSO_CELL_UNDERLINE_MASK)) >> UInt32(KANSO_CELL_UNDERLINE_SHIFT)
        return FrameCell(
            ch: c.ch,
            fg: FrameColor(raw: c.fg),
            bg: FrameColor(raw: c.bg),
            flags: CellFlags(rawValue: c.flags),
            underlineStyle: UInt8(truncatingIfNeeded: style),
            extra: c.extra
        )
    }

    /// 单元格的字形簇文本（经 CellExtra 与字符串池）；没有字形簇时返回 nil，
    /// 此时单元格只有 `ch` 一个码点。
    public func grapheme(for cell: FrameCell) -> String? {
        // 这里没有单元格指针，只能按 extra 序号自己定位；与 C 头文件的
        // kanso_frame_extra / kanso_frame_grapheme 保持同一偏移公式。
        guard cell.extra != 0 else { return nil }
        let h = header.pointee
        let base = UnsafeRawPointer(header)
        let extra = (base + Int(h.extras_offset))
            .assumingMemoryBound(to: KansoCellExtra.self)
            .advanced(by: Int(cell.extra) - 1)
            .pointee
        guard extra.grapheme_len > 0 else { return nil }
        let bytes = UnsafeRawBufferPointer(
            start: base + Int(h.pool_offset) + Int(extra.grapheme_offset),
            count: Int(extra.grapheme_len)
        )
        return String(decoding: bytes, as: UTF8.self)
    }

    /// 单元格的独立下划线颜色（`CellExtra.underline_color`）；没有附加信息或颜色为 0
    /// （跟随前景）时返回 nil。
    public func underlineColor(for cell: FrameCell) -> FrameColor? {
        guard cell.extra != 0 else { return nil }
        let h = header.pointee
        let extra = (UnsafeRawPointer(header) + Int(h.extras_offset))
            .assumingMemoryBound(to: KansoCellExtra.self)
            .advanced(by: Int(cell.extra) - 1)
            .pointee
        return extra.underline_color == 0 ? nil : FrameColor(raw: extra.underline_color)
    }

    private func rawCell(row: Int, col: Int) -> UnsafePointer<KansoCell> {
        precondition(row >= 0 && row < rows && col >= 0 && col < cols, "row / col 越界")
        return kanso_frame_row(header, UInt16(row)) + col
    }
}
