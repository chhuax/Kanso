import AppKit
import KansoBridge
import KansoUI
import TerminalKit

extension GutterMode {
    /// 持久化用的稳定字符串。不要用 case 顺序或 `String(describing:)`：改名 / 调整顺序会让旧偏好失效。
    var storageValue: String {
        switch self {
        case .off: "off"
        case .lineNumbers: "lineNumbers"
        case .timestamps: "timestamps"
        case .both: "both"
        }
    }

    init?(storageValue: String) {
        guard let mode = Self.allCases.first(where: { $0.storageValue == storageValue }) else { return nil }
        self = mode
    }

    /// 菜单项显示名（ui-shell.md §6 四种模式）。
    var displayName: String {
        switch self {
        case .off: String(localized: "Off")
        case .lineNumbers: String(localized: "Line Numbers Only")
        case .timestamps: String(localized: "Timestamps Only")
        case .both: String(localized: "Time and Line Numbers")
        }
    }

    /// 对应的 core 枚举。注意 core 侧"仅时间"叫 `time`。
    var ffiMode: FfiTimestampMode {
        switch self {
        case .off: .off
        case .lineNumbers: .lineNumbers
        case .timestamps: .time
        case .both: .both
        }
    }
}

/// 时间戳 / 行号栏显示模式（全局，所有会话共用）的分发（持久化交给 `SettingsStore`，core 设置表的 `timeline` 分区），以及 AI 会话的临时设置。
///
/// **AI 会话的临时设置**（terminal.md §2、ui-shell.md §9）：会话第一次报告 agent 活动时，该会话的时间栏默认隐藏，
/// 不影响全局；用户在这个会话里点状态栏"时间轴"只改这个会话；同一工具重复上报保留用户的选择；
/// 工具退出（活动回到非 agent）或换成另一个工具时临时设置作废。临时设置只存在内存里（随会话，不持久化）。
/// 本类只维护状态并告诉调用方"这个会话现在该用什么模式"，把模式应用到视图与 core 是 `WorkspaceController` 的事。
//
/// 接入方：`AppDelegate` 持有唯一实例，主菜单"会话 → 时间戳与行号"用 `makeMenu()` 生成，
/// 状态栏"时间轴"开关调用 `toggleTimeline()`；`WorkspaceController` 通过 `observe` 把变化应用到所有终端视图并同步给 core。
/// 切换后网格列数改变，由视图已有的 `onGridSizeChange` → `core.resize` 链路处理，这里不需要额外通知 core 调整尺寸。
@MainActor
final class GutterModeController: NSObject, NSMenuItemValidation {
    private let settings: SettingsStore
    /// 最近一次通知给观察者的模式：设置载入 / 恢复默认会让模式在"设置值"层面变化，要据此判断是否真的变了。
    private var notifiedMode: GutterMode

    private struct Observer {
        weak var owner: AnyObject?
        let handler: (GutterMode) -> Void
    }
    private var observers: [Observer] = []

    /// "时间轴"开关重新打开时恢复的模式，跨重启持久化。读不到、无效或为 off 时回退 `.both`。
    /// 单独存一个字段而不是从 `mode` 推导：`mode` 为 off 时无法得知关闭前是什么。
    var lastVisibleMode: GutterMode { settings.timeline.lastVisible }

    /// - Parameter settings: 设置存储。测试注入独立实例（不挂后端即纯内存，不碰真实存储）。
    init(settings: SettingsStore = SettingsStore()) {
        self.settings = settings
        notifiedMode = settings.timeline.mode
        super.init()
        settings.observe(self) { [weak self] section in
            guard section == .timeline else { return }
            self?.settingsDidChange()
        }
    }

    private func settingsDidChange() {
        let current = mode
        guard current != notifiedMode else { return }
        notifiedMode = current
        observers.removeAll { $0.owner == nil }
        for observer in observers { observer.handler(current) }
    }

    /// 当前模式。未设置或存了无法识别的值时为 `.off`（terminal.md §2 默认关闭，由设置解码保证）。
    /// setter 写入设置；值真正变化时通知观察者（值不变时不通知）。
    var mode: GutterMode {
        get { settings.timeline.mode }
        set {
            settings.updateTimeline {
                $0.mode = newValue
                if newValue != .off { $0.lastVisible = newValue }
            }
        }
    }

    /// 状态栏"时间轴"是否处于打开状态（任一栏显示即算打开）。
    var isTimelineOn: Bool { mode != .off }

