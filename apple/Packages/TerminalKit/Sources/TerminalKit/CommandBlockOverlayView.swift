import AppKit
import KansoBridge

@MainActor
final class CommandBlockOverlayView: NSView {
    override var isFlipped: Bool { true }
    var onCopy: ((UInt64, FfiBlockText) -> Void)?
    private(set) var hoveredBlock: FfiCommandBlock?
    private(set) var buttons: [NSButton] = []
    private var snapshot: FrameSnapshot?
    private var geometry: TerminalHitGeometry?
    private var palette = TerminalPalette.kansoDark
    private var blocks: [FfiCommandBlock] = []

    override init(frame: NSRect) {
        super.init(frame: frame)
        for (index, item) in [
            ("terminal", String(localized: "Copy Command")),
            ("text.alignleft", String(localized: "Copy Output")),
            ("doc.on.doc", String(localized: "Copy All"))
        ].enumerated() {
            let button = NSButton(image: NSImage(systemSymbolName: item.0, accessibilityDescription: item.1)!, target: self, action: #selector(copyBlock(_:)))
            button.tag = index
            button.toolTip = item.1
            button.setAccessibilityLabel(item.1)
            button.bezelStyle = .accessoryBarAction
            button.controlSize = .mini
            button.isHidden = true
            buttons.append(button)
            addSubview(button)
        }
    }

    required init?(coder: NSCoder) { fatalError("CommandBlockOverlayView 只支持代码创建") }

    static func showsHover(flags: FrameFlags, selecting: Bool) -> Bool {
        !flags.contains(.altScreen) && !flags.contains(.mouseReporting) && !selecting
    }

    static func statusFits(_ frame: FrameSnapshot, row: Int, fromColumn: Int) -> Bool {
        guard row >= 0, row < frame.rows, fromColumn >= 0, fromColumn < frame.cols else { return false }
        return (fromColumn..<frame.cols).allSatisfy { column in
            let cell = frame.cell(row: row, col: column)
            return (cell.ch == 0 || cell.ch == 32) && !cell.flags.contains(.wideSpacer)
        }
    }

    func update(snapshot: FrameSnapshot, geometry: TerminalHitGeometry, palette: TerminalPalette,
                hoveredID: UInt64?, selecting: Bool) {
        self.snapshot = snapshot
        self.geometry = geometry
        self.palette = palette
        blocks = snapshot.flags.contains(.altScreen) ? [] : snapshot.commandBlocks
        hoveredBlock = Self.showsHover(flags: snapshot.flags, selecting: selecting)
            ? blocks.first { $0.id == hoveredID } : nil
        for button in buttons { button.isHidden = true }
        if let block = hoveredBlock {
            let available = [block.canCopyCommand, block.canCopyOutput, true]
            let row = max(0, Int(clamping: block.startLine) - Int(clamping: snapshot.lineMeta(row: 0).absLine))
            var right = bounds.width - 4
            for button in buttons.reversed() where available[button.tag] {
                right -= 26
                button.frame = NSRect(x: right, y: geometry.originY + Double(row) * geometry.cellHeight + 2, width: 24, height: 18)
                button.isHidden = false
            }
        }
        needsDisplay = true
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit is NSButton ? hit : nil
    }

    @objc private func copyBlock(_ sender: NSButton) {
        guard let block = hoveredBlock else { return }
        let parts: [FfiBlockText] = [.command, .output, .all]
        onCopy?(block.id, parts[sender.tag])
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let snapshot, let geometry, snapshot.rows > 0 else { return }
        let first = snapshot.lineMeta(row: 0).absLine
        if let block = hoveredBlock {
            let start = Int(max(block.startLine, first) - first)
            let end = Int(min(block.endLine, first + UInt64(snapshot.rows - 1)) - first)
            palette.foreground.nsColor(alpha: 0.03).setFill()
            NSRect(x: geometry.originX, y: geometry.originY + Double(start) * geometry.cellHeight,
                   width: bounds.width - geometry.originX, height: Double(end - start + 1) * geometry.cellHeight).fill()
        }
        let font = NSFont.monospacedSystemFont(ofSize: 10, weight: .regular)
        // 悬停的块右上角显示复制按钮，它的状态标签暂时隐藏，避免和按钮重叠。
        for block in blocks where block.id != hoveredBlock?.id {
            guard let commandLine = block.commandLine, commandLine >= first,
                  commandLine - first < UInt64(snapshot.rows) else { continue }
            let row = Int(commandLine - first)
            let text: String
            if block.running {
                text = "◌"
            } else if let duration = block.duration {
                let elapsed = renderMessage(message: duration, languages: Bundle.main.preferredLocalizations)
                if let code = block.exitCode {
                    text = code == 0 ? String(format: String(localized: "✓ %@"), elapsed)
                        : String(format: String(localized: "✗ %d · %@"), code, elapsed)
                } else { text = elapsed }
            } else { continue }
            let width = ceil((text as NSString).size(withAttributes: [.font: font]).width) + 10
            let left = bounds.width - width - 5
            let column = Int(floor((left - geometry.originX) / geometry.cellWidth))
            guard Self.statusFits(snapshot, row: row, fromColumn: column) else { continue }
            let rect = NSRect(x: left, y: geometry.originY + Double(row) * geometry.cellHeight,
                              width: width, height: geometry.cellHeight)
            palette.background.nsColor(alpha: 0.85).setFill()
            NSBezierPath(roundedRect: rect, xRadius: 3, yRadius: 3).fill()
            (text as NSString).draw(at: NSPoint(x: rect.minX + 5, y: rect.midY - font.boundingRectForFont.height / 2),
                withAttributes: [.font: font, .foregroundColor: palette.foreground.nsColor(alpha: 0.65)])
        }
    }
}

extension TerminalView {
    func updateCommandBlocks() {
        guard let frame = currentFrame, let geometry = hitGeometry() else { return }
        if let location = commandHoverLocation, let point = geometry.viewportPoint(x: location.x, y: location.y),
           Int(point.row) < frame.rows {
            let line = frame.lineMeta(row: Int(point.row)).absLine
            hoveredCommandBlock = frame.commandBlocks.first { $0.startLine <= line && line <= $0.endLine }?.id
        } else { hoveredCommandBlock = nil }
        commandOverlay.update(snapshot: frame, geometry: geometry, palette: currentPalette,
                              hoveredID: hoveredCommandBlock, selecting: activeButtons[.left] != nil)
    }

