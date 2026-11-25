import Foundation
import KansoBridge
import KansoUI

/// 会话启动器的视图模型：搜索、分组归类、选中项、分组与会话的增删改。不依赖 AppKit，可直接驱动测试。
///
/// 打开时读取快照；库变更通知使快照更新，搜索只过滤内存。
@MainActor
final class SessionLauncherModel {
    /// 一个分组区：`group` 为 nil 是未分组的会话（显示在最前，没有标题）。
    struct Section: Equatable {
        let group: FfiGroup?
        let profiles: [FfiProfile]
    }

    let store: SessionStore
    /// MCP 访问级别的读写。nil 表示渠道不支持 MCP（MAS / Setapp）：启动器不显示 MCP 菜单与标记。
    let mcpAccess: (any McpAccessStoring)?

    /// 一次批量设置 MCP 访问级别的结果；成功的会话保持新级别，失败的不回滚。
    struct McpAccessChange {
        let total: Int
        let failed: Int
        /// 第一个失败的错误，用来给用户说明原因；全部成功为 nil。
        let firstError: Error?
    }

    private(set) var sections: [Section] = []
    private(set) var hasAnyProfile = false
    /// 键盘选中的会话 id。
    private(set) var selectedID: String?
    private var profiles: [FfiProfile] = []
    private(set) var groups: [FfiGroup] = []
    /// 会话 id → MCP 访问级别，只含不是「关闭」的会话；每次 `reload` 用一次 `mcpAccesses()` 刷新。
    private var mcpLevels: [String: FfiMcpAccess] = [:]
    /// 最近一次读取授权失败的原因；nil 表示读取成功（或渠道不支持 MCP）。
    /// 非 nil 时授权状态未知：行尾不显示标记、MCP 菜单项禁用、不弹「从关闭调高」确认，不能当成「全部关闭」。
    private(set) var mcpReadError: String?
    nonisolated(unsafe) private var libraryObserver: NSObjectProtocol?

