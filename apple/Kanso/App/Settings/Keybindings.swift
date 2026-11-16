import AppKit
import TerminalKit

/// 快捷键对话框里的分组（与菜单一致）。
enum KeyCommandSection: CaseIterable {
    case session
    case edit
    case view

    var title: String {
        switch self {
        case .session: String(localized: "Session")
        case .edit: String(localized: "Edit")
        case .view: String(localized: "View")
        }
    }

    var symbol: String {
        switch self {
        case .session: "rectangle.stack"
        case .edit: "pencil"
        case .view: "sidebar.left"
        }
    }
}

/// 可改绑的命令（ui-shell.md §7）。每个命令对应主菜单里的一个菜单项。
///
/// 只列已经实现的菜单命令。
/// ⌘1–⌘9 固定不可改，不在此枚举里。`rawValue` 会写进用户设置，发布后不得改名。
enum KeyCommand: String, CaseIterable {
    case newSession
    case newLocalShell
    case previousSession
    case nextSession
    case previousSplit
    case nextSplit
    case closeSession
    case copy
    case paste
    case selectAll
    case clearBuffer
    case previousCommandBlock
    case nextCommandBlock
    case find
    case findNext
    case findPrevious
    case commandSuggestions
    case clearCommandHistory
    case toggleTabBar
    case toggleFiler
    case toggleSender
    case locateWorkingDirectory
    case commandPalette
    case splitRight
    case splitDown

    var section: KeyCommandSection {
        switch self {
        case .newSession, .newLocalShell, .previousSession, .nextSession, .previousSplit, .nextSplit, .closeSession:
            .session
        case .copy, .paste, .selectAll, .clearBuffer, .previousCommandBlock, .nextCommandBlock, .find, .findNext, .findPrevious, .commandSuggestions, .clearCommandHistory:
            .edit
        case .toggleTabBar, .toggleFiler, .toggleSender, .locateWorkingDirectory, .commandPalette, .splitRight, .splitDown:
            .view
        }
    }

    var title: String {
        switch self {
        case .toggleSender: String(localized: "Toggle Sender")
        case .newSession: String(localized: "New Session…")
        case .newLocalShell: String(localized: "New Local Shell")
        case .previousSession: String(localized: "Previous Session")
        case .nextSession: String(localized: "Next Session")
        case .previousSplit: String(localized: "Previous Split")
        case .nextSplit: String(localized: "Next Split")
        case .closeSession: String(localized: "Close Session")
        case .copy: String(localized: "Copy")
        case .paste: String(localized: "Paste")
        case .selectAll: String(localized: "Select All")
        case .clearBuffer: String(localized: "Clear Buffer")
        case .previousCommandBlock: String(localized: "Previous Command Block")
        case .nextCommandBlock: String(localized: "Next Command Block")
        case .find: String(localized: "Find…")
        case .findNext: String(localized: "Find Next")
        case .findPrevious: String(localized: "Find Previous")
        case .commandSuggestions: String(localized: "Command Suggestions")
        case .clearCommandHistory: String(localized: "Clear Command History…")
        case .toggleTabBar: String(localized: "Toggle Tab Bar")
        case .toggleFiler: String(localized: "Toggle File Browser")
        case .locateWorkingDirectory: String(localized: "Show Working Directory in File Browser")
        case .commandPalette: String(localized: "Command Palette")
        case .splitRight: String(localized: "Split Right")
        case .splitDown: String(localized: "Split Down")
        }
    }

