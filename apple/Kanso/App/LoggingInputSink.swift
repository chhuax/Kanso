import Foundation
import os
import TerminalKit

/// 调试用的输入出口：把 `TerminalView` 发出的事件打到 `os_log`，再原样转发给下一个出口（`CoreInputSink`）。
///
/// 用途是人工验证 IME 与鼠标行为（docs/ime-test-checklist.md）：在真实 shell 接入（I1）之前，
/// mock core 不会回显输入，日志是观察输入法回调结果的唯一手段；接入之后也仍然有助于排查输入问题。
///
/// **隐私约束（architecture §13）：日志绝不记录按键输入。** 因此事件内容（按键、文字、坐标）
/// 只在 `#if DEBUG` 下记录；Release 构建里整个类是空实现，连参数都不会被格式化。
/// 不要把 `#if DEBUG` 改成运行时开关：运行时开关一旦被误开，用户的密码就会进日志。
@MainActor
final class LoggingInputSink: TerminalInputSink {
    /// 强引用：`TerminalView.inputSink` 是弱引用，链上的下一个出口由这里保活。
    private let next: TerminalInputSink

    init(forwardingTo next: TerminalInputSink) {
        self.next = next
    }

    #if DEBUG
    private let logger = Logger(subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "terminal-input")
    #endif

    func key(_ event: TerminalKeyEvent) {
        #if DEBUG
        logger.debug("key \(String(describing: event), privacy: .public)")
        #endif
        next.key(event)
    }

    func textInput(_ text: String) {
        #if DEBUG
        logger.debug("textInput \(text, privacy: .public)")
        #endif
        next.textInput(text)
    }

    func mouse(_ event: TerminalMouseEvent) {
        #if DEBUG
        logger.debug("mouse \(String(describing: event), privacy: .public)")
        #endif
        next.mouse(event)
    }

    func scroll(lines: Int32, at: ViewportPointValue, mods: KeyModifiers) {
        #if DEBUG
        logger.debug("scroll lines=\(lines) at=\(String(describing: at), privacy: .public)")
        #endif
        next.scroll(lines: lines, at: at, mods: mods)
    }

    func focus(_ focused: Bool) {
        #if DEBUG
        logger.debug("focus \(focused)")
        #endif
        next.focus(focused)
    }
}
