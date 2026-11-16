import AppKit

// 终端视图向外输出的输入事件类型。
//
// 这些类型与 `crates/kanso-types/src/input.rs` 一一对应，I3（会话层）负责把它们映射成 Rust 类型。
// 视图只描述"用户做了什么"，不做任何转义或编码：按键编码、鼠标上报、bracketed paste 等
// 全部在 core 里完成（architecture §5、terminal.md §8/§9）。

/// 修饰键状态。对应 Rust `kanso_types::input::Modifiers`。
///
/// Alt 区分左右：因为"Option 作为 Meta"可以只对一侧生效（terminal.md §9）。
public struct KeyModifiers: OptionSet, Hashable, Sendable {
    public let rawValue: UInt8
    public init(rawValue: UInt8) { self.rawValue = rawValue }

    /// Rust `shift`。
    public static let shift = KeyModifiers(rawValue: 1 << 0)
    /// Rust `ctrl`。
    public static let ctrl = KeyModifiers(rawValue: 1 << 1)
    /// Rust `alt_left`：左 Option。
    public static let altLeft = KeyModifiers(rawValue: 1 << 2)
    /// Rust `alt_right`：右 Option。
    public static let altRight = KeyModifiers(rawValue: 1 << 3)
    /// Rust `meta`：macOS 的 ⌘。
    public static let meta = KeyModifiers(rawValue: 1 << 4)

    /// 任意一侧 Option 是否按下。对应 Rust `Modifiers::alt()`。
    public var hasAlt: Bool { !isDisjoint(with: [.altLeft, .altRight]) }
}

/// 逻辑键。对应 Rust `kanso_types::input::Key`（`function` 对应 `F`）。
public enum TerminalKey: Hashable, Sendable {
    /// 字符键：该键在当前布局下**不带修饰键**时产生的字符（Rust `Key::Character`）。
    case character(String)
    case enter
    case tab
    case backspace
    case escape
    case space
    case arrowUp
    case arrowDown
    case arrowLeft
    case arrowRight
    case home
    case end
    case pageUp
    case pageDown
    case insert
    /// 向前删除（Rust `Key::Delete`）。
    case delete
    /// 功能键，取值 1...24（Rust `Key::F`）。
    case function(UInt8)
}

/// 按键事件类型。对应 Rust `KeyEventKind`；kitty keyboard protocol 需要区分 release。
public enum KeyEventKind: Hashable, Sendable {
    case press
    /// 按住不放产生的自动重复（`NSEvent.isARepeat`）。
    case `repeat`
    case release
}

/// 一次按键。对应 Rust `KeyEvent`。
///
/// IME 组字期间的按键**不会**产生本事件；IME 上屏的文字走 `TerminalInputSink.textInput`（terminal.md §16）。
public struct TerminalKeyEvent: Hashable, Sendable {
    public var key: TerminalKey
    public var mods: KeyModifiers
    /// 系统按当前布局和修饰键产生的文本（Rust `text: Option<String>`）；非文本键为 nil。
    public var text: String?
    public var kind: KeyEventKind

    public init(key: TerminalKey, mods: KeyModifiers = [], text: String? = nil, kind: KeyEventKind = .press) {
        self.key = key
        self.mods = mods
        self.text = text
        self.kind = kind
    }
}

/// 鼠标按键。对应 Rust `MouseButton`。
public enum TerminalMouseButton: Hashable, Sendable {
    case left
    case middle
    case right
    /// 没有按键（单纯移动）。
    case none
}

/// 鼠标事件类型。对应 Rust `MouseEventKind`。
public enum TerminalMouseEventKind: Hashable, Sendable {
    case down
    case up
    /// 按住按键移动。
    case drag
    /// 未按键移动。
    case move
}

/// 落在单元格的左半还是右半。对应 Rust `kanso_types::geometry::CellSide`。
/// 选区端点需要半格精度（terminal.md §8.1）。
public enum TerminalCellSide: Hashable, Sendable {
    case left
    case right
}

