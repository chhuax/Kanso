import AppKit
import KansoBridge

/// 命令块的交互：右键菜单里的"复制命令 / 复制输出"与 ⌘↑ / ⌘↓ 跳转。
/// 产品决定（2026-11-16）：不显示块右端的耗时标签、不显示悬停复制按钮与悬停高亮，
/// 失败块的左侧红条由渲染器绘制，不在这里。
extension TerminalView {
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

    func addCommandBlockMenuItems(to menu: NSMenu, event: NSEvent) {
        let block = commandBlock(at: event)
        menuCommandBlock = block?.id
        guard let block, block.canCopyOutput else { return }
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

    @objc public func previousCommandBlock(_ sender: Any?) {
        if let id = editActions?.jumpCommandBlock(forward: false) { selectedCommandBlock = id }
    }
    @objc public func nextCommandBlock(_ sender: Any?) {
        if let id = editActions?.jumpCommandBlock(forward: true) { selectedCommandBlock = id }
    }
}
