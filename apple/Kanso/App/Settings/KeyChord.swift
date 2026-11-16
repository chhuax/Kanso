import AppKit

/// 组合键的修饰键。只包含快捷键会用到的四个，且不含 capsLock / fn / numericPad 等 `NSEvent.ModifierFlags` 里的噪声位。
struct ChordModifiers: OptionSet, Hashable {
    let rawValue: Int
    static let control = ChordModifiers(rawValue: 1 << 0)
    static let option = ChordModifiers(rawValue: 1 << 1)
    static let shift = ChordModifiers(rawValue: 1 << 2)
    static let command = ChordModifiers(rawValue: 1 << 3)

    init(rawValue: Int) { self.rawValue = rawValue }

    init(_ flags: NSEvent.ModifierFlags) {
        var m: ChordModifiers = []
        if flags.contains(.control) { m.insert(.control) }
        if flags.contains(.option) { m.insert(.option) }
        if flags.contains(.shift) { m.insert(.shift) }
        if flags.contains(.command) { m.insert(.command) }
        self = m
    }

    var flags: NSEvent.ModifierFlags {
        var f: NSEvent.ModifierFlags = []
        if contains(.control) { f.insert(.control) }
        if contains(.option) { f.insert(.option) }
        if contains(.shift) { f.insert(.shift) }
        if contains(.command) { f.insert(.command) }
        return f
    }

    /// 存储与显示的固定顺序：⌃ ⌥ ⇧ ⌘（macOS 菜单里的惯例顺序）。
    fileprivate static let ordered: [(ChordModifiers, token: String, symbol: String)] = [
        (.control, "ctrl", "⌃"), (.option, "alt", "⌥"), (.shift, "shift", "⇧"), (.command, "cmd", "⌘"),
    ]
}

/// 一个快捷键组合：键 + 修饰键。
///
/// `key` 是规范化的键名：可打印键为单个小写字符（"c"、"\\"、"["），特殊键为名字（"left"、"f5"、"return"……）。
/// 用键名而不是 keyCode：菜单项的 `keyEquivalent` 按字符匹配，键位布局不同的键盘上同一个字符才是用户想要的键。
///
/// 字符取自 `charactersIgnoringModifiers`，它会保留 Shift 的效果（⇧[ 给出 "{"），而菜单项约定用**未按 Shift 的字符**
/// 配合 Shift 修饰位（现有的 ⌘⇧\ 就是 "\\" + shift），所以录制时要把常见的"Shift 后字符"还原成基础字符。
/// 还原表按美式键盘；其他布局上带 Shift 的符号键可能还原不全，但不影响字母、数字与未带 Shift 的符号键。
struct KeyChord: Hashable {
    var key: String
    var modifiers: ChordModifiers

    init(key: String, modifiers: ChordModifiers) {
        self.key = key
        self.modifiers = modifiers
    }

    // MARK: 特殊键

    /// 特殊键：名字 ↔ `NSEvent` 里的功能键字符 / 控制字符，以及显示用的符号。
    private static let specialKeys: [(name: String, char: Character, display: String)] = {
        func fn(_ value: Int) -> Character { Character(UnicodeScalar(UInt32(value))!) }
        var table: [(String, Character, String)] = [
            ("left", fn(NSLeftArrowFunctionKey), "←"),
            ("right", fn(NSRightArrowFunctionKey), "→"),
            ("up", fn(NSUpArrowFunctionKey), "↑"),
            ("down", fn(NSDownArrowFunctionKey), "↓"),
            ("home", fn(NSHomeFunctionKey), "↖"),
            ("end", fn(NSEndFunctionKey), "↘"),
            ("pageup", fn(NSPageUpFunctionKey), "⇞"),
            ("pagedown", fn(NSPageDownFunctionKey), "⇟"),
            ("insert", fn(NSInsertFunctionKey), "Ins"),
            ("forwarddelete", fn(NSDeleteFunctionKey), "⌦"),
            ("return", "\r", "↩"),
            ("tab", "\t", "⇥"),
            ("space", " ", "Space"),
        ]
        for n in 1...20 { table.append(("f\(n)", fn(NSF1FunctionKey + n - 1), "F\(n)")) }
        return table
    }()

    /// 功能键 / 导航键（方向键、F 键、Insert、Home 等）。它们没有可打印字符，所以与"可打印键"的绑定门槛不同。
    /// Space / Return / Tab 虽然也在特殊键表里（要有名字才能存储与显示），但敲它们会输入内容，按可打印键对待。
    var isNavigationOrFunctionKey: Bool {
        isSpecial && !["return", "tab", "space"].contains(key)
    }

