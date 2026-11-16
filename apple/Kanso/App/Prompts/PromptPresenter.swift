import Foundation
import KansoBridge
import KansoUI

/// 回答 core 提问的出口。生产实现调 `core.respondPrompt`；测试注入记录型替身。
@MainActor
protocol PromptResponder: AnyObject {
    func respond(prompt: UInt64, response: FfiPromptResponse)
}

/// 生产应答者：转给 core。必须在主线程（事件处理者所在线程）调用，不在 core 的回调栈里同步调用。
@MainActor
final class CorePromptResponder: PromptResponder {
    private let core: KansoCore
    init(core: KansoCore) { self.core = core }

    func respond(prompt: UInt64, response: FfiPromptResponse) {
        core.respondPrompt(prompt: prompt, response: response)
    }
}

/// 对话框的显示层（可注入）。生产实现把对话框作为 sheet 挂在主窗口；测试用记录型替身，不弹任何界面。
@MainActor
protocol PromptSheetHost: AnyObject {
    /// 当前能否展示（主窗口是否存在）。
    var canPresent: Bool { get }
    /// 展示对话框；用户作出选择后恰好调用一次 `completion`。
    /// 约束：同一时刻最多一个，调用方（`PromptPresenter`）保证串行。
    func present(_ model: PromptDialogModel, completion: @escaping @MainActor (FfiPromptResponse) -> Void)
    /// 关闭正在显示的对话框。**不得**再调用该次 `present` 的 `completion`：关闭原因是 core 已撤销提问，不需要回答。
    func dismissCurrent()
}

/// 提问队列与展示协调。
///
/// 同一时间只显示一个提问，其余按到达顺序排队。`promptDismissed`（core 超时 / 会话关闭）时：
/// 正在显示的关闭且不回答，排队中的直接移除。UI 不自己计时：180 秒超时由 core 处理。
@MainActor
final class PromptPresenter {
    private struct Pending {
        var id: UInt64
        var model: PromptDialogModel
    }

    private let responder: PromptResponder
    private let host: PromptSheetHost
    private let sessionTitle: (SessionID) -> String?
    private var queue: [Pending] = []
    private var current: Pending?

    /// - Parameter sessionTitle: 由会话 id 取标签标题，用于在对话框里说明是哪个标签在问。
    init(responder: PromptResponder, host: PromptSheetHost, sessionTitle: @escaping (SessionID) -> String?) {
        self.responder = responder
        self.host = host
        self.sessionTitle = sessionTitle
    }

    /// 正在显示的提问 id（测试用）。
    var currentPromptID: UInt64? { current?.id }
    /// 排队中的提问 id（测试用）。
    var queuedPromptIDs: [UInt64] { queue.map(\.id) }

    /// 收到 core 的提问。
    ///
    /// 文件传输面板与其他提问共用队列，逐个展示并支持 core 撤销。
    func enqueue(prompt: UInt64, session: SessionID?, request: FfiPromptRequest) {
        let title = session.flatMap(sessionTitle)
        guard let model = PromptDialogModel(request: request, sessionTitle: title) else {
            responder.respond(prompt: prompt, response: .cancel)
            return
        }
        queue.append(Pending(id: prompt, model: model))
        showNextIfIdle()
    }

    /// core 撤销提问。
    func dismiss(prompt: UInt64) {
        if current?.id == prompt {
            host.dismissCurrent()
            current?.model.clear()
            current = nil
            showNextIfIdle()
        } else {
            queue.removeAll { $0.id == prompt }
        }
    }

    private func showNextIfIdle() {
        while current == nil, !queue.isEmpty {
            let next = queue.removeFirst()
            // 没有窗口可挂 sheet（极端情况）：不能让 core 一直等到超时，按取消回答。
            guard host.canPresent else {
                responder.respond(prompt: next.id, response: next.model.cancelResponse)
                continue
            }
            current = next
            let id = next.id
            host.present(next.model) { [weak self] response in self?.finish(prompt: id, response: response) }
        }
    }

    private func finish(prompt: UInt64, response: FfiPromptResponse) {
        // 已被 `dismiss` 撤销或重复回调：忽略，避免对 core 已失效的提问再次应答。
        guard let done = current, done.id == prompt else { return }
        current = nil
        responder.respond(prompt: prompt, response: response)
        done.model.clear()
        showNextIfIdle()
    }
}
