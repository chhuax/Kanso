import AppKit
import KansoBridge

/// 命令块的交互：右键菜单里的"复制命令 / 复制输出"与 ⌘↑ / ⌘↓ 跳转。
/// 产品决定（2026-11-16）：不显示块右端的耗时标签、不显示悬停复制按钮与悬停高亮，
/// 失败块的左侧红条由渲染器绘制，不在这里。
extension TerminalView {
    /// 鼠标事件所在行是否落在某个可折叠命令块的左侧折叠按钮区（`[-]` / `[+]`）。
    func foldableBlock(at event: NSEvent) -> FfiCommandBlock? {
        guard let frame = currentFrame, !frame.flags.contains(.altScreen),
              let renderer, let geometry = hitGeometry() else { return nil }
        let location = convert(event.locationInWindow, from: nil)
        let top = bounds.height - location.y
        let scale = currentScaleForInput
        let gutterRight = Double(renderer.layout(scale: scale).gutterWidth) / scale
        guard bounds.contains(location),
              location.x >= gutterRight, location.x < geometry.originX,
              top >= geometry.originY,
              top < geometry.originY + Double(frame.rows) * geometry.cellHeight else { return nil }
        let row = Int((top - geometry.originY) / geometry.cellHeight)
        guard row >= 0, row < frame.rows else { return nil }
        let line = frame.lineMeta(row: row).absLine
        return frame.commandBlocks.first {
            $0.foldable && ($0.commandLine ?? $0.startLine) == line
        }
    }

    /// 鼠标事件所在行属于哪个命令块；不在任何块内（或全屏程序期间）为 nil。
    func commandBlock(at event: NSEvent) -> FfiCommandBlock? {
        guard let frame = currentFrame, !frame.flags.contains(.altScreen), let geometry = hitGeometry() else { return nil }
        let location = convert(event.locationInWindow, from: nil)
        let top = bounds.height - location.y
        guard bounds.contains(location), top >= geometry.originY,
              top < geometry.originY + Double(frame.rows) * geometry.cellHeight,
              let point = geometry.viewportPoint(x: location.x, y: top),
              Int(point.row) < frame.rows else { return nil }
        let line = frame.lineMeta(row: Int(point.row)).absLine
        return frame.commandBlocks.first { $0.startLine <= line && line <= $0.endLine }
    }

    /// 右键菜单里命令块动作的插入锚点：存在时插在该分隔项之前，保持末尾项（如关闭会话）贴底。
    public static let commandBlockMenuAnchor = NSUserInterfaceItemIdentifier("kanso.context.commandBlocksAnchor")

    func addCommandBlockMenuItems(to menu: NSMenu, event: NSEvent) {
        let block = commandBlock(at: event)
        menuCommandBlock = block?.id
        guard let block, block.canCopyOutput else { return }
        let command = NSMenuItem(title: String(localized: "Copy Command"), action: #selector(copyBlockCommand(_:)), keyEquivalent: "")
        command.target = self
        command.image = NSImage(systemSymbolName: "terminal", accessibilityDescription: nil)
        command.isEnabled = block.canCopyCommand
        let output = NSMenuItem(title: String(localized: "Copy Output"), action: #selector(copyBlockOutput(_:)), keyEquivalent: "")
        output.target = self
        output.image = NSImage(systemSymbolName: "doc.text", accessibilityDescription: nil)
        let items: [NSMenuItem] = [.separator(), command, output]
        if let index = menu.items.firstIndex(where: { $0.identifier == Self.commandBlockMenuAnchor }) {
            for (offset, item) in items.enumerated() { menu.insertItem(item, at: index + offset) }
        } else {
            for item in items { menu.addItem(item) }
        }
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

    @objc public func previousCommandBlock(_ sender: Any?) {
        _ = editActions?.jumpCommandBlock(forward: false)
    }
    @objc public func nextCommandBlock(_ sender: Any?) {
        _ = editActions?.jumpCommandBlock(forward: true)
    }
}
