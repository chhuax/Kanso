import Foundation

/// 标签的展示辅助（纯函数，不依赖视图）。
public enum TabPresentation {
    /// 把本机路径里的 home 前缀替换为 `~`，并去掉末尾多余的 `/`（根目录 `/` 除外）。
    /// `/Users/ab` 不是 `/Users/a` 的子目录，不替换。
    public static func displayPath(_ cwd: String, homeDirectory: String) -> String {
        let path = trimTrailingSlashes(cwd)
        let home = trimTrailingSlashes(homeDirectory)
        // home 为空或根目录时替换没有意义（否则所有绝对路径都会变成 `~/...`）。
        guard !home.isEmpty, home != "/" else { return path }
        if path == home { return "~" }
        if path.hasPrefix(home + "/") { return "~" + path.dropFirst(home.count) }
        return path
    }

    /// 主标题：已设置自定义名 → 自定义名；否则本地且 cwd 已知 → 路径（home 前缀替换为 `~`）；其他 → `name`。
    ///
    /// 返回完整字符串，过长时截断由视图层做。
    public static func primaryTitle(_ item: TabItem, homeDirectory: String) -> String {
        if let custom = item.customTitle, !custom.isEmpty { return custom }
        guard item.kind == .local, let cwd = item.cwd, !cwd.isEmpty else { return item.name }
        return displayPath(cwd, homeDirectory: homeDirectory)
    }

    /// 副标题：
    /// - 未改名：本地且有 Git 分支 → 分支名；否则 nil（不在仓库里不显示，也不重复显示目录）。
    /// - 已改名：保留上下文——SSH / SFTP 显示原会话名（与自定义名不同时）；
    ///   本地显示当前目录，在仓库里时连同分支一并显示（`~/work · main`）。
    public static func subtitle(_ item: TabItem, homeDirectory: String = "") -> String? {
        if let custom = item.customTitle, !custom.isEmpty {
            switch item.kind {
            case .local:
                let dir = item.cwd.flatMap { $0.isEmpty ? nil : displayPath($0, homeDirectory: homeDirectory) }
                let branch = item.gitBranch.flatMap { $0.isEmpty ? nil : $0 }
                switch (dir, branch) {
                case (let d?, let b?): return "\(d) · \(b)"
                case (let d?, nil): return d
                case (nil, let b?): return b
                case (nil, nil): return nil
                }
            case .ssh, .sftp:
                return (!item.name.isEmpty && item.name != custom) ? item.name : nil
            }
        }
        guard item.kind == .local, let branch = item.gitBranch, !branch.isEmpty else { return nil }
        return branch
    }

    /// 悬停时的快捷键提示：前 9 个是 ⌘1–⌘9，之后是序号。
    public static func shortcutLabel(sidebarIndex: Int) -> String {
        (0...8).contains(sidebarIndex) ? "⌘\(sidebarIndex + 1)" : "\(sidebarIndex + 1)"
    }

    /// 会话颜色在调色板中的下标。配置了 `colorHex` 时调用方直接用它，不经过这里。
    ///
    /// 用 FNV-1a 64 位而不是 Swift 的 `hashValue`/`Hasher`：后者每个进程随机播种，
    /// 同一个会话每次启动颜色都会变，违反"同一个会话每次都是同一个颜色"。
    public static func colorIndex(for item: TabItem, paletteCount: Int) -> Int {
        guard paletteCount > 0 else { return 0 }
        let key = item.profileID ?? item.name
        return Int(fnv1a64(key) % UInt64(paletteCount))
    }

    /// FNV-1a 64 位哈希（按 UTF-8 字节）。跨进程、跨版本稳定。
    public static func fnv1a64(_ string: String) -> UInt64 {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in string.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        return hash
    }

    /// 路径筛选：query 去首尾空白后为空 → 匹配全部；否则只匹配本地且 cwd 已知的标签
    /// （SSH/SFTP 不拿名称冒充路径）。不区分大小写，`\` 与 `/` 视为同一分隔符，子串匹配。
    public static func matchesPathFilter(_ item: TabItem, query: String) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if q.isEmpty { return true }
        guard item.kind == .local, let cwd = item.cwd, !cwd.isEmpty else { return false }
        return normalizeForFilter(cwd).contains(normalizeForFilter(q))
    }

    private static func normalizeForFilter(_ s: String) -> String {
        s.replacingOccurrences(of: "\\", with: "/").lowercased()
    }

    private static func trimTrailingSlashes(_ s: String) -> String {
        var t = Substring(s)
        while t.count > 1, t.hasSuffix("/") { t = t.dropLast() }
        return String(t)
    }
}