    /// 在"关闭"与"上次非关闭的模式"之间切换。
    func toggleTimeline() {
        mode = isTimelineOn ? .off : lastVisibleMode
    }

    // MARK: AI 会话的临时设置

    private struct AgentOverride {
        /// 触发这个临时设置的工具：同一工具重复上报要保留用户选择，换工具则作废。
        var agent: FfiAgentKind
        var mode: GutterMode
    }
    private var overrides: [SessionID: AgentOverride] = [:]

    /// AI 会话刚被识别时的默认模式：全屏 TUI 不需要时间栏（terminal.md §2）。
    static let agentDefaultMode: GutterMode = .off

    /// 该会话的临时模式；没有临时设置（普通会话）为 nil。
    func override(for session: SessionID) -> GutterMode? { overrides[session]?.mode }

    /// 该会话实际使用的模式：有临时设置用它，否则用全局模式。`session` 为 nil（没有活动会话）用全局模式。
    func effectiveMode(for session: SessionID?) -> GutterMode {
        session.flatMap { overrides[$0]?.mode } ?? mode
    }

    /// 状态栏"时间轴"开关对该会话是否处于打开状态。
    func isTimelineOn(for session: SessionID?) -> Bool { effectiveMode(for: session) != .off }

    /// 会话报告了 agent 活动（`agentIdle` / `agentWorking` 都算）。
    ///
    /// - Returns: 该会话需要改用的模式（调用方应用到视图与 core）；同一工具重复上报、保留现有设置时为 nil。
    ///   换成另一个工具时旧设置（包括用户的选择）作废，按"第一次报告"处理，重新默认隐藏。
    func agentReported(session: SessionID, agent: FfiAgentKind) -> GutterMode? {
        if overrides[session]?.agent == agent { return nil }
        overrides[session] = AgentOverride(agent: agent, mode: Self.agentDefaultMode)
        return Self.agentDefaultMode
    }

    /// 会话报告了非 agent 活动（空闲或普通命令运行）：工具已退出，临时设置作废。
    ///
    /// - Returns: 回到的全局模式；本来就没有临时设置时为 nil（不需要动视图，也不必打扰 core）。
    func agentEnded(session: SessionID) -> GutterMode? {
        overrides.removeValue(forKey: session) == nil ? nil : mode
    }

    /// 会话关闭，丢弃它的临时设置。
    func forget(session: SessionID) {
        overrides[session] = nil
    }

    /// 状态栏"时间轴"开关：当前会话有临时设置时**只改这个会话**，否则切换全局模式。
    ///
    /// - Returns: 只改了该会话时返回它的新模式（调用方应用到视图与 core）；切换的是全局模式返回 nil
    ///   （全局变化经 `observe` 通知，由 `WorkspaceController` 统一应用）。
    @discardableResult
    func toggleTimeline(for session: SessionID?) -> GutterMode? {
        guard let session, var current = overrides[session] else {
            toggleTimeline()
            return nil
        }
        current.mode = current.mode == .off ? lastVisibleMode : .off
        overrides[session] = current
        return current.mode
    }

    /// 注册模式变化回调（仅值真正变化时触发）。`owner` 只用于生命周期判断（弱引用）。
    func observe(_ owner: AnyObject, _ handler: @escaping (GutterMode) -> Void) {
        observers.append(Observer(owner: owner, handler: handler))
    }

    // MARK: 菜单

    /// 生成"时间戳与行号"子菜单：四项单选。勾选在 `validateMenuItem` 里刷新。
    func makeMenu() -> NSMenu {
        let menu = NSMenu(title: String(localized: "Timestamps and Line Numbers"))
        for mode in Self.menuOrder {
            let item = NSMenuItem(title: mode.displayName, action: #selector(selectMode(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = mode.storageValue
            item.state = mode == self.mode ? .on : .off
            menu.addItem(item)
        }
        return menu
    }

    /// 菜单顺序：关闭 / 只显示行号 / 只显示时间 / 时间和行号（ui-shell.md §6）。
    private static let menuOrder: [GutterMode] = [.off, .lineNumbers, .timestamps, .both]

    @objc func selectMode(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let selected = GutterMode(storageValue: raw) else { return }
        mode = selected
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(selectMode(_:)), let raw = menuItem.representedObject as? String {
            menuItem.state = raw == mode.storageValue ? .on : .off
        }
        return true
    }
}
