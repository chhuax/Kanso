import AppKit

extension NSColor {
    /// 取 design token 颜色集（`DesignTokens.Color.*`）。颜色集随外观自动变化；
    /// 找不到（资源缺失）时退回灰色，不崩溃。
    static func token(_ name: String) -> NSColor {
        NSColor(named: name) ?? .gray
    }
}

extension NSView {
    /// 按视图当前外观解析动态色。直接用 `color.cgColor` 会按全局外观解析，
    /// 窗口外观与系统不一致（强制浅色 / 深色）时给 layer 上了错误的颜色。
    func resolvedCGColor(_ color: NSColor) -> CGColor {
        var result = color.cgColor
        effectiveAppearance.performAsCurrentDrawingAppearance { result = color.cgColor }
        return result
    }
}
