import AppKit
import Foundation
import KansoBridge
import KansoUI

/// 命令面板的一条可选项（四类结果之一）。
struct CommandPaletteHit: Equatable {
    enum Kind: Equatable {
        case tab(SessionID)
        case session(String)
        case command(String)
        case file(path: String)
    }

    let kind: Kind
    let title: String
    /// 副标题或快捷键显示（命令显示快捷键；会话为 user@host:port，画在名称下方；文件可不显示）。
    let detail: String?
    /// 会话色点；仅 `.session` 使用。
    let color: FfiRgb?
    let score: Int

    init(kind: Kind, title: String, detail: String?, score: Int, color: FfiRgb? = nil) {
        self.kind = kind
        self.title = title
        self.detail = detail
        self.color = color
        self.score = score
    }
}

/// 命令面板分组（显示顺序固定）。
enum CommandPaletteSection: Int, CaseIterable, Equatable {
    case tabs
    case sessions
    case commands
    case files

    var title: String {
        switch self {
        case .tabs: String(localized: "Open Tabs")
        case .sessions: String(localized: "Saved Sessions")
        case .commands: String(localized: "Commands")
        case .files: String(localized: "Files")
        }
    }
}

/// 收集结果时用的输入快照；不依赖 AppKit，便于单测。
struct CommandPaletteSnapshot: Equatable, Sendable {
    struct Tab: Equatable, Sendable {
        var id: SessionID
        var title: String
        var detail: String?
    }

    struct Session: Equatable, Sendable {
        var id: String
        var title: String
        var detail: String
        var color: FfiRgb? = nil
    }

    struct Command: Equatable, Sendable {
        var id: String
        var title: String
        var shortcut: String?
    }

    struct File: Equatable, Sendable {
        var name: String
        var path: String
    }

    var tabs: [Tab] = []
    var sessions: [Session] = []
    var commands: [Command] = []
    /// 当前活动本地会话工作目录下一层；SSH / 无 cwd 时为空。
    var files: [File] = []
}

/// 命令面板视图模型：按查询过滤四类结果、键盘选中、执行分发。
@MainActor
final class CommandPaletteModel {
    enum Row: Equatable {
        case header(CommandPaletteSection)
        case hit(CommandPaletteHit)
    }

    /// 执行选中项。由呈现方接线。
    struct Actions {
        var activateTab: (SessionID) -> Void
        var openSession: (String) -> Void
        var runCommand: (String) -> Void
        var revealFile: (String) -> Void
        var dismiss: () -> Void
    }

    private(set) var snapshot = CommandPaletteSnapshot()
    private(set) var rows: [Row] = []
    private(set) var selectedIndex: Int?
    var actions: Actions?
    private let normalize: (String) -> [Character]
    private var tabs: [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.Tab>] = []
    private var sessions: [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.Session>] = []
    private var commands: [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.Command>] = []
    private var files: [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.File>] = []

    init(normalize: @escaping (String) -> [Character] = CommandPaletteFuzzy.normalize) {
        self.normalize = normalize
    }

    var query = "" {
        didSet {
            guard query != oldValue else { return }
            rebuild()
        }
    }

    /// 换一批源数据（打开面板或外部变化时）。
    func replaceSnapshot(_ snapshot: CommandPaletteSnapshot) {
        self.snapshot = snapshot
        tabs = CommandPaletteFuzzy.prepare(snapshot.tabs, text: { $0.title + " " + ($0.detail ?? "") }, normalize: normalize)
        sessions = CommandPaletteFuzzy.prepare(snapshot.sessions, text: { $0.title + " " + $0.detail }, normalize: normalize)
        commands = CommandPaletteFuzzy.prepare(snapshot.commands, text: { $0.title }, normalize: normalize)
        files = CommandPaletteFuzzy.prepare(snapshot.files, text: { $0.name }, normalize: normalize)
        rebuild()
    }

    func replaceFiles(_ files: [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.File>]) {
        self.files = files
        snapshot.files = files.map(\.item)
        rebuild()
    }

    /// 可键盘选中的行下标（只有 hit，不含 header）。
    var selectableIndexes: [Int] {
        rows.enumerated().compactMap { if case .hit = $0.element { $0.offset } else { nil } }
    }

    func moveSelection(by delta: Int) {
        let indexes = selectableIndexes
        guard !indexes.isEmpty else { return }
        let current = selectedIndex.flatMap { indexes.firstIndex(of: $0) } ?? (delta > 0 ? -1 : indexes.count)
        let next = min(max(current + delta, 0), indexes.count - 1)
        selectedIndex = indexes[next]
    }

    func selectHit(at row: Int) {
        guard row >= 0, row < rows.count, case .hit = rows[row] else { return }
        selectedIndex = row
    }

    /// 回车：执行当前选中项并关闭。
    @discardableResult
    func executeSelection() -> Bool {
        guard let selectedIndex, case .hit(let hit) = rows[selectedIndex] else { return false }
        execute(hit)
        return true
    }

    func execute(_ hit: CommandPaletteHit) {
        switch hit.kind {
        case .tab(let id): actions?.activateTab(id)
        case .session(let id): actions?.openSession(id)
        case .command(let id): actions?.runCommand(id)
        case .file(let path): actions?.revealFile(path)
        }
        actions?.dismiss()
    }

    // MARK: - 过滤

