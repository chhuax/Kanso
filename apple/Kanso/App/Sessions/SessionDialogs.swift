import AppKit

/// 启动器用到的确认 / 输入 / 错误提示。抽成协议是为了测试可以注入替身，不弹真的模态框。
@MainActor
protocol SessionDialogs: AnyObject {
    /// 确认框；返回 true 表示用户选了确认按钮。
    func confirm(title: String, message: String, confirmTitle: String, destructive: Bool) async -> Bool
    /// 单行文本输入；取消返回 nil。
    func promptText(title: String, message: String?, initial: String, confirmTitle: String) async -> String?
    func showError(title: String, message: String)
}

/// 基于 `NSAlert` 的实现：有窗口时以 sheet 呈现，没有窗口时退化为应用级模态。
@MainActor
final class AlertSessionDialogs: SessionDialogs {
    /// 弱引用：由窗口控制器持有，反向强引用会形成循环。
    weak var window: NSWindow?

    func confirm(title: String, message: String, confirmTitle: String, destructive: Bool) async -> Bool {
        let alert = NSAlert()
        alert.alertStyle = destructive ? .warning : .informational
        alert.messageText = title
        alert.informativeText = message
        let confirmButton = alert.addButton(withTitle: confirmTitle)
        if destructive { confirmButton.hasDestructiveAction = true }
        alert.addButton(withTitle: String(localized: "Cancel"))
        return await run(alert) == .alertFirstButtonReturn
    }

    func promptText(title: String, message: String?, initial: String, confirmTitle: String) async -> String? {
        let alert = NSAlert()
        alert.messageText = title
        if let message { alert.informativeText = message }
        let field = NSTextField(string: initial)
        field.frame = NSRect(x: 0, y: 0, width: 260, height: 22)
        alert.accessoryView = field
        alert.addButton(withTitle: confirmTitle)
        alert.addButton(withTitle: String(localized: "Cancel"))
        // 让光标直接落在输入框里；alert 显示前窗口还不存在，所以通过 initialFirstResponder 指定。
        alert.window.initialFirstResponder = field
        guard await run(alert) == .alertFirstButtonReturn else { return nil }
        return field.stringValue
    }

    func showError(title: String, message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        if let window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    private func run(_ alert: NSAlert) async -> NSApplication.ModalResponse {
        guard let window else { return alert.runModal() }
        return await withCheckedContinuation { continuation in
            alert.beginSheetModal(for: window) { continuation.resume(returning: $0) }
        }
    }
}
