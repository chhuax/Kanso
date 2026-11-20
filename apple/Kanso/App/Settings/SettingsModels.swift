import Foundation
import KansoBridge
import TerminalKit

// 设置的数据结构（ui-shell.md §10、store-import.md §9）。
//
// 存储方式：每个分区是 core 设置表里的一个 key，值是 JSON 对象。所有分区都遵守同一套兼容规则：
// - **带版本号**：写入时带 `"v"`；
// - **未知字段忽略**：新版本写入的字段旧版本读到也不报错（JSONDecoder 本身忽略未知键，下面的自定义解码也只按已知键取值）；
// - **缺字段用默认值**、**字段类型不对用默认值**：设置文件可能来自 `.kanso` 导入或其他版本，
//   任何一个字段出问题都不能让整个分区（更不能让应用）失败。
// 版本号目前只用于将来需要迁移时判断；读取不因版本号比自己新而拒绝。

/// 设置分区，对应 core 设置表里的 key。key 发布后不得改名（改名等于丢失用户设置）。
enum SettingsSection: String, CaseIterable {
    /// 终端显示：字体、字号、光标、回滚、最低对比度。
    case display
    /// 外观：外观模式、浅色 / 深色各自的终端配色、标签栏位置。
    case appearance
    /// 时间戳 / 行号栏模式。
    case timeline
    /// 快捷键改绑。
    case keybindings
    /// 命令建议及历史采集。
    case completion
    /// 鼠标：中键、右键行为与选中即复制。
    case mouse
}

struct CompletionSettings: Codable, Equatable {
    var v = SettingsCodec.version
    var enabled = false

    init() {}
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        enabled = (try? values.decode(Bool.self, forKey: .enabled)) ?? false
    }
}

/// 中键行为（terminal.md §8.1）。raw 值写入设置文件，发布后不得改名。
enum MiddleClickAction: String, CaseIterable {
    case paste
    // 不叫 `none`：会和 `Optional.none` 撞名，`optional == .none` 会被悄悄当成判空。
    case nothing = "none"
}

/// 右键行为（terminal.md §8.1）。raw 值写入设置文件，发布后不得改名。
enum RightClickAction: String, CaseIterable {
    case menu
    case copyOrPaste
}

/// 鼠标设置。未知枚举值回落默认，与其他分区的宽松解码规则一致。
struct MouseSettings: Equatable, Codable {
    var middleClick: MiddleClickAction = .paste
    var rightClick: RightClickAction = .menu
    var copyOnSelect = false

    init() {}

    private enum CodingKeys: String, CodingKey {
        case v, middleClick, rightClick, copyOnSelect
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = MouseSettings()
        middleClick = MiddleClickAction(rawValue: c.lenient(.middleClick, default: "")) ?? defaults.middleClick
        rightClick = RightClickAction(rawValue: c.lenient(.rightClick, default: "")) ?? defaults.rightClick
        copyOnSelect = c.lenient(.copyOnSelect, default: defaults.copyOnSelect)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(SettingsCodec.version, forKey: .v)
        try c.encode(middleClick.rawValue, forKey: .middleClick)
        try c.encode(rightClick.rawValue, forKey: .rightClick)
        try c.encode(copyOnSelect, forKey: .copyOnSelect)
    }
}

/// 设置分区的通用编解码规则。
enum SettingsCodec {
    /// 当前写入的版本号。
    static let version = 1

    static func decode<T: Decodable>(_ type: T.Type, from json: String?) -> T? {
        guard let data = json?.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    static func encode<T: Encodable>(_ value: T) -> String {
        let encoder = JSONEncoder()
        // 固定键序：同样的设置产生同样的文本，导出文件与测试断言都更稳定。
        encoder.outputFormatting = [.sortedKeys]
        // 编码自有的纯值类型不会失败；万一失败写空对象，读取时全部回落默认值，好过丢掉写入动作。
        guard let data = try? encoder.encode(value), let text = String(data: data, encoding: .utf8) else { return "{}" }
        return text
    }
}

extension KeyedDecodingContainer {
    /// 宽松取值：键缺失或类型不对都返回默认值，不抛错。
    func lenient<T: Decodable>(_ key: Key, default value: T) -> T {
        ((try? decodeIfPresent(T.self, forKey: key)) ?? nil) ?? value
    }
}

// MARK: - 显示

/// 默认光标形状（与 core 的 `FfiCursorShape` 对应）。
enum CursorShapeSetting: String, CaseIterable {
    case block
    case underline
    case bar

