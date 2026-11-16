import Foundation
import KansoBridge
import KansoUI

/// core 事件翻译成的、对工作区的一次更新。纯值类型，便于单测。
enum SessionUpdate: Equatable {
    case connection(SessionID, TabConnection)
    case location(SessionID, cwd: String?, cwdHost: String?, gitBranch: String?)
    case activity(SessionID, TabActivity)
    case transferStarted(transfer: UInt64, session: SessionID)
    /// 结束事件不带 session，要靠 `TransferTracker` 反查。
    case transferFinished(transfer: UInt64)
    /// 与标签状态无关的事件（TerminalDirty、通知等）。
    case ignored
}

/// 传输 id → 会话的映射。`transferFinished` 事件没有 session 字段，
/// 所以必须在 `transferStarted` 时记下来，否则没法把活动传输计数减回去。
struct TransferTracker: Equatable {
    private var sessionByTransfer: [UInt64: SessionID] = [:]

    mutating func start(_ transfer: UInt64, session: SessionID) {
        sessionByTransfer[transfer] = session
    }

    /// 返回该传输所属会话并移除映射；未知 id（重复的结束事件）返回 nil。
    mutating func finish(_ transfer: UInt64) -> SessionID? {
        sessionByTransfer.removeValue(forKey: transfer)
    }

    /// 会话被移除时清理，避免映射无限增长。
    mutating func forget(session: SessionID) {
        sessionByTransfer = sessionByTransfer.filter { $0.value != session }
    }

    var count: Int { sessionByTransfer.count }
}

/// `FfiCoreEvent` 到工作区更新的纯函数层。
enum CoreEventMapping {
    static func update(for event: FfiCoreEvent) -> SessionUpdate {
        switch event {
        case .sessionStateChanged(let session, let state):
            return .connection(session, connection(for: state))
        case .locationChanged(let session, let cwd, let cwdHost, let gitBranch):
            return .location(session, cwd: cwd, cwdHost: cwdHost, gitBranch: gitBranch)
        case .activityChanged(let session, let activity):
            return .activity(session, self.activity(for: activity))
        case .transferStarted(let transfer, let session, _, _, _, _):
            return .transferStarted(transfer: transfer, session: session)
        case .transferFinished(let transfer, _):
            return .transferFinished(transfer: transfer)
        default:
            return .ignored
        }
    }

    static func connection(for state: FfiSessionState) -> TabConnection {
        switch state {
        case .connecting: return .connecting
        case .connected: return .connected
        case .closed(let reason): return .closed(reason: closeText(for: reason))
        }
    }

    /// 关闭原因的展示文字。用户主动关闭返回 nil（不提示"已结束"，local-shell.md §7）。
    static func closeText(for reason: FfiCloseReason) -> String? {
        switch reason {
        case .byUser:
            return nil
        case .exited(let code):
            if let code { return String(localized: "Session ended (exit code \(code))") }
            return String(localized: "Session ended")
        case .error(let message):
            return CoreMessageText.text(message)
        }
    }

    static func activity(for activity: FfiActivity) -> TabActivity {
        switch activity {
        case .idle: return .idle
        case .commandRunning: return .commandRunning
        case .agentIdle(let agent): return .agentIdle(name: agentName(agent), iconAsset: AgentIcon.assetName(for: agent))
        case .agentWorking(let agent): return .agentWorking(name: agentName(agent), iconAsset: AgentIcon.assetName(for: agent))
        }
    }

    /// AI 工具的显示名。名字是产品名，不随语言变化；未知取值（core 新增了 App 还不认识的工具）用通用名。
    static func agentName(_ agent: FfiAgentKind) -> String {
        switch agent {
        case .claude: return "Claude Code"
        case .codex: return "Codex"
        case .gemini: return "Gemini CLI"
        case .aider: return "Aider"
        default: return "AI"
        }
    }

    static func tabKind(for kind: FfiSessionKind) -> TabKind {
        switch kind {
        case .local: return .local
        case .ssh: return .ssh
        case .sftp: return .sftp
        }
    }

    /// 把更新应用到工作区。传输计数按 `tracker` 反查会话。
    @MainActor
    static func apply(_ update: SessionUpdate, to workspace: Workspace, transfers tracker: inout TransferTracker) {
        switch update {
        case .connection(let id, let connection):
            workspace.applyConnection(id, connection)
        case .location(let id, let cwd, let host, let branch):
            workspace.applyLocation(id, cwd: cwd, cwdHost: host, gitBranch: branch)
        case .activity(let id, let activity):
            workspace.applyActivity(id, activity)
        case .transferStarted(let transfer, let session):
            // 会话已不在工作区（刚关闭）时不记映射，否则结束事件永远不会来清理。
            guard workspace.item(session) != nil else { return }
            tracker.start(transfer, session: session)
            workspace.applyTransfer(session, started: true)
        case .transferFinished(let transfer):
            if let session = tracker.finish(transfer) {
                workspace.applyTransfer(session, started: false)
            }
        case .ignored:
            break
        }
    }
}