    /// 美式键盘上 Shift 后的符号 → 基础字符。
    private static let unshifted: [Character: Character] = [
        "{": "[", "}": "]", "|": "\\", ":": ";", "\"": "'", "<": ",", ">": ".", "?": "/", "~": "`", "_": "-", "+": "=",
        "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0",
    ]

    /// 是否特殊键（功能键、方向键等，没有可打印字符）。
    var isSpecial: Bool { Self.specialKeys.contains { $0.name == key } }

    /// 是否字母键（a–z）。
    var isLetter: Bool { key.count == 1 && key.unicodeScalars.first.map { $0.value >= 97 && $0.value <= 122 } == true }

    /// "普通的 Ctrl+字母"：只有 Ctrl 修饰的字母键。它们是 shell 的控制字符（terminal.md §9），
    /// 除非用户主动绑定否则永远不拦截；被绑定为复制键时有特殊规则（terminal.md §8.2）。
    var isPlainCtrlLetter: Bool { modifiers == .control && isLetter }

    // MARK: 事件 ↔ 组合键

    /// 由按键事件构造；事件没有字符（纯修饰键等）时为 nil。
    init?(event: NSEvent) {
        guard event.type == .keyDown || event.type == .keyUp,
              let raw = event.charactersIgnoringModifiers, let first = raw.first else { return nil }
        let mods = ChordModifiers(event.modifierFlags)
        var char = Character(first.lowercased())
        if mods.contains(.shift), let base = Self.unshifted[char] { char = base }
        if let special = Self.specialKeys.first(where: { $0.char == char }) {
            self.init(key: special.name, modifiers: mods)
        } else {
            self.init(key: String(char), modifiers: mods)
        }
    }

    /// 事件是否匹配本组合。
    func matches(_ event: NSEvent) -> Bool {
        KeyChord(event: event) == self
    }

    /// 能不能作为绑定：可打印键（含 Space / Return / Tab）必须带 ⌘ / ⌃ / ⌥ 之一，只带 Shift 或不带修饰键会让输入文字变成快捷键；
    /// 功能键 / 导航键至少带一个修饰键（用户常用 Shift+Insert / Ctrl+Insert 作粘贴 / 复制，但不带修饰键的方向键属于终端程序）。
    var isValidBinding: Bool {
        if isNavigationOrFunctionKey { return !modifiers.isEmpty }
        return !modifiers.isDisjoint(with: [.command, .control, .option])
    }

    // MARK: 菜单项

    /// 菜单项的 `keyEquivalent`。
    var menuKeyEquivalent: String {
        if let special = Self.specialKeys.first(where: { $0.name == key }) { return String(special.char) }
        return key
    }

    /// 由菜单项的 `keyEquivalent` + 修饰键构造；空快捷键返回 nil。
    /// 与设置页共用同一套特殊键表，保证命令面板等处的显示与 `display` 一致。
    init?(keyEquivalent: String, modifierMask: NSEvent.ModifierFlags) {
        guard !keyEquivalent.isEmpty, let first = keyEquivalent.first else { return nil }
        let mods = ChordModifiers(modifierMask)
        if let special = Self.specialKeys.first(where: { $0.char == first }) {
            self.init(key: special.name, modifiers: mods)
        } else {
            self.init(key: String(first).lowercased(), modifiers: mods)
        }
    }

    // MARK: 存储与显示

    /// 存储文本，如 `cmd+shift+\`、`ctrl+insert`。修饰键在前、键在最后（键本身可以是 `+`，解析时靠修饰键白名单区分）。
    var storage: String {
        let mods = ChordModifiers.ordered.filter { modifiers.contains($0.0) }.map { $0.token + "+" }.joined()
        return mods + key
    }

    /// 解析存储文本；格式不对返回 nil（调用方回落默认值）。
    init?(storage: String) {
        var rest = Substring(storage)
        var mods: ChordModifiers = []
        outer: while true {
            for (modifier, token, _) in ChordModifiers.ordered where rest.hasPrefix(token + "+") && rest.count > token.count + 1 {
                mods.insert(modifier)
                rest = rest.dropFirst(token.count + 1)
                continue outer
            }
            break
        }
        let key = String(rest)
        let isSpecial = Self.specialKeys.contains { $0.name == key }
        guard isSpecial || (key.count == 1 && key == key.lowercased()) else { return nil }
        self.init(key: key, modifiers: mods)
    }

    /// 界面显示，如 `⌘⇧\`、`⌃Ins`。
    var display: String {
        let mods = ChordModifiers.ordered.filter { modifiers.contains($0.0) }.map(\.symbol).joined()
        let keyText = Self.specialKeys.first { $0.name == key }?.display ?? key.uppercased()
        return mods + keyText
    }
}