    var ffiShape: FfiCursorShape {
        switch self {
        case .block: .block
        case .underline: .underline
        case .bar: .bar
        }
    }

    var displayName: String {
        switch self {
        case .block: String(localized: "Block")
        case .underline: String(localized: "Underline")
        case .bar: String(localized: "Bar")
        }
    }
}

/// 终端显示设置。所有范围在解码时夹取：设置文件里的越界值（手改、其他版本）不应让界面出现不可用的字号。
struct DisplaySettings: Equatable, Codable {
    static let fontSizeRange: ClosedRange<Double> = 9...36
    static let lineHeightRange: ClosedRange<Double> = 1...2
    static let scrollbackRange: ClosedRange<Int> = 1_000...1_000_000
    static let contrastRange: ClosedRange<Double> = 1...21

    /// 终端字体家族名；空串表示系统等宽字体。
    var fontFamily = ""
    var fontSize: Double = 14
    var lineHeight: Double = 1.2
    var cursorShape: CursorShapeSetting = .bar
    var cursorBlink = true
    /// 默认值与 core 创建会话时的初始回滚行数一致（`TerminalOptions::DEFAULT_SCROLLBACK_LINES`）。
    var scrollbackLines = 10_000
    var minimumContrastEnabled = true
    var semanticColoring = true
    var copyOnSelect = false
    var minimumContrastRatio: Double = 4.5

    init() {}

    private enum CodingKeys: String, CodingKey {
        case v, fontFamily, fontSize, lineHeight, cursorShape, cursorBlink, scrollbackLines, minimumContrastEnabled, minimumContrastRatio, semanticColoring, copyOnSelect
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = DisplaySettings()
        fontFamily = c.lenient(.fontFamily, default: defaults.fontFamily)
        fontSize = Self.clamp(c.lenient(.fontSize, default: defaults.fontSize), Self.fontSizeRange, fallback: defaults.fontSize)
        lineHeight = Self.clamp(c.lenient(.lineHeight, default: defaults.lineHeight), Self.lineHeightRange, fallback: defaults.lineHeight)
        cursorShape = CursorShapeSetting(rawValue: c.lenient(.cursorShape, default: "")) ?? defaults.cursorShape
        cursorBlink = c.lenient(.cursorBlink, default: defaults.cursorBlink)
        let lines = c.lenient(.scrollbackLines, default: defaults.scrollbackLines)
        scrollbackLines = min(max(lines, Self.scrollbackRange.lowerBound), Self.scrollbackRange.upperBound)
        minimumContrastEnabled = c.lenient(.minimumContrastEnabled, default: defaults.minimumContrastEnabled)
        semanticColoring = c.lenient(.semanticColoring, default: defaults.semanticColoring)
        copyOnSelect = c.lenient(.copyOnSelect, default: defaults.copyOnSelect)
        minimumContrastRatio = Self.clamp(
            c.lenient(.minimumContrastRatio, default: defaults.minimumContrastRatio), Self.contrastRange,
            fallback: defaults.minimumContrastRatio
        )
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(SettingsCodec.version, forKey: .v)
        try c.encode(fontFamily, forKey: .fontFamily)
        try c.encode(fontSize, forKey: .fontSize)
        try c.encode(lineHeight, forKey: .lineHeight)
        try c.encode(cursorShape.rawValue, forKey: .cursorShape)
        try c.encode(cursorBlink, forKey: .cursorBlink)
        try c.encode(scrollbackLines, forKey: .scrollbackLines)
        try c.encode(minimumContrastEnabled, forKey: .minimumContrastEnabled)
        try c.encode(semanticColoring, forKey: .semanticColoring)
        try c.encode(copyOnSelect, forKey: .copyOnSelect)
        try c.encode(minimumContrastRatio, forKey: .minimumContrastRatio)
    }

    /// NaN / 无穷（手改 JSON 或损坏数据）退回默认值，其余夹进范围。
    private static func clamp(_ value: Double, _ range: ClosedRange<Double>, fallback: Double) -> Double {
        guard value.isFinite else { return fallback }
        return min(max(value, range.lowerBound), range.upperBound)
    }

