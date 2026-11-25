import CoreGraphics
import Foundation
import Observation

/// 会话标识（与 core 的 session id 同型，模型层不依赖 FFI 类型）。
public typealias SessionID = UInt64

public enum TabKind: Sendable, Equatable { case local, ssh, sftp }

public enum TabConnection: Equatable, Sendable {
    case connecting
    case connected
    /// `reason` 是已本地化的错误文字；nil 表示正常结束/用户关闭。
    case closed(reason: String?)
}

public enum TabActivity: Equatable, Sendable {
    case idle
    case commandRunning
    /// `iconAsset` 是 Assets 模板图名；`nil` 表示用通用 SF Symbol。
    case agentWorking(name: String, iconAsset: String? = nil)
    case agentIdle(name: String, iconAsset: String? = nil)

    /// "忙"：有前台命令或 AI 在运行。
    var isBusy: Bool {
        switch self {
        case .commandRunning, .agentWorking: true
        case .idle, .agentIdle: false
        }
    }
}

public struct TabItem: Identifiable, Equatable, Sendable {
    public let id: SessionID
    public var kind: TabKind
    public var name: String
    /// 用户或 MCP 设置的自定义标签名；nil 表示使用默认标题。
    public var customTitle: String?
    public var profileID: String?
    /// 配置里设置的会话颜色（#RRGGBB）；nil 则按 `TabPresentation.colorIndex` 稳定生成。
    public var colorHex: String?
    public var cwd: String?
    public var cwdHost: String?
    public var gitBranch: String?
    public var connection: TabConnection
    public var activity: TabActivity
    /// 有未读完成：一直保留直到用户切到这个标签。
    public var unreadDone: Bool
    public var activeTransfers: Int

    public init(
        id: SessionID,
        kind: TabKind,
        name: String,
        customTitle: String? = nil,
        profileID: String? = nil,
        colorHex: String? = nil,
        cwd: String? = nil,
        cwdHost: String? = nil,
        gitBranch: String? = nil,
        connection: TabConnection = .connecting,
        activity: TabActivity = .idle,
        unreadDone: Bool = false,
        activeTransfers: Int = 0
    ) {
        self.id = id
        self.kind = kind
        self.name = name
        self.customTitle = customTitle
        self.profileID = profileID
        self.colorHex = colorHex
        self.cwd = cwd
        self.cwdHost = cwdHost
        self.gitBranch = gitBranch
        self.connection = connection
        self.activity = activity
        self.unreadDone = unreadDone
        self.activeTransfers = activeTransfers
    }
}

/// 侧栏里的一个分屏组。
public struct SidebarSection: Equatable, Sendable, Identifiable {
    public var pane: PaneID
    /// 标题序号，从 1 开始（"分屏 1"）。
    public var titleIndex: Int
    public var tabs: [TabItem]
    public var id: PaneID { pane }

    public init(pane: PaneID, titleIndex: Int, tabs: [TabItem]) {
        self.pane = pane
        self.titleIndex = titleIndex
        self.tabs = tabs
    }
}

public enum CloseScope: Sendable, Equatable {
    case others
    /// 菜单叫"关闭左侧"，竖向列表里是上方。
    case above
    /// 菜单叫"关闭右侧"，竖向列表里是下方。
    case below
    case all
}

/// 需要用户确认的关闭请求。
public struct CloseConfirmation: Equatable, Sendable {
    /// 需要列在确认提示里的会话（保持传入顺序）。
    public var sessions: [SessionID]
    /// 其中有文件传输正在进行，关闭会中断传输。
    public var interruptsTransfers: Bool

    public init(sessions: [SessionID], interruptsTransfers: Bool) {
        self.sessions = sessions
        self.interruptsTransfers = interruptsTransfers
    }
}

/// 工作区：分屏布局 + 每个分屏的有序标签 + 活动状态。
///
/// 纯逻辑，不含视图、不依赖 FFI。所有操作同步完成，对不存在的 id 安静地不做事。
/// 这里只改位置与展示状态，不关闭会话：关闭要由调用方走确认和 core，成功后再调 `removeTab`。
@MainActor
@Observable
public final class Workspace {
    public private(set) var layout: SplitLayout
    public private(set) var activePane: PaneID

    private var paneTabs: [PaneID: [SessionID]]
    private var paneActive: [PaneID: SessionID]
    private var items: [SessionID: TabItem] = [:]
    /// 计数器不参与观察：只在内部分配 id 时使用。
    @ObservationIgnored private var nextPaneRaw: Int

