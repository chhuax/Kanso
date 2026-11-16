import AppKit
import KansoBridge
import TerminalKit

extension FfiCompletionCategory {
    var localizedLabel: String {
        switch self {
        case .directory: String(localized: "Directory")
        case .file: String(localized: "File")
        case .history: String(localized: "History")
        case .kubernetes: String(localized: "Kubernetes")
        case .docker: String(localized: "Docker")
        case .git: String(localized: "Git")
        case .systemd: String(localized: "systemd")
        }
    }
}

struct CompletionModel {
    enum Key { case down, up, escape, tab, other }
    enum Action { case forward, consume, accept(FfiCompletionCandidate) }

    private(set) var generation: UInt64 = 0
    private(set) var candidates: [FfiCompletionCandidate] = []
    private(set) var selected: Int?
    private(set) var waiting = false

    mutating func invalidate() {
        generation += 1
        candidates = []
        selected = nil
        waiting = false
    }

    mutating func begin() -> UInt64 {
        invalidate()
        waiting = true
        return generation
    }

    mutating func receive(_ values: [FfiCompletionCandidate], generation request: UInt64, finished: Bool) {
        guard request == generation else { return }
        candidates = Array(values.prefix(8))
        selected = nil
        waiting = !finished
    }

    mutating func key(_ key: Key, composing: Bool = false) -> Action {
        if composing { invalidate(); return .forward }
        switch key {
        case .escape where waiting || !candidates.isEmpty:
            invalidate()
            return .consume
        case .tab:
            if candidates.indices.contains(selected ?? 0) {
                let candidate = candidates[selected ?? 0]
                invalidate()
                return .accept(candidate)
            }
            return waiting ? .consume : .forward
        case .down where !candidates.isEmpty:
            selected = min((selected ?? -1) + 1, candidates.count - 1)
            return .consume
        case .up where selected != nil:
            selected = max(0, selected! - 1)
            return .consume
        default:
            selected = nil
            return .forward
        }
    }

    @MainActor
    static func replace(_ candidate: FfiCompletionCandidate, through sink: TerminalInputSink) {
        for _ in 0..<candidate.deleteCodepoints { sink.key(TerminalKeyEvent(key: .backspace)) }
        sink.textInput(candidate.replacement)
    }
}

@MainActor
final class CompletionPopupView: StyledBoxView {
    static let rowHeight: CGFloat = 30
    var candidates: [FfiCompletionCandidate] = [] { didSet { needsDisplay = true } }
    var selected: Int? { didSet { needsDisplay = true } }

    static func size(count: Int, width: CGFloat = 460) -> NSSize {
        NSSize(width: width, height: CGFloat(count) * rowHeight + 12)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        fillToken = DesignTokens.Color.bgPopover
        borderToken = DesignTokens.Color.borderStrong
        cornerRadius = DesignTokens.Radius.xl
        setAccessibilityRole(.list)
        setAccessibilityLabel(String(localized: "Command Suggestions"))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("CompletionPopupView 只支持代码创建") }

    override var wantsUpdateLayer: Bool { false }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.token(DesignTokens.Color.bgPopover).setFill()
        let outline = NSBezierPath(roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), xRadius: 12, yRadius: 12)
        outline.fill()
        NSColor.token(DesignTokens.Color.borderStrong).setStroke()
        outline.stroke()
        for (index, candidate) in candidates.enumerated() {
            let row = NSRect(x: 6, y: 6 + CGFloat(index) * Self.rowHeight, width: bounds.width - 12, height: Self.rowHeight)
            if selected == index {
                NSColor.selectedContentBackgroundColor.withAlphaComponent(0.25).setFill()
                NSBezierPath(roundedRect: row, xRadius: 6, yRadius: 6).fill()
            }
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineBreakMode = .byTruncatingTail
            let labelWidth: CGFloat = 86
            (candidate.replacement as NSString).draw(in: NSRect(x: 16, y: row.minY + 6, width: row.width - labelWidth - 18, height: 20), withAttributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                .foregroundColor: NSColor.token(DesignTokens.Color.textPrimary), .paragraphStyle: paragraph
            ])
            paragraph.alignment = .right
            (candidate.category.localizedLabel as NSString).draw(in: NSRect(x: bounds.width - labelWidth - 16, y: row.minY + 7, width: labelWidth, height: 18), withAttributes: [
                .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.secondaryLabelColor, .paragraphStyle: paragraph
            ])
        }
    }
}
