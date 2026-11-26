import AppKit
import KansoBridge

/// 8 位 sRGB 颜色。用值类型而不是 `NSColor`，让调色板可以是 `Sendable` 的静态常量。
public struct RGB8: Equatable, Hashable, Sendable {
    public let r: UInt8
    public let g: UInt8
    public let b: UInt8

    public init(_ r: UInt8, _ g: UInt8, _ b: UInt8) {
        self.r = r
        self.g = g
        self.b = b
    }

    /// 由 `0xRRGGBB` 构造。
    public init(hex: UInt32) {
        self.init(UInt8((hex >> 16) & 0xFF), UInt8((hex >> 8) & 0xFF), UInt8(hex & 0xFF))
    }

    /// 打包成 `0x00RRGGBB`，用作缓存键与快速相等比较。
    var packed: UInt32 { UInt32(r) << 16 | UInt32(g) << 8 | UInt32(b) }

    public func nsColor(alpha: CGFloat = 1) -> NSColor {
        NSColor(
            srgbRed: CGFloat(r) / 255,
            green: CGFloat(g) / 255,
            blue: CGFloat(b) / 255,
            alpha: alpha
        )
    }

    /// 在 sRGB 编码值上向 `other` 线性插值（`t` 为 0 返回自身，1 返回 `other`）。
    ///
    /// 不做线性光转换：终端文字、选区高亮的混合都在 sRGB 编码空间进行（见 TerminalRenderer 的颜色空间说明），
    /// 这样 CPU 预混合的结果与着色器里 alpha 混合的结果一致。
    func mixed(with other: RGB8, _ t: Float) -> RGB8 {
        func lerp(_ a: UInt8, _ b: UInt8) -> UInt8 {
            UInt8((Float(a) + (Float(b) - Float(a)) * t).rounded())
        }
        return RGB8(lerp(r, other.r), lerp(g, other.g), lerp(b, other.b))
    }
}

/// 带 alpha 的 8 位 sRGB 颜色（非预乘），用于半透明的界面 token（分隔线、光标行高亮）。
public struct RGBA8: Equatable, Sendable {
    public let r: UInt8
    public let g: UInt8
    public let b: UInt8
    public let a: UInt8

    public init(r: UInt8, g: UInt8, b: UInt8, a: UInt8) {
        self.r = r
        self.g = g
        self.b = b
        self.a = a
    }
}

/// 终端配色：渲染器把帧里的"默认 / 索引 / 语义类别"映射成像素色时使用。
///
/// 两套内置配色（`kansoLight` / `kansoDark`）由 `cargo xtask gen-tokens` 从
/// `design/terminal-themes/*.json` 与 `design/tokens.json` 生成到 `TerminalThemes.swift`，
/// 这里不手抄任何数值。
public struct TerminalPalette: Sendable {
    public let foreground: RGB8
    public let background: RGB8
    public let cursor: RGB8
    /// 块状光标覆盖下字符的颜色。
    public let cursorText: RGB8
    public let selectionBackground: RGB8
    /// 16 色 ANSI（0–7 普通，8–15 高亮）。
    public let ansi: [RGB8]
    /// 13 个语义着色类别色，下标即帧里的类别号。
    public let semantic: [RGB8]
    /// token `terminal.gutterText`：时间戳与行号栏的文字色。
    public let gutterText: RGBA8
    /// token `terminal.gutterCursorLine`：栏内光标所在行的高亮底色（半透明）。
    public let gutterCursorLine: RGBA8
    /// token `terminal.commandSeparator`：命令分隔线颜色（半透明）。
    public let commandSeparator: RGBA8
    /// token `terminal.promptBackground`：命令提示符所在行的整行浅底色（半透明）。
    public let promptBackground: RGBA8
    public let chipBackground: RGBA8
    public let chipBorder: RGBA8
    /// token `terminal.chipIcon`：标签内置图标（目录、Node、Git 分支）的颜色。
    public let chipIcon: RGBA8

    /// 256 色调色板：0–15 取自主题，16–231 为 6×6×6 色立方，232–255 为 24 级灰阶（xterm 标准公式）。
    private let table: [RGB8]

    public init(
        foreground: RGB8, background: RGB8, cursor: RGB8, cursorText: RGB8,
        selectionBackground: RGB8, ansi: [RGB8], semantic: [RGB8],
        gutterText: RGBA8, gutterCursorLine: RGBA8, commandSeparator: RGBA8, promptBackground: RGBA8,
        chipBackground: RGBA8, chipBorder: RGBA8, chipIcon: RGBA8
    ) {
        precondition(ansi.count == 16 && semantic.count == 13, "调色板尺寸错误")
        self.foreground = foreground
        self.background = background
        self.cursor = cursor
        self.cursorText = cursorText
        self.selectionBackground = selectionBackground
        self.ansi = ansi
        self.semantic = semantic
        self.gutterText = gutterText
        self.gutterCursorLine = gutterCursorLine
        self.commandSeparator = commandSeparator
        self.promptBackground = promptBackground
        self.chipBackground = chipBackground
        self.chipBorder = chipBorder
        self.chipIcon = chipIcon

        var table = ansi
        // 色立方每个分量只有 6 级：0 与 95、135、175、215、255，不是均匀分布，不能改成线性插值。
        let cubeLevels: [UInt8] = [0, 95, 135, 175, 215, 255]
        for i in 0..<216 {
            table.append(RGB8(cubeLevels[i / 36], cubeLevels[(i / 6) % 6], cubeLevels[i % 6]))
        }
        for i in 0..<24 {
            let v = UInt8(8 + i * 10)
            table.append(RGB8(v, v, v))
        }
        self.table = table
    }

    public func indexed(_ index: UInt8) -> RGB8 { table[Int(index)] }

    /// 语义类别号超出范围时回退到前景色，避免新版 core 增加类别后旧 UI 崩溃。
    public func semanticColor(_ category: UInt8) -> RGB8 {
        Int(category) < semantic.count ? semantic[Int(category)] : foreground
    }

    /// 把帧颜色解析成像素色；`fallback` 是默认色在该字段（前景或背景）上的取值。
    public func resolve(_ color: FrameColor, fallback: RGB8) -> RGB8 {
        switch color {
        case .defaultColor: fallback
        case .indexed(let i): indexed(i)
        case .rgb(let r, let g, let b): RGB8(r, g, b)
        case .semantic(let c): semanticColor(c)
        }
    }

    /// 解析粗体前景：ANSI 0..=7 提亮为 8..=15（bold-as-bright，xterm / Xshell 惯例）。
    ///
    /// 只作用于显示解析，帧里的索引与复制文本都不变。RGB、语义色、默认色以及
    /// 已是亮色的 8..=15 原样返回。`ls` 的 `01;32`（粗体绿）靠这条规则显示成亮绿。
    public func resolveForeground(_ color: FrameColor, fallback: RGB8, bold: Bool) -> RGB8 {
        if bold, case .indexed(let i) = color, i <= 7 {
            indexed(i + 8)
        } else {
            resolve(color, fallback: fallback)
        }
    }
}

/// 终端外观：决定使用哪一套配色。与系统外观解耦，让离屏渲染可以显式指定。
public enum TerminalAppearance: Sendable {
    case light
    case dark

    public var palette: TerminalPalette {
        switch self {
        case .light: .kansoLight
        case .dark: .kansoDark
        }
    }

    /// 由 `NSAppearance` 解析；无法识别时按浅色处理。
    public init(_ appearance: NSAppearance) {
        self = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? .dark : .light
    }
}