    /// 初始状态：一个空 pane（零个标签，UI 据此显示空状态页）。
    public init() {
        let first = PaneID(raw: 1)
        layout = SplitLayout(single: first)
        activePane = first
        paneTabs = [first: []]
        paneActive = [:]
        nextPaneRaw = 2
    }

    // MARK: - 查询

    public var allPanes: [PaneID] { layout.panes }

    public func tabs(in pane: PaneID) -> [TabItem] {
        (paneTabs[pane] ?? []).compactMap { items[$0] }
    }

    public func activeTab(in pane: PaneID) -> SessionID? {
        paneActive[pane]
    }

    /// 活动 pane 的活动标签。
    public var activeSession: SessionID? { paneActive[activePane] }

    public func item(_ id: SessionID) -> TabItem? { items[id] }

    /// 标签所在的 pane。
    public func pane(of session: SessionID) -> PaneID? {
        items[session] == nil ? nil : paneTabs.first { $0.value.contains(session) }?.key
    }

    /// 侧栏顺序：按 pane 阅读顺序拼接每个 pane 的标签。
    public var sidebarOrder: [SessionID] {
        allPanes.flatMap { paneTabs[$0] ?? [] }
    }

    public var sidebarSections: [SidebarSection] {
        allPanes.enumerated().map { offset, pane in
            SidebarSection(pane: pane, titleIndex: offset + 1, tabs: tabs(in: pane))
        }
    }

    /// 只有多个 pane 时才显示分组标题。
    public var showsSectionHeaders: Bool { allPanes.count > 1 }

    /// 路径筛选后的侧栏。只影响显示，不改顺序和活动状态；
    /// 空组仍然返回（`tabs` 为空），是否隐藏由视图决定。
    public func filteredSidebar(_ query: String) -> [SidebarSection] {
        sidebarSections.map { section in
            var s = section
            s.tabs = section.tabs.filter { TabPresentation.matchesPathFilter($0, query: query) }
            return s
        }
    }

    // MARK: - 标签增删与激活

    /// 加到 pane（默认活动 pane）当前活动标签的后面；pane 为空时成为唯一标签。
    /// 重复 id 或 pane 不存在时忽略。`activate == false` 时不改变活动标签，
    /// 但空 pane 里的第一个标签总会成为该 pane 的活动标签（否则 pane 有标签却没有活动标签）。
    public func addTab(_ item: TabItem, in pane: PaneID? = nil, activate: Bool = true) {
        let target = pane ?? activePane
        guard paneTabs[target] != nil, items[item.id] == nil else { return }
        items[item.id] = item
        var list = paneTabs[target] ?? []
        if let active = paneActive[target], let idx = list.firstIndex(of: active) {
            list.insert(item.id, at: idx + 1)
        } else {
            list.append(item.id)
        }
        paneTabs[target] = list
        if paneActive[target] == nil {
            paneActive[target] = item.id
        }
        if activate {
            self.activate(item.id)
        }
    }

    /// 设为所在 pane 的活动标签，同时把该 pane 设为活动 pane，并清除 `unreadDone`。
    @discardableResult
    public func activate(_ session: SessionID) -> Bool {
        guard let pane = pane(of: session) else { return false }
        paneActive[pane] = session
        activePane = pane
        items[session]?.unreadDone = false
        return true
    }

    /// 切换活动 pane，同时清除该 pane 活动标签的 `unreadDone`（它现在被用户看到了）。
    @discardableResult
    public func activatePane(_ pane: PaneID) -> Bool {
        guard paneTabs[pane] != nil else { return false }
        activePane = pane
        if let session = paneActive[pane] {
            items[session]?.unreadDone = false
        }
        return true
    }

    /// 从 pane 中移除标签（会话已关闭后调用）。
    ///
    /// 若它是该 pane 的活动标签，新的活动标签取原位置的下一个，没有则取上一个。
    /// pane 因此变空且不是唯一 pane → 从 layout 移除（份额按比例分给兄弟）；
    /// 若它是活动 pane，活动 pane 改为阅读顺序中的前一个（没有前一个取新的第一个）。
    /// 唯一 pane 变空则保留空 pane。
    @discardableResult
    public func removeTab(_ session: SessionID) -> Bool {
        guard detach(session) else { return false }
        items[session] = nil
        return true
    }

