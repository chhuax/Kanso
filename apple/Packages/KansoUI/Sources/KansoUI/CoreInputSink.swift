import KansoBridge
import TerminalKit

/// 把 `TerminalView` 的输入事件映射成 `KansoCore` 的输入调用。
///
/// **隐私约束（architecture §13）：** 本类不记录任何按键、文字内容。输入类调用的错误
/// （会话已关闭、纯 SFTP 会话等）一律忽略：core 的约定是会话已结束时输入被丢弃并返回 `Ok`，
/// 其余错误也没有 UI 可做的事，视图的状态由 `SessionStateChanged` 事件驱动。
@MainActor
public final class CoreInputSink: TerminalInputSink {
    private let core: KansoCore
    public let session: UInt64

    public init(core: KansoCore, session: UInt64) {
        self.core = core
        self.session = session
    }

    public func key(_ event: TerminalKeyEvent) {
        try? core.key(sessionId: session, event: Self.map(event))
    }

    public func textInput(_ text: String) {
        try? core.textInput(sessionId: session, text: text)
    }

    public func mouse(_ event: TerminalMouseEvent) {
        try? core.mouse(sessionId: session, event: Self.map(event))
    }

    /// 滚动符号约定：`lines` 沿用 `NSEvent.scrollingDeltaY`，正数 = 内容向下移动、露出更早的输出；
    /// core 的 `scroll(delta_lines)` 同样是"正数向上（看更早的内容）"。两者符号一致，直接转成 `Float`，
    /// **不要取反**。
    public func scroll(lines: Int32, at: ViewportPointValue, mods: KeyModifiers) {
        try? core.scroll(sessionId: session, deltaLines: Float(lines), at: Self.map(at), mods: Self.map(mods))
    }

    public func focus(_ focused: Bool) {
        try? core.focus(sessionId: session, focused: focused)
    }

    // MARK: 编辑动作（复制 / 粘贴 / 全选）

    // 选区与剪贴板相关的逻辑都在 core：这里只转发。错误同样忽略（会话已结束时 core 丢弃输入）。
}

extension CoreInputSink: TerminalEditActions {
    public func commandBlockText(id: UInt64, part: FfiBlockText) -> String? {
        core.commandBlockText(sessionId: session, id: id, part: part)
    }

    public func jumpCommandBlock(forward: Bool) -> UInt64? {
        try? core.jumpCommandBlock(sessionId: session, forward: forward)
    }
    public var hasSelection: Bool {
        core.hasSelection(sessionId: session)
    }

    public func selectionText() -> String? {
        core.selectionText(sessionId: session)
    }

    public func paste(_ content: TerminalPasteContent) {
        let mapped: FfiPasteContent = switch content {
        case .text(let text): .text(text: text)
        case .files(let paths): .files(paths: paths)
        case .imageOnly: .imageOnly
        }
        try? core.paste(sessionId: session, content: mapped)
    }

    public func selectAll() {
        try? core.selectAll(sessionId: session)
    }
}

extension CoreInputSink {
    // MARK: 类型映射（与 crates/kanso-types/src/input.rs 一一对应）

    static func map(_ mods: KeyModifiers) -> FfiModifiers {
        FfiModifiers(
            shift: mods.contains(.shift),
            ctrl: mods.contains(.ctrl),
            altLeft: mods.contains(.altLeft),
            altRight: mods.contains(.altRight),
            meta: mods.contains(.meta)
        )
    }

    static func map(_ key: TerminalKey) -> FfiKey {
        switch key {
        case .character(let text): .character(text: text)
        case .enter: .enter
        case .tab: .tab
        case .backspace: .backspace
        case .escape: .escape
        case .space: .space
        case .arrowUp: .arrowUp
        case .arrowDown: .arrowDown
        case .arrowLeft: .arrowLeft
        case .arrowRight: .arrowRight
        case .home: .home
        case .end: .end
        case .pageUp: .pageUp
        case .pageDown: .pageDown
        case .insert: .insert
        case .delete: .delete
        case .function(let n): .function(number: n)
        }
    }

    static func map(_ kind: KeyEventKind) -> FfiKeyEventKind {
        switch kind {
        case .press: .press
        case .repeat: .repeat
        case .release: .release
        }
    }

    static func map(_ event: TerminalKeyEvent) -> FfiKeyEvent {
        FfiKeyEvent(key: map(event.key), mods: map(event.mods), text: event.text, kind: map(event.kind))
    }

    static func map(_ side: TerminalCellSide) -> FfiCellSide {
        switch side {
        case .left: .left
        case .right: .right
        }
    }

    static func map(_ point: ViewportPointValue) -> FfiViewportPoint {
        FfiViewportPoint(col: point.col, row: point.row, side: map(point.side))
    }

    static func map(_ event: TerminalMouseEvent) -> FfiMouseEvent {
        let button: FfiMouseButton
        switch event.button {
        case .left: button = .left
        case .middle: button = .middle
        case .right: button = .right
        case .none: button = .none
        }
        let kind: FfiMouseEventKind
        switch event.kind {
        case .down: kind = .down
        case .up: kind = .up
        case .drag: kind = .drag
        case .move: kind = .move
        }
        return FfiMouseEvent(
            kind: kind, button: button, point: map(event.point), mods: map(event.mods),
            clickCount: event.clickCount
        )
    }
}
