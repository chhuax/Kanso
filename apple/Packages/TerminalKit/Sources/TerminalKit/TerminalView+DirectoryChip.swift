import AppKit
import KansoBridge

extension TerminalView {
    public override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .iBeam)
        guard onDirectoryChipClick != nil, isDirectoryChangeAvailable,
              let frame = currentFrame, let block = frame.commandBlocks.last, let geometry = hitGeometry() else { return }
        for row in 0..<frame.rows {
            var col = 0
            while col < frame.cols {
                if let columns = Self.directoryChipColumns(row: row, col: col, frame: frame, block: block) {
                    addCursorRect(directoryChipRect(columns: columns, row: row, geometry: geometry), cursor: .pointingHand)
                    col = columns.upperBound
                } else {
                    col += 1
                }
            }
        }
    }

    public var isDirectoryChangeAvailable: Bool {
        guard let frame = currentFrame, !frame.flags.contains(.altScreen),
              !frame.flags.contains(.mouseReporting), let block = frame.commandBlocks.last,
              block.latest, !block.running, block.commandLine == nil else { return false }
        return true
    }

    static func directoryChipColumns(row: Int, col: Int, frame: FrameSnapshot, block: FfiCommandBlock) -> Range<Int>? {
        let line = frame.lineMeta(row: row).absLine
        guard block.latest, !block.running, block.commandLine == nil, block.startLine <= line, line <= block.endLine,
              frame.cell(row: row, col: col).flags.contains(.chip) else { return nil }
        var start = col
        var end = col + 1
        while start > 0, frame.cell(row: row, col: start - 1).flags.contains(.chip) { start -= 1 }
        while end < frame.cols, frame.cell(row: row, col: end).flags.contains(.chip) { end += 1 }
        // 空白格在帧里是 0（从未写过或被清除），也可能是 32（shim 输出的空格），两种都要跳过。
        let first = (start..<end).first { ![0, 32].contains(frame.cell(row: row, col: $0).ch) }
        guard let first, frame.cell(row: row, col: first).ch == 0xf07b else { return nil }
        return start..<end
    }

    func directoryChip(at event: NSEvent) -> NSRect? {
        guard onDirectoryChipClick != nil, isDirectoryChangeAvailable,
              let frame = currentFrame, let block = frame.commandBlocks.last, let geometry = hitGeometry() else { return nil }
        let location = convert(event.locationInWindow, from: nil)
        let top = bounds.height - location.y
        guard bounds.contains(location), location.x >= geometry.originX, top >= geometry.originY,
              location.x < geometry.originX + Double(frame.cols) * geometry.cellWidth,
              top < geometry.originY + Double(frame.rows) * geometry.cellHeight,
              let point = geometry.viewportPoint(x: location.x, y: top),
              let columns = Self.directoryChipColumns(row: Int(point.row), col: Int(point.col), frame: frame, block: block) else { return nil }
        return directoryChipRect(columns: columns, row: Int(point.row), geometry: geometry)
    }

    private func directoryChipRect(columns: Range<Int>, row: Int, geometry: TerminalHitGeometry) -> NSRect {
        let origin = geometry.cellOrigin(col: columns.lowerBound, row: row)
        return NSRect(x: origin.x, y: bounds.height - origin.y - geometry.cellHeight,
                      width: Double(columns.count) * geometry.cellWidth, height: geometry.cellHeight)
    }
}
