import AppKit

/// 面包屑：点击某一段跳转；放不下时从左侧省略。点击空白处通知进入编辑。
@MainActor
final class FilerBreadcrumbView: NSView {
    var path = "" { didSet { needsDisplay = true } }
    var onNavigate: ((String) -> Void)?
    var onBeginEditing: (() -> Void)?
    private var hitRects: [(rect: NSRect, path: String)] = []

    override var isFlipped: Bool { true }
    private static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

    /// 当前可见的分段（含省略后的）。测试用。
    var visibleSegments: [(title: String, path: String)] { layoutSegments(width: bounds.width).segments }

    private func layoutSegments(width: CGFloat) -> (segments: [(title: String, path: String)], truncated: Bool) {
        var crumbs = FilerPath.breadcrumbs(path)
        let sep: CGFloat = 14
        func w(_ s: String) -> CGFloat { (s as NSString).size(withAttributes: [.font: Self.font]).width }
        var truncated = false
        func total() -> CGFloat { crumbs.reduce(0) { $0 + w($1.title) + sep } + (truncated ? w("…") + sep : 0) }
        while crumbs.count > 1, total() > width - 8 {
            crumbs.removeFirst()
            truncated = true
        }
        return (crumbs, truncated)
    }

    override func draw(_ dirtyRect: NSRect) {
        hitRects = []
        let (segments, truncated) = layoutSegments(width: bounds.width)
        var x: CGFloat = 4
        let attrs: [NSAttributedString.Key: Any] = [.font: Self.font, .foregroundColor: NSColor.token(DesignTokens.Color.textPrimary)]
        let dim: [NSAttributedString.Key: Any] = [.font: Self.font, .foregroundColor: NSColor.token(DesignTokens.Color.textTertiary)]
        let lineH = ("A" as NSString).size(withAttributes: attrs).height
        let y = (bounds.height - lineH) / 2
        func drawText(_ s: String, _ a: [NSAttributedString.Key: Any]) -> NSRect {
            let size = (s as NSString).size(withAttributes: a)
            (s as NSString).draw(at: NSPoint(x: x, y: y), withAttributes: a)
            defer { x += size.width }
            return NSRect(x: x, y: 0, width: size.width, height: bounds.height)
        }
        if truncated { _ = drawText("…", dim); x += 4; _ = drawText("/", dim); x += 4 }
        for (i, seg) in segments.enumerated() {
            let rect = drawText(seg.title, attrs)
            hitRects.append((rect, seg.path))
            // 根目录本身就是 "/"，不再追加分隔符。
            if seg.title != "/", i < segments.count - 1 || false {
                x += 4; _ = drawText("/", dim); x += 4
            } else if seg.title == "/" {
                x += 2
            }
        }
    }

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        if let hit = hitRects.first(where: { $0.rect.contains(p) }) { onNavigate?(hit.path) } else { onBeginEditing?() }
    }
}

/// 路径栏：默认是面包屑；点击空白或聚焦时变成可编辑的等宽文本（Enter 打开 / Esc 恢复）。
@MainActor
final class FilerPathBar: StyledBoxView, NSTextFieldDelegate {
    let field = NSTextField()
    let crumbs = FilerBreadcrumbView()
    var onSubmit: ((String) -> Void)?
    var onNavigate: ((String) -> Void)?
    /// Esc 时用来取回最后成功的目录文字。
    var restoredText: (() -> String)?
    private(set) var isEditing = false
    private var finishing = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        fillToken = DesignTokens.Color.bgField
        cornerRadius = DesignTokens.Radius.md
        field.isBezeled = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        field.textColor = .token(DesignTokens.Color.textPrimary)
        field.delegate = self
        field.cell?.isScrollable = true
        field.cell?.lineBreakMode = .byClipping
        field.isHidden = true
        crumbs.onNavigate = { [weak self] in self?.onNavigate?($0) }
        crumbs.onBeginEditing = { [weak self] in self?.beginEditing() }
        addSubview(crumbs)
        addSubview(field)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("FilerPathBar 只支持代码创建") }

    var text: String {
        get { isEditing ? field.stringValue : crumbs.path }
        set {
            crumbs.path = newValue
            if !isEditing { field.stringValue = newValue }
        }
    }

    override func layout() {
        super.layout()
        let frame = bounds.insetBy(dx: 6, dy: 0)
        crumbs.frame = frame
        let h: CGFloat = 16
        field.frame = NSRect(x: frame.minX, y: (bounds.height - h) / 2, width: frame.width, height: h)
    }

    func beginEditing() {
        guard !isEditing else { return }
        isEditing = true
        finishing = false
        field.stringValue = crumbs.path
        field.isHidden = false
        crumbs.isHidden = true
        borderToken = DesignTokens.Color.accent
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    private func endEditing() {
        isEditing = false
        field.isHidden = true
        crumbs.isHidden = false
        borderToken = nil
        field.stringValue = crumbs.path
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.insertNewline(_:)) {
            finishing = true
            let text = field.stringValue
            endEditing()
            window?.makeFirstResponder(nil)
            onSubmit?(text)
            return true
        }
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            finishing = true
            // 放弃输入，恢复最后成功目录，不发出目录请求。
            crumbs.path = restoredText?() ?? crumbs.path
            endEditing()
            window?.makeFirstResponder(nil)
            return true
        }
        return false
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        // 失焦等同放弃输入（只修改文字不发出请求）。
        guard isEditing, !finishing else { return }
        endEditing()
    }
}
