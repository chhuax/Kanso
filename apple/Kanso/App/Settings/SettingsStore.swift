import Foundation
import KansoBridge
import TerminalKit
import os

/// 设置的持久化后端：core 的设置表加终端选项下发。
///
/// 抽成协议只是为了让设置逻辑不依赖具体的 `KansoCore`；测试直接用 mock core 或内存实现。
/// 方法与 `KansoCore` 的 FFI 签名一致，所以 `KansoCore` 零代码即可满足它。
protocol SettingsBackend: AnyObject {
    func setting(key: String) -> String?
    func setSetting(key: String, json: String) throws
    func resetSettings() throws
    func setTerminalOptions(options: FfiTerminalOptions)
}

extension KansoCore: SettingsBackend {}

/// 全部设置的内存镜像 + 持久化 + 变更分发。
///
/// 设置存在 core 里（这样 `.kanso` 导出带得上设置，store-import.md §6.1），但 core 要等到主窗口创建时才存在，
/// 而菜单、设置窗口等更早就要读设置。所以分两个阶段：
/// 1. 创建时没有后端：只在内存里保存默认值，修改也只改内存；
/// 2. `attach(backend:)`：从后端载入（首次启动时把旧的 `UserDefaults` 偏好迁移进去），通知所有观察者，
///    之后每次修改同步写回后端。
/// 之后以后端为准；`UserDefaults` 里的旧 key 迁移后立即删除（见 `migrateLegacyDefaults`）。
///
/// 只在主线程使用。
@MainActor
final class SettingsStore {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "settings"
    )

    /// 迁移用的旧 `UserDefaults` key（F7 之前外观与时间轴模式存在这里）。
    enum LegacyKey {
        static let appearanceMode = "kanso.appearanceMode"
        static let gutterMode = "kanso.gutterMode"
        static let gutterLastVisible = "kanso.gutterMode.lastVisible"
    }

    private var backend: SettingsBackend?
    private(set) var themesDirectory: URL?
    private var themeFiles: [FfiTerminalTheme] = []
    private(set) var themes = TerminalTheme.builtIn

    /// 启动、打开设置或展开配色菜单时扫描。同一文件改色后也要立刻刷新已有终端。
    func reloadThemes() {
        let files = themesDirectory.map { loadTerminalThemes(directory: $0.path) } ?? []
        guard files != themeFiles else { return }
        updateThemes(files)
        notify(.appearance)
    }

    private func updateThemes(_ files: [FfiTerminalTheme]) {
        themeFiles = files
        themes = TerminalTheme.builtIn + files.map(TerminalTheme.fromFile)
    }

    private(set) var display = DisplaySettings()
    private(set) var appearance = AppearanceSettings()
    private(set) var timeline = TimelineSettings()
    private(set) var keybindings = KeybindingSettings()
    private(set) var completion = CompletionSettings()
    private(set) var mouse = MouseSettings()

    private struct Observer {
        weak var owner: AnyObject?
        let handler: (SettingsSection) -> Void
    }
    private var observers: [Observer] = []

    /// 是否已连接后端。
    var isAttached: Bool { backend != nil }

    init() {}

    // MARK: 载入与重置

    /// 连接后端并载入全部分区。
    ///
    /// - Parameter legacyDefaults: 要迁移的旧偏好存储；nil 不迁移（测试可以用临时 suite，生产传 `.standard`）。
    ///   只迁移"后端里还没有该分区"的情形，已有数据以后端为准。
    /// 载入后把终端选项下发给 core（core 的默认值未必与用户保存的一致），并通知所有观察者。
    func attach(backend: SettingsBackend, legacyDefaults: UserDefaults? = nil, themesDirectory: URL? = nil) {
        self.backend = backend
        self.themesDirectory = themesDirectory
        if let legacyDefaults { migrateLegacyDefaults(from: legacyDefaults, into: backend) }
        load(from: backend)
        updateThemes(themesDirectory.map { loadTerminalThemes(directory: $0.path) } ?? [])
        backend.setTerminalOptions(options: display.terminalOptions)
        notifyAll()
    }

    private func load(from backend: SettingsBackend) {
        display = SettingsCodec.decode(DisplaySettings.self, from: backend.setting(key: SettingsSection.display.rawValue))
            ?? DisplaySettings()
        appearance = SettingsCodec.decode(AppearanceSettings.self, from: backend.setting(key: SettingsSection.appearance.rawValue))
            ?? AppearanceSettings()
        timeline = SettingsCodec.decode(TimelineSettings.self, from: backend.setting(key: SettingsSection.timeline.rawValue))
            ?? TimelineSettings()
        keybindings = SettingsCodec.decode(KeybindingSettings.self, from: backend.setting(key: SettingsSection.keybindings.rawValue))
            ?? KeybindingSettings()
        completion = SettingsCodec.decode(CompletionSettings.self, from: backend.setting(key: SettingsSection.completion.rawValue))
            ?? CompletionSettings()
        if let json = backend.setting(key: SettingsSection.mouse.rawValue) {
            mouse = SettingsCodec.decode(MouseSettings.self, from: json) ?? MouseSettings()
        } else {
            // 没有 mouse 分区：1.0.7 把选中即复制存在 display.copyOnSelect 里，迁移一次并写回。
            // 写回后分区存在，旧字段不再被读取；"恢复默认设置"会连 display 一起清空，不会把旧值迁回来。
            mouse = MouseSettings()
            if let legacy = Self.legacyCopyOnSelect(displayJSON: backend.setting(key: SettingsSection.display.rawValue)) {
                mouse.copyOnSelect = legacy
                persist(mouse, as: .mouse, to: backend)
            }
        }
    }

    /// 读取 display 分区里遗留的 `copyOnSelect`；没有该字段或不是布尔时返回 nil。
    /// `DisplaySettings` 已不解码这个字段，只能读原始 JSON。
    private static func legacyCopyOnSelect(displayJSON: String?) -> Bool? {
        guard let data = displayJSON?.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let number = object["copyOnSelect"] as? NSNumber,
              CFGetTypeID(number) == CFBooleanGetTypeID() else { return nil }
        return number.boolValue
    }

    /// 一次性迁移：后端里没有 `appearance` / `timeline` 分区、而 `UserDefaults` 里有旧值时，写进后端。
    ///
    /// 迁移后**立即删除**旧 key：否则"恢复默认设置"清空后端后，下次启动会因为"后端没有、旧存储有"
    /// 又把旧值迁回来，恢复默认就失效了。
    private func migrateLegacyDefaults(from defaults: UserDefaults, into backend: SettingsBackend) {
        if backend.setting(key: SettingsSection.appearance.rawValue) == nil,
           let raw = defaults.string(forKey: LegacyKey.appearanceMode) {
            var settings = AppearanceSettings()
            settings.mode = AppearanceMode(rawValue: raw) ?? .system
            persist(settings, as: .appearance, to: backend)
        }
        if backend.setting(key: SettingsSection.timeline.rawValue) == nil,
           defaults.string(forKey: LegacyKey.gutterMode) != nil || defaults.string(forKey: LegacyKey.gutterLastVisible) != nil {
            var settings = TimelineSettings()
            if let raw = defaults.string(forKey: LegacyKey.gutterMode), let mode = GutterMode(storageValue: raw) {
                settings.mode = mode
            }
            if let raw = defaults.string(forKey: LegacyKey.gutterLastVisible), let mode = GutterMode(storageValue: raw), mode != .off {
                settings.lastVisible = mode
            }
            persist(settings, as: .timeline, to: backend)
        }
        for key in [LegacyKey.appearanceMode, LegacyKey.gutterMode, LegacyKey.gutterLastVisible] {
            defaults.removeObject(forKey: key)
        }
    }

    /// 恢复默认设置（帮助 → 恢复默认设置…）：清空后端的设置表并重置内存，**不动**会话、分组、Sender 命令与历史
    /// （store-import.md §9）。失败时抛出且内存不变，让调用方提示用户。
    func resetToDefaults() throws {
        if let backend {
            try backend.resetSettings()
            backend.setTerminalOptions(options: DisplaySettings().terminalOptions)
        }
        display = DisplaySettings()
        appearance = AppearanceSettings()
        timeline = TimelineSettings()
        keybindings = KeybindingSettings()
        completion = CompletionSettings()
        mouse = MouseSettings()
        notifyAll()
    }

    // MARK: 修改

    /// 修改显示设置。值不变时什么都不做；变化时写回、下发终端选项并通知。
    func updateDisplay(_ change: (inout DisplaySettings) -> Void) {
        var new = display
        change(&new)
        guard new != display else { return }
        display = new
        persist(new, as: .display)
        backend?.setTerminalOptions(options: new.terminalOptions)
        notify(.display)
    }

    func updateAppearance(_ change: (inout AppearanceSettings) -> Void) {
        var new = appearance
        change(&new)
        guard new != appearance else { return }
        appearance = new
        persist(new, as: .appearance)
        notify(.appearance)
    }

    func updateTimeline(_ change: (inout TimelineSettings) -> Void) {
        var new = timeline
        change(&new)
        guard new != timeline else { return }
        timeline = new
        persist(new, as: .timeline)
        notify(.timeline)
    }

    func updateKeybindings(_ change: (inout KeybindingSettings) -> Void) {
        var new = keybindings
        change(&new)
        guard new != keybindings else { return }
        keybindings = new
        persist(new, as: .keybindings)
        notify(.keybindings)
    }

    func updateCompletion(_ change: (inout CompletionSettings) -> Void) {
        var new = completion
        change(&new)
        guard new != completion else { return }
        completion = new
        persist(new, as: .completion)
        notify(.completion)
    }

    func updateMouse(_ change: (inout MouseSettings) -> Void) {
        var new = mouse
        change(&new)
        guard new != mouse else { return }
        mouse = new
        persist(new, as: .mouse)
        notify(.mouse)
    }

    // MARK: 持久化

    private func persist<T: Encodable>(_ value: T, as section: SettingsSection) {
        guard let backend else { return }
        persist(value, as: section, to: backend)
    }

    /// 写失败只记日志：内存里的值照常生效（本次运行内用户看到的是自己改的结果），下次修改会再次尝试写入。
    /// 弹窗没有意义——用户无法处理存储层错误，而设置页上每次改动都会触发一次写入。
    private func persist<T: Encodable>(_ value: T, as section: SettingsSection, to backend: SettingsBackend) {
        do {
            try backend.setSetting(key: section.rawValue, json: SettingsCodec.encode(value))
        } catch {
            Self.logger.error("failed to save setting \(section.rawValue, privacy: .public): \(String(describing: error), privacy: .public)")
        }
    }

    // MARK: 观察

    /// 注册变更回调。`owner` 只用于生命周期判断（弱引用），释放后自动失效。
    /// `attach` 与 `resetToDefaults` 会对每个分区各通知一次。
    func observe(_ owner: AnyObject, _ handler: @escaping (SettingsSection) -> Void) {
        observers.append(Observer(owner: owner, handler: handler))
    }

    private func notify(_ section: SettingsSection) {
        // 先清掉已释放的观察者；遍历副本，回调里可能再注册观察者。
        observers.removeAll { $0.owner == nil }
        for observer in observers { observer.handler(section) }
    }

    private func notifyAll() {
        for section in SettingsSection.allCases { notify(section) }
    }
}