/// 视口内坐标（相对可见区域，不是绝对行号）。对应 Rust `ViewportPoint`。
public struct ViewportPointValue: Hashable, Sendable {
    /// 列号，从 0 起（已扣除时间戳栏与留白）。
    public var col: UInt16
    /// 视口内行号，从 0 起。
    public var row: UInt16
    public var side: TerminalCellSide

    public init(col: UInt16, row: UInt16, side: TerminalCellSide) {
        self.col = col
        self.row = row
        self.side = side
    }
}

/// 一次鼠标事件。对应 Rust `MouseEvent`。
public struct TerminalMouseEvent: Hashable, Sendable {
    public var kind: TerminalMouseEventKind
    public var button: TerminalMouseButton
    public var point: ViewportPointValue
    public var mods: KeyModifiers
    /// 连击次数，从 1 起（Rust `click_count: u8`）。
    public var clickCount: UInt8

    public init(
        kind: TerminalMouseEventKind, button: TerminalMouseButton, point: ViewportPointValue,
        mods: KeyModifiers = [], clickCount: UInt8 = 1
    ) {
        self.kind = kind
        self.button = button
        self.point = point
        self.mods = mods
        self.clickCount = clickCount
    }
}

/// `TerminalView` 的输入出口。由会话层（I3）实现并映射成 core 调用。
///
/// 所有方法都在主线程调用。**实现不得记录按键内容**到日志（architecture §13）。
@MainActor
public protocol TerminalInputSink: AnyObject {
    /// 一次按键（含 press / repeat / release）。
    func key(_ event: TerminalKeyEvent)
    /// IME 上屏文字，不经按键编码（对应 Rust `Core::text_input`，terminal.md §16）。
    func textInput(_ text: String)
    /// 鼠标事件。是否上报给程序、是否做选区由 core 决定。
    func mouse(_ event: TerminalMouseEvent)
    /// 滚轮滚动。`lines` 的符号与 `NSEvent.scrollingDeltaY` 一致（系统已按"自然滚动"设置翻转）：
    /// 正数表示内容向下移动、露出更早的输出；负数相反。
    func scroll(lines: Int32, at: ViewportPointValue, mods: KeyModifiers)
    /// 视图焦点变化（窗口 key 状态与 first responder 的合成结果）。
    func focus(_ focused: Bool)
}

/// 把"事件交给输入法"抽象出来，使 IME 序列可以在测试里用假输入法驱动。
///
/// 生产环境不需要设置：`TerminalView.inputMethod` 为 nil 时直接使用 `NSTextInputContext`。
@MainActor
public protocol TerminalInputMethod: AnyObject {
    /// 把按键或 flagsChanged 事件交给输入法；返回输入法是否处理了它。
    /// 输入法通过 `TerminalView` 的 `NSTextInputClient` 方法回调结果。
    func handleEvent(_ event: NSEvent) -> Bool
    /// 视图失焦等原因强行终止组字时调用。
    func discardMarkedText()
}

/// 滚动累积器：把连续的小增量累积成整行。
///
/// 触控板的精确滚动每次只有几个像素，若每个事件都取整会丢失全部小增量；因此保留余量，满一个单位才产生一行。
/// 方向反转时丢弃反向的余量，否则用户掉头滚动会先"还债"而感觉迟钝。
struct ScrollAccumulator {
    private var remainder = 0.0

    /// - Parameters:
    ///   - delta: 本次增量（像素或行，与 `unit` 同一单位）。
    ///   - unit: 一行对应的大小。
    /// - Returns: 本次应发出的行数（可为 0），符号与 `delta` 一致。
    mutating func add(delta: Double, unit: Double) -> Int32 {
        guard unit > 0, delta != 0 else { return 0 }
        if remainder != 0, (remainder > 0) != (delta > 0) { remainder = 0 }
        remainder += delta
        let lines = (remainder / unit).rounded(.towardZero)
        remainder -= lines * unit
        return Int32(max(-1000, min(1000, lines)))
    }

