import AppKit

/// 自绘控件的底座：实色背景 + 细边框 + 圆角，颜色全部来自 design token（visual-style §1）。
///
/// 颜色以 token 名保存、在 `updateLayer` 里按视图自己的外观解析：`CGColor` 不会随外观自动更新，
/// 外观切换时系统会重新调用 `updateLayer`；`viewDidChangeEffectiveAppearance` 里再标记一次，
/// 是为了在没有进入绘制周期的离屏视图（测试）里也能保证重新解析。
/// 子类要改边框 / 背景，改 token 属性即可，不要直接写 layer（否则外观切换时会被旧值覆盖）。
///
/// 坐标系是 flipped：自绘布局按"从上到下"写更直观。
@MainActor
class StyledBoxView: NSView {
    var fillToken: String? { didSet { needsDisplay = true } }
    var borderToken: String? { didSet { needsDisplay = true } }
    var cornerRadius: CGFloat = DesignTokens.Radius.md { didSet { needsDisplay = true } }
    var borderWidth: CGFloat = 1 { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }
    override var wantsUpdateLayer: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("StyledBoxView 只支持代码创建") }

    override func updateLayer() {
        guard let layer else { return }
        layer.cornerRadius = cornerRadius
        layer.borderWidth = borderToken == nil ? 0 : borderWidth
        layer.backgroundColor = fillToken.map { resolvedCGColor(.token($0)) }
        layer.borderColor = borderToken.map { resolvedCGColor(.token($0)) }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}

extension NSImage {
    /// 取 SF Symbol，固定使用英文区域的通用图形；名字不存在时返回空图。
    static func symbol(_ name: String, description: String? = nil) -> NSImage {
        NSImage(systemSymbolName: name, accessibilityDescription: description)?.withLocale(Locale(identifier: "en")) ?? NSImage()
    }
}

/// 不拦截鼠标事件的透明容器，用于把图标 / 文字叠在可点击的自绘视图上。
@MainActor
final class PassthroughLabel: NSTextField {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