    /// 下发给 core 的终端选项（回滚上限、默认光标样式）。
    var terminalOptions: FfiTerminalOptions {
        FfiTerminalOptions(
            scrollbackLines: UInt32(scrollbackLines), cursorShape: cursorShape.ffiShape, cursorBlink: cursorBlink,
            semanticColoring: semanticColoring
        )
    }
}

// MARK: - 外观

/// 标签栏位置（ui-shell.md §2.5）。存储值是 rawValue，GPUI 端用同样的字符串，`.kanso` 导出跨平台可读。
enum TabBarPosition: String, CaseIterable {
    /// 左侧竖向标签栏（默认）。
    case left
    /// 每个分屏顶部各一条横向标签条。
    case top

    var displayName: String {
        switch self {
        case .left: String(localized: "Left")
        case .top: String(localized: "Top")
        }
    }
}

/// 外观设置：外观模式、浅色 / 深色外观各自使用的终端配色、标签栏位置。
struct AppearanceSettings: Equatable, Codable {
    var mode: AppearanceMode = .system
    /// 终端配色 id（`TerminalTheme.id`）。未知 id 在使用处回落到该外观的默认配色，这里原样保留，
    /// 这样新版本写入的配色 id 经旧版本读写一轮也不会丢。
    var lightTheme = TerminalTheme.kansoLight.id
    var darkTheme = TerminalTheme.kansoDark.id
    var tabBarPosition: TabBarPosition = .left

    init() {}

    private enum CodingKeys: String, CodingKey {
        case v, mode, lightTheme, darkTheme, tabBarPosition
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = AppearanceSettings()
        mode = AppearanceMode(rawValue: c.lenient(.mode, default: "")) ?? defaults.mode
        lightTheme = c.lenient(.lightTheme, default: defaults.lightTheme)
        darkTheme = c.lenient(.darkTheme, default: defaults.darkTheme)
        // 旧版本写入的设置没有这个字段，未知值（更新版本新增的位置）同样回落到左侧。
        tabBarPosition = TabBarPosition(rawValue: c.lenient(.tabBarPosition, default: "")) ?? defaults.tabBarPosition
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(SettingsCodec.version, forKey: .v)
        try c.encode(mode.rawValue, forKey: .mode)
        try c.encode(lightTheme, forKey: .lightTheme)
        try c.encode(darkTheme, forKey: .darkTheme)
        try c.encode(tabBarPosition.rawValue, forKey: .tabBarPosition)
    }

    /// 浅色外观的实际配色（未知 id 回落默认）。
    func resolvedLightTheme(in themes: [TerminalTheme]) -> TerminalTheme {
        TerminalTheme.theme(id: lightTheme, in: themes) ?? .kansoLight
    }
    /// 深色外观的实际配色。
    func resolvedDarkTheme(in themes: [TerminalTheme]) -> TerminalTheme {
        TerminalTheme.theme(id: darkTheme, in: themes) ?? .kansoDark
    }
}

// MARK: - 时间轴

/// 时间戳 / 行号栏设置。
struct TimelineSettings: Equatable, Codable {
    /// 默认关闭（产品决定 2026-11-16）；打开时恢复 `lastVisible`。
    var mode: GutterMode = .off
    /// "时间轴"开关重新打开时恢复的模式：`mode` 为 off 时无法得知关闭前是什么，所以单独存。
    var lastVisible: GutterMode = .both

    init() {}

    private enum CodingKeys: String, CodingKey {
        case v, mode, lastVisible
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = TimelineSettings()
        mode = GutterMode(storageValue: c.lenient(.mode, default: "")) ?? defaults.mode
        // lastVisible 不能是 off：off 不是"可恢复的可见模式"。
        let last = GutterMode(storageValue: c.lenient(.lastVisible, default: "")) ?? defaults.lastVisible
        lastVisible = last == .off ? defaults.lastVisible : last
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(SettingsCodec.version, forKey: .v)
        try c.encode(mode.storageValue, forKey: .mode)
        try c.encode(lastVisible.storageValue, forKey: .lastVisible)
    }
}

// MARK: - 快捷键

/// 快捷键改绑。只存与默认值不同的项：键为命令 id，值为组合键文本（`KeyChord.storage`），空串表示用户清空（不绑定任何键）。
/// 没有出现的命令使用默认值；这样新版本新增命令时，旧设置不会把它们当成"被清空"。
struct KeybindingSettings: Equatable, Codable {
    var overrides: [String: String] = [:]

    init() {}

    private enum CodingKeys: String, CodingKey {
        case v, bindings
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        overrides = c.lenient(.bindings, default: [:])
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(SettingsCodec.version, forKey: .v)
        try c.encode(overrides, forKey: .bindings)
    }
}