    /// 在活动 pane 后面拆出新 pane，把 `item` 放进去并激活新 pane。
    /// 调用方负责先开好新会话（同一会话不能同时显示在两处）。
    /// `item.id` 已存在时不做任何事，返回当前活动 pane。
    /// 活动 pane 没有标签（空状态页）时不拆分，直接把标签放进它：否则布局里会留下一个
    /// 没有标签的 pane，而空 pane 只允许在"唯一 pane"时存在。
    @discardableResult
    public func splitActivePane(axis: SplitAxis, with item: TabItem) -> PaneID {
        guard items[item.id] == nil else { return activePane }
        if paneTabs[activePane]?.isEmpty ?? true {
            addTab(item, in: activePane, activate: true)
            return activePane
        }
        let new = PaneID(raw: nextPaneRaw)
        guard layout.split(activePane, axis: axis, newPane: new) else { return activePane }
        nextPaneRaw += 1
        paneTabs[new] = []
        addTab(item, in: new, activate: true)
        return new
    }

    // MARK: - 分隔条

    /// 拖动分隔条（转发给 `SplitLayout.moveDivider`）。
    /// `layout` 对外只读，视图层拖动分隔条只能经由这里改动，才能触发 Observation 刷新。
    public func moveDivider(splitPath: [Int], dividerIndex: Int, delta: Double, minFraction: Double) {
        layout.moveDivider(splitPath: splitPath, dividerIndex: dividerIndex, delta: delta, minFraction: minFraction)
    }

    // MARK: - 切换

    public func selectNextTabInPane() { cycleTab(by: 1) }
    public func selectPreviousTabInPane() { cycleTab(by: -1) }
    public func selectNextPane() { cyclePane(by: 1) }
    public func selectPreviousPane() { cyclePane(by: -1) }

    /// ⌘1–⌘9 用；越界返回 false。
    @discardableResult
    public func selectTab(atSidebarIndex index: Int) -> Bool {
        let order = sidebarOrder
        guard order.indices.contains(index) else { return false }
        return activate(order[index])
    }

    private func cycleTab(by step: Int) {
        let list = paneTabs[activePane] ?? []
        guard list.count > 1, let active = paneActive[activePane], let idx = list.firstIndex(of: active) else { return }
        activate(list[(idx + step + list.count) % list.count])
    }

    private func cyclePane(by step: Int) {
        let panes = allPanes
        guard panes.count > 1, let idx = panes.firstIndex(of: activePane) else { return }
        activatePane(panes[(idx + step + panes.count) % panes.count])
    }

    // MARK: - 移动

    /// 同 pane 内重排。`toIndex` 是移除自身后的插入位置，夹到合法范围。不改变活动状态。
    @discardableResult
    public func moveTab(_ session: SessionID, toIndex: Int) -> Bool {
        guard let pane = pane(of: session), var list = paneTabs[pane], let from = list.firstIndex(of: session) else {
            return false
        }
        list.remove(at: from)
        list.insert(session, at: min(max(toIndex, 0), list.count))
        paneTabs[pane] = list
        return true
    }

    /// 跨 pane 移动：会话不重建，只改位置。`index` 为移除/插入后目标 pane 内的位置，nil 表示末尾。
    /// 源 pane 变空按 `removeTab` 的规则收起；目标 pane 激活该标签。
    /// 目标就是源 pane 时退化为同组重排（nil 即移到末尾）。
    @discardableResult
    public func moveTab(_ session: SessionID, toPane target: PaneID, at index: Int? = nil) -> Bool {
        guard let source = pane(of: session), paneTabs[target] != nil else { return false }
        if source == target {
            return moveTab(session, toIndex: index ?? Int.max)
        }
        guard detach(session) else { return false }
        var list = paneTabs[target] ?? []
        list.insert(session, at: min(max(index ?? list.count, 0), list.count))
        paneTabs[target] = list
        paneActive[target] = session
        activate(session)
        return true
    }

    /// 把标签从所在 pane 摘下（保留 `items`），并处理活动标签与空 pane 收起。
    private func detach(_ session: SessionID) -> Bool {
        guard let pane = pane(of: session), var list = paneTabs[pane], let idx = list.firstIndex(of: session) else {
            return false
        }
        list.remove(at: idx)
        paneTabs[pane] = list

        if list.isEmpty {
            paneActive[pane] = nil
            let before = allPanes
            if before.count > 1 {
                let previous = before.firstIndex(of: pane).flatMap { $0 > 0 ? before[$0 - 1] : nil }
                if layout.remove(pane) {
                    paneTabs[pane] = nil
                    if activePane == pane {
                        activePane = previous ?? allPanes[0]
                        if let s = paneActive[activePane] { items[s]?.unreadDone = false }
                    }
                }
            }
        } else if paneActive[pane] == session {
            // 原位置的下一个已前移到 idx；越界说明删的是最后一个，取上一个。
            let next = list[min(idx, list.count - 1)]
            paneActive[pane] = next
            // 活动 pane 里新露出的标签正被用户看到，清除未读。
            if pane == activePane { items[next]?.unreadDone = false }
        }
        return true
    }

