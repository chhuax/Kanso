import AppKit
import UniformTypeIdentifiers
import KansoBridge

/// 剪贴板内容的归类结果，与 `crates/kanso-types/src/input.rs` 的 `PasteContent` 一一对应。
///
/// 视图只负责读剪贴板并归类；转义规则、AI CLI 会话的特殊处理、bracketed paste 都由 core 决定
/// （terminal.md §8.3）。
public enum TerminalPasteContent: Equatable, Sendable {
    /// 文本。
    case text(String)
    /// 文件（例如在 Finder 里复制的文件）的路径。
    case files([String])
    /// 剪贴板里只有图片，没有文字也没有文件。
    case imageOnly
}

/// 终端的编辑动作出口：复制、粘贴、全选。由持有 core 的上层实现（目前是 `CoreInputSink`）。
///
/// 与 `TerminalInputSink` 分开，是因为这几个动作来自菜单 / 快捷键（responder chain 的
/// `copy:` / `paste:` / `selectAll:`），而不是按键编码；以后快捷键可重新绑定（ui-shell.md §7）时，
/// 绑定落到的也是这几个动作。
@MainActor
public protocol TerminalEditActions: AnyObject {
    /// 当前是否有选区；决定"复制"菜单项是否可用。
    var hasSelection: Bool { get }
    /// 选区文本；没有选区时为 nil。
    func selectionText() -> String?
    /// 把剪贴板内容发给会话。
    func paste(_ content: TerminalPasteContent)
    /// 全选终端缓冲区（回滚 + 屏幕）。
    func selectAll()
    /// 选中视口里 `point` 处的单词（右键弹菜单前调用，terminal.md §8.1）。
    func selectWord(at point: ViewportPointValue)
    /// 清除文本选区（右键"复制或粘贴"复制后调用，terminal.md §8.1）。
    func clearSelection()
    func commandBlockText(id: UInt64, part: FfiBlockText) -> String?
    func jumpCommandBlock(forward: Bool) -> UInt64?
    func toggleCommandBlockFold(id: UInt64) -> Bool
}

extension TerminalEditActions {
    public func selectWord(at point: ViewportPointValue) {}
    public func clearSelection() {}
    public func commandBlockText(id: UInt64, part: FfiBlockText) -> String? { nil }
    public func jumpCommandBlock(forward: Bool) -> UInt64? { nil }
    public func toggleCommandBlockFold(id: UInt64) -> Bool { false }
}

extension TerminalView: NSMenuItemValidation {
    /// 编辑菜单的"复制"（⌘C）。菜单项用标准 selector，first responder 是终端视图时自动路由到这里。
    @objc public func copy(_ sender: Any?) {
        copySelectionIfPresent()
    }

    /// 复制当前文本选区（不回落命令块）；供 `copy(_:)` 与"选中即复制"共用。
    func copySelectionIfPresent() {
        guard let text = editActions?.selectionText(), !text.isEmpty else { return }
        // 写剪贴板失败必须提示（terminal.md §8.2）：否则用户看到选区还在，会以为已经复制成功。
        if !writeCopiedText(text) {
            presentEditFailure(String(localized: "Couldn’t copy to the clipboard."))
        } else {
            onCopied?()
        }
    }

    /// 编辑菜单的"粘贴"（⌘V）。按 terminal.md §8.3 的顺序归类：文件 → 只有图片 → 文本。
    @objc public func paste(_ sender: Any?) {
        guard let actions = editActions else { return }
        let pasteboard = self.pasteboard
        if let content = Self.classify(pasteboard) {
            actions.paste(content)
        } else if pasteboard.types?.isEmpty == false {
            // 剪贴板有内容但读不出可用的类型：不能静默失败（terminal.md §8.3）。
            presentEditFailure(String(localized: "Couldn’t read the clipboard."))
        }
    }

    /// 编辑菜单的"全选"（⌘A）：选中整个终端缓冲区，而不是 NSResponder 的默认行为。
    @objc public override func selectAll(_ sender: Any?) {
        editActions?.selectAll()
    }

    public func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(copy(_:)):
            return editActions?.hasSelection == true
        case #selector(paste(_:)), #selector(selectAll(_:)):
            return editActions != nil
        case #selector(copyBlockCommand(_:)):
            return currentFrame?.commandBlocks.first { $0.id == menuCommandBlock }?.canCopyCommand ?? false
        case #selector(copyBlockOutput(_:)):
            return currentFrame?.commandBlocks.first { $0.id == menuCommandBlock }?.canCopyOutput ?? false
        default:
            return true
        }
    }

    /// 按 terminal.md §8.3 的优先级归类剪贴板内容。抽成静态方法便于测试。
    static func classify(_ pasteboard: NSPasteboard) -> TerminalPasteContent? {
        // 1. 文件：Finder 复制的文件同时带有文件名文本，必须先判断文件，否则会粘贴成裸文件名。
        if let urls = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !urls.isEmpty {
            return .files(urls.map(\.path))
        }
        // 3. 文本（先于"只有图片"判断：同时有文字和图片时按文字粘贴）。
        if let text = pasteboard.string(forType: .string) {
            return .text(text)
        }
        // 2. 只有图片：交给 core 决定（AI CLI 会话里发送工具自己的图片粘贴键）。
        let imageTypes: [NSPasteboard.PasteboardType] = [.png, .tiff, NSPasteboard.PasteboardType(UTType.jpeg.identifier)]
        if let types = pasteboard.types, types.contains(where: imageTypes.contains) {
            return .imageOnly
        }
        return nil
    }

    /// 文案取自 App 的 `Localizable.xcstrings`（TerminalKit 包本身不带资源）。
    func presentEditFailure(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = message
        if let window {
            alert.beginSheetModal(for: window)
        } else {
            alert.runModal()
        }
    }
}