    /// 菜单项的动作。复制 / 粘贴 / 全选用标准 selector，由 first responder（终端视图）接住。
    var action: Selector {
        switch self {
        case .toggleSender: #selector(MainWindowController.kansoToggleSender(_:))
        case .newSession: #selector(MainWindowController.kansoNewSession(_:))
        case .newLocalShell: #selector(MainWindowController.kansoNewLocalShell(_:))
        case .previousSession: #selector(MainWindowController.kansoPreviousSession(_:))
        case .nextSession: #selector(MainWindowController.kansoNextSession(_:))
        case .previousSplit: #selector(MainWindowController.kansoPreviousSplit(_:))
        case .nextSplit: #selector(MainWindowController.kansoNextSplit(_:))
        case .closeSession: #selector(MainWindowController.kansoCloseSession(_:))
        case .copy: #selector(NSText.copy(_:))
        case .paste: #selector(NSText.paste(_:))
        case .selectAll: #selector(NSText.selectAll(_:))
        case .clearBuffer: #selector(MainWindowController.kansoClearBuffer(_:))
        case .previousCommandBlock: #selector(TerminalView.previousCommandBlock(_:))
        case .nextCommandBlock: #selector(TerminalView.nextCommandBlock(_:))
        case .find: #selector(MainWindowController.kansoFind(_:))
        case .findNext: #selector(MainWindowController.kansoFindNext(_:))
        case .findPrevious: #selector(MainWindowController.kansoFindPrevious(_:))
        case .commandSuggestions: #selector(MainWindowController.kansoCommandSuggestions(_:))
        case .clearCommandHistory: #selector(MainWindowController.kansoClearCommandHistory(_:))
        case .toggleTabBar: #selector(MainWindowController.kansoToggleTabBar(_:))
        case .toggleFiler: #selector(MainWindowController.kansoToggleFiler(_:))
        case .locateWorkingDirectory: #selector(MainWindowController.kansoLocateWorkingDirectory(_:))
        case .commandPalette: #selector(MainWindowController.kansoCommandPalette(_:))
        case .splitRight: #selector(MainWindowController.kansoSplitRight(_:))
        case .splitDown: #selector(MainWindowController.kansoSplitDown(_:))
        }
    }

    /// 默认组合键（macOS 列，ui-shell.md §7）。
    var defaultChord: KeyChord? {
        let cmd: ChordModifiers = .command
        switch self {
        case .newSession: return KeyChord(key: "n", modifiers: cmd)
        case .toggleSender: return KeyChord(key: "down", modifiers: [.command, .option])
        case .newLocalShell: return KeyChord(key: "t", modifiers: cmd)
        case .previousSession: return KeyChord(key: "[", modifiers: cmd)
        case .nextSession: return KeyChord(key: "]", modifiers: cmd)
        case .previousSplit: return KeyChord(key: "[", modifiers: [.command, .option])
        case .nextSplit: return KeyChord(key: "]", modifiers: [.command, .option])
        case .closeSession: return KeyChord(key: "w", modifiers: cmd)
        case .copy: return KeyChord(key: "c", modifiers: cmd)
        case .paste: return KeyChord(key: "v", modifiers: cmd)
        case .selectAll: return KeyChord(key: "a", modifiers: cmd)
        case .clearBuffer: return KeyChord(key: "k", modifiers: cmd)
        case .previousCommandBlock: return KeyChord(key: "up", modifiers: cmd)
        case .nextCommandBlock: return KeyChord(key: "down", modifiers: cmd)
        case .find: return KeyChord(key: "f", modifiers: cmd)
        case .findNext: return KeyChord(key: "g", modifiers: cmd)
        case .findPrevious: return KeyChord(key: "g", modifiers: [.command, .shift])
        case .commandSuggestions, .clearCommandHistory: return nil
        case .toggleTabBar: return KeyChord(key: "left", modifiers: [.command, .option])
        case .toggleFiler: return KeyChord(key: "right", modifiers: [.command, .option])
        case .locateWorkingDirectory: return KeyChord(key: "j", modifiers: cmd)
        case .commandPalette: return KeyChord(key: "p", modifiers: [.command, .shift])
        case .splitRight: return KeyChord(key: "\\", modifiers: cmd)
        case .splitDown: return KeyChord(key: "\\", modifiers: [.command, .shift])
        }
    }

    /// 主菜单里对应菜单项的标识，`AppDelegate.makeMainMenu` 设置、`KeybindingController` 据此改写按键。
    var menuIdentifier: NSUserInterfaceItemIdentifier {
        NSUserInterfaceItemIdentifier("kanso.command.\(rawValue)")
    }

    init?(menuIdentifier: NSUserInterfaceItemIdentifier?) {
        let prefix = "kanso.command."
        guard let raw = menuIdentifier?.rawValue, raw.hasPrefix(prefix) else { return nil }
        self.init(rawValue: String(raw.dropFirst(prefix.count)))
    }
}

