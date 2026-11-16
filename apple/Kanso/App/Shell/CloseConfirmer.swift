import AppKit
import KansoUI

/// 交给确认者的关闭请求。
struct CloseConfirmationRequest: Equatable {
    /// 需要列出的会话标题（仍在运行 / 连接的会话）。
    var titles: [String]
    /// 关闭会中断正在进行的传输。
    var interruptsTransfers: Bool
    /// 请求来自退出应用 / 关闭窗口（影响按钮与标题文字）。
    var isQuit: Bool
}

/// 关闭确认。生产实现是 `AlertCloseConfirmer`（NSAlert sheet）；测试注入总是同意 / 总是拒绝的替身，
/// 避免在测试里弹出模态框。
///
/// 调用约束：批量关闭只调用一次，`titles` 已包含本次要列出的全部会话。
@MainActor
protocol CloseConfirmer: AnyObject {
    /// 返回 true 表示用户同意关闭。
    func confirm(_ request: CloseConfirmationRequest) async -> Bool
}

/// 基于 NSAlert 的确认实现：有窗口时以 sheet 呈现，没有窗口时退化为应用级模态。
@MainActor
final class AlertCloseConfirmer: CloseConfirmer {
    /// 弱引用：确认者由窗口控制器持有，反向强引用会形成循环。
    weak var window: NSWindow?

    /// 确认框最多列出的会话数，超出部分汇总成"另外 N 个"。
    private static let maxListed = 8

    func confirm(_ request: CloseConfirmationRequest) async -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = request.isQuit
            ? String(localized: "Quit Kanso?")
            : String(localized: "Close sessions that are still running?")
        var lines = request.titles.prefix(Self.maxListed).map { "• \($0)" }
        if request.titles.count > Self.maxListed {
            lines.append(String(localized: "…and \(request.titles.count - Self.maxListed) more"))
        }
        var info = String(localized: "These sessions are still running or connected:") + "\n" + lines.joined(separator: "\n")
        if request.interruptsTransfers {
            info += "\n\n" + String(localized: "Closing will interrupt transfers in progress.")
        }
        alert.informativeText = info
        alert.addButton(withTitle: request.isQuit ? String(localized: "Quit") : String(localized: "Close"))
        alert.addButton(withTitle: String(localized: "Cancel"))

        guard let window else { return alert.runModal() == .alertFirstButtonReturn }
        return await withCheckedContinuation { continuation in
            alert.beginSheetModal(for: window) { response in
                continuation.resume(returning: response == .alertFirstButtonReturn)
            }
        }
    }
}
