import AppKit
import KansoUI

/// "帮助 → 恢复默认设置…"的流程：确认 → 重置 → 失败时提示。
///
/// 确认与失败提示抽成参数，是为了测试不弹模态框；生产用 `confirmWithAlert` / `presentFailure`。
/// 重置只清设置（含快捷键），**不动**会话、分组、Sender 命令和历史（store-import.md §9）——
/// 这由 core 的 `reset_settings` 保证，这里不需要再操心。
@MainActor
enum SettingsResetFlow {
    /// 返回是否执行了重置（用户取消或失败返回 false）。
    @discardableResult
    static func run(
        settings: SettingsStore,
        confirm: () async -> Bool,
        onFailure: (Error) -> Void
    ) async -> Bool {
        guard await confirm() else { return false }
        do {
            try settings.resetToDefaults()
            return true
        } catch {
            onFailure(error)
            return false
        }
    }

    static func confirmWithAlert() async -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "Restore default settings?")
        alert.informativeText = String(
            localized: "Display, appearance and keyboard shortcut settings will be reset. Sessions, groups, snippets and history are not affected."
        )
        alert.addButton(withTitle: String(localized: "Restore"))
        alert.addButton(withTitle: String(localized: "Cancel"))
        let response: NSApplication.ModalResponse
        if let window = NSApp.keyWindow {
            response = await alert.beginSheetModal(for: window)
        } else {
            response = alert.runModal()
        }
        return response == .alertFirstButtonReturn
    }

    static func presentFailure(_ error: Error) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "Couldn’t restore default settings")
        // core 的用户错误按语言渲染；其余技术细节是英文，不本地化（只用于排查）。
        alert.informativeText = CoreMessageText.text(for: error)
        alert.runModal()
    }
}