/// 快捷键的读取、改绑与生效。
///
/// 生效机制：改写主菜单项的 `keyEquivalent` / `keyEquivalentModifierMask`。原来的默认键因此自然失效
/// （菜单项只有一组按键，不会新旧同时生效，ui-shell.md §7）；清空则菜单项没有按键。
/// 设置变化（改绑、恢复默认、设置载入）后自动重新应用。
///
/// 没有菜单项的命令需要在窗口层面拦截——目前列出的命令全部有菜单项，所以没有这条路径；
/// 新增无菜单项的命令时再加。
@MainActor
final class KeybindingController {
    /// 绑定结果。
    enum AssignResult: Equatable {
        /// 已绑定（含"与当前相同"）。
        case assigned
        /// 与另一个命令的当前按键相同；调用方询问用户后用 `assignReplacingConflict` 继续，或放弃。
        case conflict(KeyCommand)
        /// 系统保留或固定的组合（⌘1–⌘9、⌘Q、⌘H、⌘M、⌘,）。
        case reserved
        /// 不能作为绑定（缺少修饰键等，见 `KeyChord.isValidBinding`）。
        case invalid
    }

    /// 不允许改绑到的组合：⌘1–⌘9 固定为切换标签（ui-shell.md §7），其余是应用 / 系统菜单的标准键。
    static let reservedChords: Set<KeyChord> = {
        var set = Set((1...9).map { KeyChord(key: String($0), modifiers: .command) })
        set.formUnion([
            KeyChord(key: "q", modifiers: .command),
            KeyChord(key: "h", modifiers: .command),
            KeyChord(key: "h", modifiers: [.command, .option]),
            KeyChord(key: "m", modifiers: .command),
            KeyChord(key: ",", modifiers: .command),
        ])
        return set
    }()

    let settings: SettingsStore
    private let menuProvider: () -> NSMenu?
    private let copySelection: (TerminalView) -> Void

    /// - Parameters:
    ///   - menuProvider: 要改写按键的主菜单；默认 `NSApp.mainMenu`，测试注入自己构造的菜单（不碰全局状态）。
    ///   - copySelection: 执行复制的动作；默认调终端视图的 `copy(_:)`（写系统剪贴板）。测试注入记录闭包，不碰用户剪贴板。
    init(
        settings: SettingsStore,
        menuProvider: @escaping () -> NSMenu? = { NSApp.mainMenu },
        copySelection: @escaping (TerminalView) -> Void = { $0.copy(nil) }
    ) {
        self.settings = settings
        self.menuProvider = menuProvider
        self.copySelection = copySelection
        settings.observe(self) { [weak self] section in
            if section == .keybindings { self?.applyToMenu() }
        }
    }

    // MARK: 读取

    /// 命令当前生效的组合；用户清空为 nil。存储里的组合无法解析时回落默认值（设置可能来自其他版本）。
    func chord(for command: KeyCommand) -> KeyChord? {
        guard let stored = settings.keybindings.overrides[command.rawValue] else { return command.defaultChord }
        if stored.isEmpty { return nil }
        return KeyChord(storage: stored) ?? command.defaultChord
    }

    /// 当前绑定了该组合的命令（不含 `excluding`）。
    func command(for chord: KeyChord, excluding: KeyCommand? = nil) -> KeyCommand? {
        KeyCommand.allCases.first { $0 != excluding && self.chord(for: $0) == chord }
    }

    /// 事件匹配到的命令与组合。
    func match(_ event: NSEvent) -> (command: KeyCommand, chord: KeyChord)? {
        guard let chord = KeyChord(event: event), let command = command(for: chord) else { return nil }
        return (command, chord)
    }

    // MARK: 修改

    /// 尝试绑定。冲突时不修改，返回 `.conflict` 让调用方决定。
    func assign(_ chord: KeyChord, to command: KeyCommand) -> AssignResult {
        guard chord.isValidBinding else { return .invalid }
        if Self.reservedChords.contains(chord) { return .reserved }
        if let other = self.command(for: chord, excluding: command) { return .conflict(other) }
        setChord(chord, for: command)
        return .assigned
    }