    mutating func reset() { remainder = 0 }
}

/// 终端网格的命中测试参数，与 `TerminalRenderer.Layout` 的设备像素几何保持同一套取整。
///
/// 独立成纯值类型是为了不依赖窗口与 Metal 就能测试坐标换算。
public struct TerminalHitGeometry: Equatable, Sendable {
    /// 正文区左上角（视图内，pt，原点在左上）。栏宽与留白都已包含在内。
    public var originX: Double
    public var originY: Double
    /// 单元格尺寸（pt）。
    public var cellWidth: Double
    public var cellHeight: Double
    public var cols: Int
    public var rows: Int

    public init(originX: Double, originY: Double, cellWidth: Double, cellHeight: Double, cols: Int, rows: Int) {
        self.originX = originX
        self.originY = originY
        self.cellWidth = cellWidth
        self.cellHeight = cellHeight
        self.cols = cols
        self.rows = rows
    }

    /// 视图内的点（pt，**原点在左上**）换算为视口单元格。
    ///
    /// 落在时间戳栏、留白或视图外的点一律夹取到最近的有效单元格，而不是忽略：
    /// 1) 从栏区开始向右拖动应当仍然能选到整行（选区起点落在第 0 列左半）；
    /// 2) 拖动出视图边界时，core 需要持续收到边缘坐标才能继续扩展选区；
    /// 3) 鼠标上报协议里坐标必须有效，xterm 同样按边缘夹取。
    public func viewportPoint(x: Double, y: Double) -> ViewportPointValue? {
        guard cols > 0, rows > 0, cellWidth > 0, cellHeight > 0 else { return nil }
        let fx = (x - originX) / cellWidth
        let fy = (y - originY) / cellHeight
        let col = min(max(Int(fx.rounded(.down)), 0), cols - 1)
        let row = min(max(Int(fy.rounded(.down)), 0), rows - 1)
        // 夹取到边缘后半格信息没有意义：左侧外部取左半，右侧外部取右半，保证选区能覆盖首尾字符。
        let side: TerminalCellSide
        if fx < 0 {
            side = .left
        } else if Int(fx.rounded(.down)) >= cols {
            side = .right
        } else {
            side = (fx - fx.rounded(.down)) < 0.5 ? .left : .right
        }
        return ViewportPointValue(col: UInt16(col), row: UInt16(row), side: side)
    }

    /// 单元格左上角在视图内的位置（pt，原点在左上）。
    public func cellOrigin(col: Int, row: Int) -> (x: Double, y: Double) {
        (originX + Double(col) * cellWidth, originY + Double(row) * cellHeight)
    }
}

/// 终端单元格宽度的粗略估计，仅用于组字文本的覆盖层宽度与候选框偏移。
///
/// 与 core 的宽字符判定（Unicode 东亚宽度）保持"CJK 占 2 列"这一主要约定；
/// 组字文本不写入终端网格，这里的偏差最多让候选框偏几格，不影响正确性。
enum CellWidth {
    static func columns(of string: some StringProtocol) -> Int {
        string.unicodeScalars.reduce(0) { $0 + columns(of: $1) }
    }

    static func columns(of scalar: Unicode.Scalar) -> Int {
        let v = scalar.value
        switch scalar.properties.generalCategory {
        case .nonspacingMark, .enclosingMark, .format: return 0
        default: break
        }
        switch v {
        case 0x1100...0x115F, 0x2E80...0x303E, 0x3041...0x33FF, 0x3400...0x4DBF, 0x4E00...0x9FFF,
             0xA000...0xA4CF, 0xAC00...0xD7A3, 0xF900...0xFAFF, 0xFE30...0xFE6F, 0xFF00...0xFF60,
             0xFFE0...0xFFE6, 0x1F300...0x1F64F, 0x1F900...0x1F9FF, 0x20000...0x3FFFD:
            return 2
        default:
            return 1
        }
    }
}
