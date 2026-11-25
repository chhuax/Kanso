import Foundation
import KansoUI

/// 标签的文字描述（悬停提示、状态栏）。纯函数，不依赖视图。
enum TabDescriptions {
    /// 悬停提示：协议+地址（本地显示 cwd）、AI 工具名、分支、活动状态或连接状态 / 错误。每项一行。
    static func tooltip(for item: TabItem) -> String {
        var lines: [String] = []
        switch item.kind {
        case .local:
            let path = item.cwd.flatMap { $0.isEmpty ? nil : $0 }
            lines.append(path.map { String(localized: "Local · \($0)") } ?? String(localized: "Local shell"))
        case .ssh: lines.append("SSH · \(item.name)")
        case .sftp: lines.append("SFTP · \(item.name)")
        }
        switch item.activity {
        case .agentWorking(let name, _), .agentIdle(let name, _): lines.append(name)
        case .idle, .commandRunning: break
        }
        if let branch = item.gitBranch, !branch.isEmpty {
            lines.append(String(localized: "Branch: \(branch)"))
        }
        if let state = stateLine(item) { lines.append(state) }
        return lines.joined(separator: "\n")
    }

    /// 活动 / 连接状态一行；没有值得提示的状态返回 nil。
    static func stateLine(_ item: TabItem) -> String? {
        switch item.connection {
        case .connecting: return String(localized: "Connecting…")
        case .closed(let reason): return reason
        case .connected: break
        }
        switch item.activity {
        case .commandRunning: return String(localized: "Command running")
        case .agentWorking: return String(localized: "AI running")
        case .agentIdle: return String(localized: "AI finished")
        case .idle: return item.unreadDone ? String(localized: "Command finished, click to view") : nil
        }
    }

    /// 状态栏的状态文字：只在非正常连接时显示（连接中、已结束含退出码 / 错误原因）。
    /// 正常连接时为空串——位置信息已由"当前目录"条目表达，再显示一遍路径只会重复。没有活动会话时也为空串。
    static func statusBarText(for item: TabItem?) -> String {
        guard let item else { return "" }
        switch item.connection {
        case .connecting: return String(localized: "Connecting…")
        case .closed(let reason): return reason ?? String(localized: "Session ended")
        case .connected: return ""
        }
    }

    /// 状态栏的"当前目录"条目文字；没有可显示的目录时为 nil（条目隐藏）。
    ///
    /// 本地会话 home 显示为 `~`；SSH / SFTP 显示 OSC 7 上报的远端路径，原样显示——
    /// 远端 home 未知，不能用本机 home 去替换。没有上报（cwd 为空）就不显示，不用会话名充数。
    static func directoryText(for item: TabItem?, homeDirectory: String) -> String? {
        guard let item, let cwd = item.cwd, !cwd.isEmpty else { return nil }
        return item.kind == .local ? TabPresentation.displayPath(cwd, homeDirectory: homeDirectory) : cwd
    }
}