    /// 按查询重建分组列表；空查询显示各组全部（仍按组顺序）。
    func rebuild() {
        let needle = normalize(query.trimmingCharacters(in: .whitespacesAndNewlines))
        var next: [Row] = []

        func appendSection(_ section: CommandPaletteSection, hits: [CommandPaletteHit]) {
            guard !hits.isEmpty else { return }
            next.append(.header(section))
            next.append(contentsOf: hits.map { .hit($0) })
        }

        let tabs = CommandPaletteFuzzy.ranked(self.tabs, needle: needle)
            .map { CommandPaletteHit(kind: .tab($0.item.id), title: $0.item.title, detail: $0.item.detail, score: $0.score) }
        appendSection(.tabs, hits: tabs)

        let sessions = CommandPaletteFuzzy.ranked(self.sessions, needle: needle)
            .map {
                CommandPaletteHit(
                    kind: .session($0.item.id), title: $0.item.title, detail: $0.item.detail,
                    score: $0.score, color: $0.item.color
                )
            }
        appendSection(.sessions, hits: sessions)

        let commands = CommandPaletteFuzzy.ranked(self.commands, needle: needle)
            .map {
                CommandPaletteHit(kind: .command($0.item.id), title: $0.item.title, detail: $0.item.shortcut, score: $0.score)
            }
        appendSection(.commands, hits: commands)

        let files = CommandPaletteFuzzy.ranked(self.files, needle: needle)
            .map { CommandPaletteHit(kind: .file(path: $0.item.path), title: $0.item.name, detail: nil, score: $0.score) }
        appendSection(.files, hits: files)

        let previous: CommandPaletteHit? = {
            guard let selectedIndex, selectedIndex < rows.count, case .hit(let hit) = rows[selectedIndex] else { return nil }
            return hit
        }()
        rows = next
        if let previous,
           let idx = rows.firstIndex(where: { if case .hit(let hit) = $0 { hit.kind == previous.kind && hit.title == previous.title } else { false } }) {
            selectedIndex = idx
        } else {
            selectedIndex = selectableIndexes.first
        }
    }
}

/// 从主菜单收集可用菜单项（有 action、未隐藏、校验通过）。
@MainActor
enum CommandPaletteMenuCollector {
    /// 返回 (稳定 id, 标题, 快捷键显示)。id 用于执行时重新查找。
    static func collect(from menu: NSMenu?) -> [CommandPaletteSnapshot.Command] {
        guard let menu else { return [] }
        var result: [CommandPaletteSnapshot.Command] = []
        var path: [String] = []
        walk(menu, path: &path, into: &result)
        return result
    }

    private static func walk(_ menu: NSMenu, path: inout [String], into result: inout [CommandPaletteSnapshot.Command]) {
        for item in menu.items {
            if item.isSeparatorItem || item.isHidden { continue }
            if let submenu = item.submenu {
                path.append(item.title)
                walk(submenu, path: &path, into: &result)
                path.removeLast()
                continue
            }
            guard item.action != nil else { continue }
            guard isEnabled(item) else { continue }
            let id = menuItemID(item, path: path)
            let shortcut = shortcutDisplay(for: item)
            result.append(.init(id: id, title: item.title, shortcut: shortcut))
        }
    }

    /// 稳定 id：优先用 identifier；否则「路径 ▸ 标题」。
    static func menuItemID(_ item: NSMenuItem, path: [String]) -> String {
        if let raw = item.identifier?.rawValue, !raw.isEmpty { return raw }
        return (path + [item.title]).joined(separator: "\u{001F}")
    }

    static func findItem(id: String, in menu: NSMenu?) -> NSMenuItem? {
        guard let menu else { return nil }
        var path: [String] = []
        return find(id: id, in: menu, path: &path)
    }

    private static func find(id: String, in menu: NSMenu, path: inout [String]) -> NSMenuItem? {
        for item in menu.items {
            if item.isSeparatorItem { continue }
            if let submenu = item.submenu {
                path.append(item.title)
                if let found = find(id: id, in: submenu, path: &path) { return found }
                path.removeLast()
                continue
            }
            if menuItemID(item, path: path) == id { return item }
        }
        return nil
    }

    private static func isEnabled(_ item: NSMenuItem) -> Bool {
        guard let action = item.action else { return false }
        let target = item.target ?? NSApp.target(forAction: action, to: nil, from: item)
        if let validator = target as? NSMenuItemValidation {
            return validator.validateMenuItem(item)
        }
        return item.isEnabled
    }

    static func shortcutDisplay(for item: NSMenuItem) -> String? {
        KeyChord(keyEquivalent: item.keyEquivalent, modifierMask: item.keyEquivalentModifierMask)?.display
    }
}

/// 列出目录下一层条目（仅名称）；失败返回空。测试注入替身，不碰真实 HOME。
enum CommandPaletteFileLister {
    static func load(directory: String, list: @escaping @Sendable (String) -> [CommandPaletteSnapshot.File]) async -> [CommandPaletteFuzzy.Candidate<CommandPaletteSnapshot.File>] {
        await Task.detached(priority: .userInitiated) {
            CommandPaletteFuzzy.prepare(list(directory), text: { $0.name })
        }.value
    }

    static func list(directory path: String, using fm: FileManager = .default) -> [CommandPaletteSnapshot.File] {
        guard !path.isEmpty else { return [] }
        let names: [String]
        do {
            names = try fm.contentsOfDirectory(atPath: path)
        } catch {
            return []
        }
        return names.map { name in
            .init(name: name, path: (path as NSString).appendingPathComponent(name))
        }
    }
}