    /// 用户确认替换：先清空占用该组合的命令，再绑定。其余校验同 `assign`。
    func assignReplacingConflict(_ chord: KeyChord, to command: KeyCommand) -> AssignResult {
        guard chord.isValidBinding else { return .invalid }
        if Self.reservedChords.contains(chord) { return .reserved }
        // 清空与绑定在同一次设置更新里完成：观察者只看到最终状态，菜单不会出现两个命令同键的中间态。
        settings.updateKeybindings { settings in
            if let other = self.command(for: chord, excluding: command) {
                Self.store(nil, for: other, in: &settings)
            }
            Self.store(chord, for: command, in: &settings)
        }
        return .assigned
    }

    /// 清空：命令不绑定任何键。
    func clear(_ command: KeyCommand) {
        setChord(nil, for: command)
    }

    /// 恢复整张快捷键表为默认值。
    func restoreDefaults() {
        settings.updateKeybindings { $0.overrides = [:] }
    }

    private func setChord(_ chord: KeyChord?, for command: KeyCommand) {
        settings.updateKeybindings { Self.store(chord, for: command, in: &$0) }
    }

    /// 只存与默认值不同的项（见 `KeybindingSettings`）。
    private static func store(_ chord: KeyChord?, for command: KeyCommand, in settings: inout KeybindingSettings) {
        if chord == command.defaultChord {
            settings.overrides[command.rawValue] = nil
        } else {
            settings.overrides[command.rawValue] = chord?.storage ?? ""
        }
    }

    // MARK: 生效

    /// 把当前绑定写进主菜单项。幂等，可随时调用（菜单构造之后、设置变化之后）。
    func applyToMenu() {
        guard let menu = menuProvider() else { return }
        apply(to: menu)
    }

    private func apply(to menu: NSMenu) {
        for item in menu.items {
            if let command = KeyCommand(menuIdentifier: item.identifier) { apply(command, to: item) }
            if let submenu = item.submenu { apply(to: submenu) }
        }
    }

    private func apply(_ command: KeyCommand, to item: NSMenuItem) {
        // 普通 Ctrl+字母绑定为复制键时不写进菜单：菜单会在窗口最前面拦截它，没有选区时就无法"照常发给程序"
        // （Ctrl+C 要继续能中断程序，terminal.md §8.2）。这种绑定由终端视图的按键拦截点处理，见 `interceptTerminalKey`。
        if let chord = chord(for: command), !Self.isTerminalHandledCopy(command, chord) {
            item.keyEquivalent = chord.menuKeyEquivalent
            item.keyEquivalentModifierMask = chord.modifiers.flags
        } else {
            item.keyEquivalent = ""
            item.keyEquivalentModifierMask = []
        }
    }

    private static func isTerminalHandledCopy(_ command: KeyCommand, _ chord: KeyChord) -> Bool {
        command == .copy && chord.isPlainCtrlLetter
    }

    // MARK: 终端按键拦截

    /// 终端视图的按键拦截点（`TerminalView.keyInterceptor`）。返回 true 表示按键已处理，不再发给 shell。
    ///
    /// - 事件匹配到某个命令的当前绑定、而菜单没有认领它（菜单项当前不可用，例如没有选区时的复制），
    ///   也要吞掉：否则 Ctrl+Shift+C 之类的组合会变成多余的控制字符发给程序（terminal.md §8.2）。
    /// - 特殊规则：复制键是普通 Ctrl+字母时，有选区就复制并吞掉；**没有选区就照常发给程序**（返回 false），
    ///   所以 Ctrl+C 仍然能中断程序。
    /// - 没有匹配任何绑定的按键一律不拦截，所以普通 Ctrl/Alt+字母永远不会被吞（terminal.md §9）。
    func interceptTerminalKey(_ event: NSEvent, in view: TerminalView?) -> Bool {
        guard let (command, chord) = match(event) else { return false }
        if command == .previousCommandBlock { view?.previousCommandBlock(nil); return true }
        if command == .nextCommandBlock { view?.nextCommandBlock(nil); return true }
        if Self.isTerminalHandledCopy(command, chord) {
            guard let view, view.editActions?.hasSelection == true else { return false }
            copySelection(view)
            return true
        }
        return true
    }
}