    private(set) var collapsedGroupIDs: Set<String>
    var onCollapsedGroupsChange: ((Set<String>) -> Void)?
    /// 库变更触发 `reload()` 后通知视图刷新（常驻在侧栏里时需要实时同步）。
    var onReload: (() -> Void)?
    private var searchTerm: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }
    var isSearching: Bool { !searchTerm.isEmpty }

    var query = "" {
        didSet {
            guard query != oldValue else { return }
            filterSnapshot()
            // 搜索词变化后选中第一个结果：回车直接连接最相关的一项。
            selectedID = visibleProfiles.first?.id
        }
    }

    init(store: SessionStore, mcpAccess: (any McpAccessStoring)? = nil, collapsedGroupIDs: Set<String> = []) {
        self.store = store
        self.mcpAccess = mcpAccess
        self.collapsedGroupIDs = collapsedGroupIDs
        reload()
        selectedID = visibleProfiles.first?.id
        libraryObserver = NotificationCenter.default.addObserver(forName: .sessionLibraryDidChange, object: store, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.reload() }
        }
    }

    deinit {
        if let libraryObserver { NotificationCenter.default.removeObserver(libraryObserver) }
    }

    // MARK: - 数据

    /// 从 core 重新读取并按当前搜索词归类。已选中的会话仍可见则保持选中，否则回到第一项。
    func reload() {
        profiles = store.profiles().sorted(by: Self.profileByName)
        groups = store.groups().sorted(by: Self.byName)
        hasAnyProfile = !profiles.isEmpty
        do {
            mcpLevels = try mcpAccess?.mcpLevelMap() ?? [:]
            mcpReadError = nil
        } catch {
            // 读失败时清空缓存并记下原因：留着旧值会显示过期标记，当成空表又会把未知当成「全部关闭」。
            mcpLevels = [:]
            mcpReadError = CoreMessageText.text(for: error)
        }
        filterSnapshot()
        onReload?()
    }

    func toggleGroup(_ id: String) {
        if collapsedGroupIDs.remove(id) == nil { collapsedGroupIDs.insert(id) }
        filterSnapshot()
        onCollapsedGroupsChange?(collapsedGroupIDs)
    }

    private func filterSnapshot() {
        let needle = searchTerm
        let groupIDs = Set(groups.map(\.id))
        var buckets: [String: [FfiProfile]] = [:]
        var ungrouped: [FfiProfile] = []
        for profile in profiles where Self.matches(profile, needle: needle) {
            if let group = profile.group, groupIDs.contains(group) {
                buckets[group, default: []].append(profile)
            } else {
                ungrouped.append(profile)
            }
        }
        var result: [Section] = []
        if !ungrouped.isEmpty { result.append(Section(group: nil, profiles: ungrouped)) }
        for group in groups {
            let members = buckets[group.id] ?? []
            // 搜索时隐藏没有结果的分组；平时空分组也要显示，否则新建后无法重命名 / 删除。
            if members.isEmpty && !needle.isEmpty { continue }
            let collapsed = needle.isEmpty && collapsedGroupIDs.contains(group.id)
            result.append(Section(group: group, profiles: collapsed ? [] : members))
        }
        sections = result
        if !visibleProfiles.contains(where: { $0.id == selectedID }) { selectedID = visibleProfiles.first?.id }
    }

    /// 屏幕上从上到下的会话顺序（键盘上下键按它移动）。
    var visibleProfiles: [FfiProfile] { sections.flatMap(\.profiles) }

    /// 名称 / 主机 / 用户名任一包含搜索词即匹配，不区分大小写与变音符号；搜索词为空匹配全部。
    nonisolated static func matches(_ profile: FfiProfile, needle: String) -> Bool {
        guard !needle.isEmpty else { return true }
        let target = profile.sshTarget
        let fields = [profile.name, target?.host, target?.user].compactMap { $0 }
        return fields.contains { $0.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
    }

    nonisolated private static func byName(_ a: FfiGroup, _ b: FfiGroup) -> Bool {
        a.name.localizedStandardCompare(b.name) == .orderedAscending
    }

    nonisolated private static func profileByName(_ a: FfiProfile, _ b: FfiProfile) -> Bool {
        a.name.localizedStandardCompare(b.name) == .orderedAscending
    }

    // MARK: - 行内容

    /// 行副标题：SSH / SFTP 为 `user@host:port`，本地为 shell 命令或"默认 Shell"。
    nonisolated static func subtitle(for profile: FfiProfile) -> String {
        if let target = profile.sshTarget {
            let hostPort = "\(target.host):\(target.port)"
            return target.user.isEmpty ? hostPort : "\(target.user)@\(hostPort)"
        }
        if case .local(let local) = profile.kind {
            let command = local.shellCommand.trimmingCharacters(in: .whitespaces)
            if !command.isEmpty { return command }
        }
        return String(localized: "Default Shell")
    }

    // MARK: - 选中

    /// 上下键移动选中，到头不循环。
    func moveSelection(by delta: Int) {
        let list = visibleProfiles
        guard !list.isEmpty else { return }
        let current = list.firstIndex { $0.id == selectedID } ?? (delta > 0 ? -1 : list.count)
        selectedID = list[min(max(current + delta, 0), list.count - 1)].id
    }

    func select(_ id: String?) { selectedID = id }

    // MARK: - 会话

    func profile(id: String) -> FfiProfile? { profiles.first { $0.id == id } }

    /// 移到分组（nil = 不分组）。其余字段原样写回。
    func move(profileID: String, toGroup groupID: String?) throws {
        guard var profile = profile(id: profileID) else { return }
        profile.group = groupID
        try store.saveProfile(profile: profile)
        store.notifyLibraryChanged()
    }

    func deleteProfile(id: String) async throws {
        try await store.deleteProfile(id: id)
        store.notifyLibraryChanged()
    }

    // MARK: - MCP 访问

    /// 渠道是否支持 MCP 访问：为假时启动器不显示 MCP 菜单与标记。
    var supportsMcpAccess: Bool { mcpAccess != nil }

    /// 会话当前的 MCP 访问级别；不在授权表里就是「关闭」。
    func mcpLevel(of profileID: String) -> FfiMcpAccess { mcpLevels[profileID] ?? .off }

    /// 分组里全部会话的 id。取自完整快照而不是 `sections`：搜索会过滤 `sections`，
    /// 而「整组设为…」必须作用于整个分组，不能只改搜索命中的那几个。
    func profileIDs(inGroup groupID: String) -> [String] {
        profiles.filter { $0.group == groupID }.map(\.id)
    }

    /// 把这些会话设成 `level` 是否会让某个原来「关闭」的会话对 AI 开放：是就要先让用户确认（spec/mcp.md §2、§7.2）。
    func raisesMcpAccessFromOff(_ ids: [String], to level: FfiMcpAccess) -> Bool {
        // 状态未知时按「可能从关闭调高」处理（要确认）：安全上宁可多问一次，不能默认放行。
        // 实际写入另由 `setMcpAccess` 的调用方在读失败时拒绝。
        guard level != .off else { return false }
        return mcpReadError != nil || ids.contains { mcpLevel(of: $0) == .off }
    }

    /// 逐个写入访问级别，成功的保留，失败的汇总进结果。写完通知会话库变化：
    /// 启动器标记和设置页的授权数据此刷新（模型自己也通过通知重读快照）。
    /// 只在渠道支持 MCP（`mcpAccess` 非 nil）时有意义，否则什么也不做。
    @discardableResult
    func setMcpAccess(_ ids: [String], to level: FfiMcpAccess) -> McpAccessChange {
        guard let mcpAccess else { return McpAccessChange(total: 0, failed: 0, firstError: nil) }
        var failed = 0
        var firstError: Error?
        for id in ids {
            do {
                try mcpAccess.setMcpAccess(profileId: id, level: level)
            } catch {
                failed += 1
                firstError = firstError ?? error
            }
        }
        store.notifyLibraryChanged()
        return McpAccessChange(total: ids.count, failed: failed, firstError: firstError)
    }

    // MARK: - 分组

    /// 新建分组，名称去掉首尾空白；空名称返回 nil 且不创建。
    @discardableResult
    func createGroup(named name: String) throws -> FfiGroup? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let group = FfiGroup(id: UUID().uuidString.lowercased(), name: trimmed)
        try store.saveGroup(group: group)
        store.notifyLibraryChanged()
        return group
    }

    func renameGroup(_ group: FfiGroup, to name: String) throws {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != group.name else { return }
        try store.saveGroup(group: FfiGroup(id: group.id, name: trimmed))
        store.notifyLibraryChanged()
    }

    /// 删除分组前的确认文字：说明会连带删除多少会话和命令（数字来自 core 的 `groupDeleteImpact`）。
    func deleteGroupConfirmation(_ group: FfiGroup) throws -> String {
        let impact = try store.groupDeleteImpact(id: group.id)
        return String(localized: "Deleting this group will also delete \(Int(impact.profiles)) sessions and \(Int(impact.snippets)) snippets.")
    }

    func deleteGroup(_ group: FfiGroup) async throws {
        try await store.deleteGroup(id: group.id)
        store.notifyLibraryChanged()
    }
}
