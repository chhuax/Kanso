import AppKit

/// 组字文本（marked text）的覆盖层，画在光标位置，不写入终端。
///
/// **为什么用子视图而不是在 Metal 渲染器里加一层**：组字文本是输入法的瞬时 UI 状态，不属于 core 的帧
/// （帧只描述终端内容），塞进渲染器会让 `render(frame:)` 多出一个与帧无关的输入，还会影响截图测试的基线；
/// 而 AppKit 文字绘制自带输入法需要的字体回退与下划线，覆盖层只有一行，几乎没有成本。
/// 子视图作为 `CAMetalLayer` 的子图层合成，不会被 Metal 的每帧重绘清掉。
///
/// 覆盖层不参与命中测试：鼠标事件必须仍然落到 `TerminalView`。
@MainActor
final class MarkedTextOverlayView: NSView {
    var text = ""
    /// 选中段（输入法正在转换的那一段），UTF-16 范围；以粗下划线显示，其余为细下划线。
    var selection = NSRange(location: 0, length: 0)
    var font: NSFont = .monospacedSystemFont(ofSize: 13, weight: .regular)
    /// 当前外观下终端实际使用的配色（用户可为浅色 / 深色分别选择，所以不能再按外观硬取内置配色）。
    /// 由 `TerminalView` 在配色或外观变化时设置。
    var palette: TerminalPalette = .kansoLight

    override var isFlipped: Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        // 不透明底色：必须盖住下面终端里原有的字符，否则组字文字与旧内容重叠无法辨认。
        palette.background.nsColor().setFill()
        bounds.fill()

        let attributed = NSMutableAttributedString(string: text, attributes: [
            .font: font,
            .foregroundColor: palette.foreground.nsColor(),
            .underlineStyle: NSUnderlineStyle.single.rawValue,
            .underlineColor: palette.foreground.nsColor(),
        ])
        let range = selection.location == NSNotFound
            ? NSRange(location: 0, length: 0) : NSIntersectionRange(selection, NSRange(location: 0, length: attributed.length))
        if range.length > 0 {
            attributed.addAttribute(
                .underlineStyle, value: NSUnderlineStyle.thick.rawValue, range: range
            )
        }
        // 文字在单元格高度内垂直居中（overlay 是 flipped 坐标，y 向下）。
        let lineHeight = font.ascender - font.descender
        attributed.draw(at: NSPoint(x: 0, y: max(0, (bounds.height - lineHeight) / 2)))
    }
}
