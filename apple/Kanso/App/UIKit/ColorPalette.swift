import AppKit
import KansoBridge

/// 色板：两行、每行 8 个圆点，选中的圆点外加一圈描边（visual-style §3.3）。
///
/// 颜色是固定 RGB（会话颜色存进 core 的就是 RGB，不随外观变化）；描边用 `text.primary` 而不是纯白：
/// 深色外观下它就是近白，浅色外观下纯白描边在浅卡片上看不见。
/// 再次点击已选中的圆点取消选择（`nil` = 不指定颜色，侧栏回退到按名称哈希取色）。
@MainActor
final class ColorPaletteView: NSView {
    static let dotSize: CGFloat = 22
    static let pitch: CGFloat = 28
    static let columns = 8

    /// 用户点击后回调；取消选择时为 nil。程序设置 `selected` 不回调。
    var onSelect: ((FfiRgb?) -> Void)?

    var selected: FfiRgb? {
        didSet { needsDisplay = true }
    }

    private(set) var colors: [FfiRgb] = []

    override var isFlipped: Bool { true }

    func setColors(_ colors: [FfiRgb]) {
        self.colors = colors
        invalidateIntrinsicContentSize()
        needsDisplay = true
    }

    override var intrinsicContentSize: NSSize {
        let rows = max(1, (colors.count + Self.columns - 1) / Self.columns)
        return NSSize(width: NSView.noIntrinsicMetric, height: CGFloat(rows) * Self.pitch)
    }

    private func frame(ofDotAt index: Int) -> NSRect {
        let row = index / Self.columns, col = index % Self.columns
        let inset = (Self.pitch - Self.dotSize) / 2
        return NSRect(x: CGFloat(col) * Self.pitch + inset, y: CGFloat(row) * Self.pitch + inset,
                      width: Self.dotSize, height: Self.dotSize)
    }

    /// 点击位置对应的色点下标；internal 供测试。
    func index(at point: NSPoint) -> Int? {
        guard point.x >= 0, point.y >= 0 else { return nil }
        let col = Int(point.x / Self.pitch), row = Int(point.y / Self.pitch)
        guard col < Self.columns else { return nil }
        let index = row * Self.columns + col
        return colors.indices.contains(index) ? index : nil
    }

    /// 选中某个色点（或取消）。internal 供测试直接驱动。
    func choose(index: Int) {
        guard colors.indices.contains(index) else { return }
        selected = selected == colors[index] ? nil : colors[index]
        onSelect?(selected)
    }

    override func mouseDown(with event: NSEvent) {
        if let index = index(at: convert(event.locationInWindow, from: nil)) { choose(index: index) }
    }

    override func draw(_ dirtyRect: NSRect) {
        // 动态色在 draw 里按当前绘制外观解析，外观切换时系统会重绘。
        let ring = NSColor.token(DesignTokens.Color.textPrimary)
        for (index, rgb) in colors.enumerated() {
            let dot = frame(ofDotAt: index)
            NSColor(srgbRed: CGFloat(rgb.r) / 255, green: CGFloat(rgb.g) / 255, blue: CGFloat(rgb.b) / 255, alpha: 1).setFill()
            NSBezierPath(ovalIn: dot).fill()
            if rgb == selected {
                ring.setStroke()
                let path = NSBezierPath(ovalIn: dot.insetBy(dx: -2, dy: -2))
                path.lineWidth = 1.5
                path.stroke()
            }
        }
    }
}