    func updateCommandHover(_ event: NSEvent) {
        let location = convert(event.locationInWindow, from: nil)
        commandHoverLocation = NSPoint(x: location.x, y: bounds.height - location.y)
        updateCommandBlocks()
    }

    public override func mouseExited(with event: NSEvent) {
        commandHoverLocation = nil
        hoveredCommandBlock = nil
        updateCommandBlocks()
    }

    func addCommandBlockMenuItems(to menu: NSMenu, event: NSEvent) {
        updateCommandHover(event)
        menuCommandBlock = hoveredCommandBlock
        guard let block = currentFrame?.commandBlocks.first(where: { $0.id == menuCommandBlock }), block.canCopyOutput else { return }
        menu.addItem(.separator())
        let command = menu.addItem(withTitle: String(localized: "Copy Command"), action: #selector(copyBlockCommand(_:)), keyEquivalent: "")
        command.target = self
        command.isEnabled = block.canCopyCommand
        let output = menu.addItem(withTitle: String(localized: "Copy Output"), action: #selector(copyBlockOutput(_:)), keyEquivalent: "")
        output.target = self
    }

    @objc func copyBlockCommand(_ sender: Any?) {
        if let id = menuCommandBlock { copyCommandBlock(id, part: .command) }
    }

    @objc func copyBlockOutput(_ sender: Any?) {
        if let id = menuCommandBlock { copyCommandBlock(id, part: .output) }
    }

    func copyCommandBlock(_ id: UInt64, part: FfiBlockText) {
        guard let text = editActions?.commandBlockText(id: id, part: part) else { return }
        if writeCopiedText(text) { onCopied?() }
        else { presentEditFailure(String(localized: "Couldn’t copy to the clipboard.")) }
    }

    @objc public func previousCommandBlock(_ sender: Any?) { editActions?.jumpCommandBlock(forward: false) }
    @objc public func nextCommandBlock(_ sender: Any?) { editActions?.jumpCommandBlock(forward: true) }
}
