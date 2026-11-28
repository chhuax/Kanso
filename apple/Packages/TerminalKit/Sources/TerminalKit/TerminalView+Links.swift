import AppKit
import KansoBridge

extension TerminalView {
    /// 只认单独按住 ⌘（允许同时按 Shift，避免把 ⌘+Control/Option 当成链接点击）。
    func isLinkModifier(_ flags: NSEvent.ModifierFlags) -> Bool {
        flags.contains(.command) && !flags.contains(.control) && !flags.contains(.option)
    }

    /// 严格把视图坐标（AppKit 左下原点）换算为网格行列；落在时间戳栏、留白或网格之外返回 nil。
    func strictCell(at location: NSPoint) -> (row: Int, col: Int)? {
        guard let frame = currentFrame, let geometry = hitGeometry(), bounds.contains(location) else { return nil }
        let x = Double(location.x)
        let top = Double(bounds.height - location.y)
        guard x >= geometry.originX, top >= geometry.originY,
              x < geometry.originX + Double(frame.cols) * geometry.cellWidth,
              top < geometry.originY + Double(frame.rows) * geometry.cellHeight,
              let point = geometry.viewportPoint(x: x, y: top) else { return nil }
        return (Int(point.row), Int(point.col))
    }

    func link(at location: NSPoint) -> TerminalLinkHit? {
        guard let frame = currentFrame, let cell = strictCell(at: location) else { return nil }
        let resolver = hyperlinkResolver ?? { _ in nil }
        return TerminalLinks.link(in: frame, row: cell.row, col: cell.col, resolveHyperlink: resolver)
    }

    func link(at event: NSEvent) -> TerminalLinkHit? {
        link(at: convert(event.locationInWindow, from: nil))
    }

    func updateHoveredLink(location: NSPoint?, flags: NSEvent.ModifierFlags) {
        commandKeyDown = isLinkModifier(flags)
        hoveredLocation = location
        if commandKeyDown, let location {
            hoveredLink = link(at: location)
        } else {
            hoveredLink = nil
        }
    }

    func refreshHoveredLink() {
        guard commandKeyDown else {
            hoveredLink = nil
            return
        }
        let location = currentHoveredLocation()
        hoveredLocation = location
        hoveredLink = location.flatMap { link(at: $0) }
    }

    func clearLinkHover() {
        commandKeyDown = false
        hoveredLocation = nil
        hoveredLink = nil
    }

    func currentHoveredLocation() -> NSPoint? {
        if let hoveredLocation { return hoveredLocation }
        guard let window, window.isKeyWindow else { return nil }
        let location = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        return bounds.contains(location) ? location : nil
    }

    func linkCursorRects() -> [NSRect] {
        guard let hoveredLink, let geometry = hitGeometry() else { return [] }
        return hoveredLink.segments.compactMap { segment in
            let count = segment.endCol - segment.startCol
            guard count > 0 else { return nil }
            let origin = geometry.cellOrigin(col: segment.startCol, row: segment.row)
            return NSRect(
                x: origin.x, y: bounds.height - origin.y - geometry.cellHeight,
                width: Double(count) * geometry.cellWidth, height: geometry.cellHeight
            )
        }
    }

    func openLink(_ rawURL: String) {
        if let url = TerminalLinks.openableURL(from: rawURL) {
            _ = openURL(url)
            return
        }
        let message = String(localized: "This kind of link can’t be opened.")
        if let onOpenLinkRejected {
            onOpenLinkRejected(message)
        } else {
            presentEditFailure(message)
        }
    }
}