    // MARK: - 批量关闭

    /// 批量关闭的目标（只在 `session` 所在的分屏组内）。只返回 id，不执行关闭。
    public func closeTargets(_ scope: CloseScope, relativeTo session: SessionID) -> [SessionID] {
        guard let pane = pane(of: session), let list = paneTabs[pane], let idx = list.firstIndex(of: session) else {
            return []
        }
        switch scope {
        case .others: return list.filter { $0 != session }
        case .above: return Array(list[..<idx])
        case .below: return Array(list[(idx + 1)...])
        case .all: return list
        }
    }

    /// 关闭前是否需要确认（§8）。
    ///
    /// - SSH/SFTP：`.connecting` 或 `.connected` 需要确认。
    /// - 本地 shell：只在有前台命令或 AI 在运行时（commandRunning / agentWorking）才需要确认。
    ///   这是对 §8"仍处于连接状态"的解释：本地 shell 空闲时也算"连接着"，
    ///   但那样每次 ⌘W 都会弹窗，体验很差，所以空闲的本地 shell 直接关。
    /// - 任何会话 `activeTransfers > 0` 都需要确认，且 `interruptsTransfers = true`。
    /// - 已 `.closed` 的会话不需要确认。不存在的 id 忽略。
    /// 不需要确认返回 nil。
    public func closeConfirmation(for sessions: [SessionID]) -> CloseConfirmation? {
        var needed: [SessionID] = []
        var interrupts = false
        for id in sessions {
            guard let item = items[id] else { continue }
            if case .closed = item.connection { continue }
            let live: Bool
            switch item.kind {
            case .ssh, .sftp: live = true // 上面已排除 closed，剩下 connecting / connected
            case .local: live = item.activity.isBusy
            }
            let transferring = item.activeTransfers > 0
            if live || transferring {
                needed.append(id)
                if transferring { interrupts = true }
            }
        }
        return needed.isEmpty ? nil : CloseConfirmation(sessions: needed, interruptsTransfers: interrupts)
    }

    // MARK: - 事件应用

    @discardableResult
    public func applyLocation(_ session: SessionID, cwd: String?, cwdHost: String?, gitBranch: String?) -> Bool {
        guard items[session] != nil else { return false }
        items[session]?.cwd = cwd
        items[session]?.cwdHost = cwdHost
        items[session]?.gitBranch = gitBranch
        return true
    }

    @discardableResult
    public func applyConnection(_ session: SessionID, _ connection: TabConnection) -> Bool {
        guard items[session] != nil else { return false }
        items[session]?.connection = connection
        return true
    }

    /// 从"忙"变到"闲"时，如果该标签不是当前 `activeSession`，置 `unreadDone`。
    /// 用户正看着的标签不需要未读提示。
    @discardableResult
    public func applyActivity(_ session: SessionID, _ activity: TabActivity) -> Bool {
        guard let old = items[session]?.activity else { return false }
        items[session]?.activity = activity
        if old.isBusy, !activity.isBusy, session != activeSession {
            items[session]?.unreadDone = true
        }
        return true
    }

    /// 设置或清除标签的自定义名（去掉首尾空白后为空视为清除）。会话不存在返回 false。
    @discardableResult
    public func setCustomTitle(_ session: SessionID, _ title: String?) -> Bool {
        guard items[session] != nil else { return false }
        let trimmed = title?.trimmingCharacters(in: .whitespacesAndNewlines)
        items[session]?.customTitle = (trimmed?.isEmpty == false) ? trimmed : nil
        return true
    }

    /// 传输计数加减，不低于 0（防止重复的结束事件把计数变成负数）。
    @discardableResult
    public func applyTransfer(_ session: SessionID, started: Bool) -> Bool {
        guard let n = items[session]?.activeTransfers else { return false }
        items[session]?.activeTransfers = started ? n + 1 : max(0, n - 1)
        return true
    }
}
