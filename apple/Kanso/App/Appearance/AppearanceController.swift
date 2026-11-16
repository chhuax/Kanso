import AppKit

/// 界面外观模式（ui-shell.md §10）：跟随系统 / 浅色 / 深色。
enum AppearanceMode: String, CaseIterable {
    case system
    case light
    case dark

    /// 本地化显示名，用于菜单与设置页。
    var displayName: String {
        switch self {
        case .system: String(localized: "Follow System")
        case .light: String(localized: "Light")
        case .dark: String(localized: "Dark")
        }
    }

    /// 对应的 `NSAppearance`；`.system` 为 nil，表示不覆盖、交还给系统。
    var nsAppearance: NSAppearance? {
        switch self {
        case .system: nil
        case .light: NSAppearance(named: .aqua)
        case .dark: NSAppearance(named: .darkAqua)
        }
    }
}

/// 外观模式的应用与分发；持久化交给 `SettingsStore`（core 设置表的 `appearance` 分区）。
///
/// 实时生效依赖的链路：设置 `NSApp.appearance` 后，所有视图的 `effectiveAppearance` 随之变化；
/// 界面颜色来自颜色集的 Any/Dark 两个外观，`TerminalView` 在 `viewDidChangeEffectiveAppearance` 里切换终端配色，
/// 因此无需重启。
///
/// 接入方：`AppDelegate` 持有唯一实例并与主窗口共用同一个 `SettingsStore`；
/// 设置载入（`SettingsStore.attach`）或恢复默认后自动应用并通知观察者，所以启动时不需要再手动调用 `applyStoredMode()`
/// （它保留给不经过 `attach` 的场景）。"视图 → 外观"子菜单用 `makeMenu()` 生成；设置窗口通过 `observe(_:_:)` 与之双向同步。
@MainActor
final class AppearanceController: NSObject, NSMenuItemValidation {
    private let settings: SettingsStore
    private let apply: (NSAppearance?) -> Void

    /// 变化观察者。持弱引用，观察者释放后自动失效，不需要显式移除。
    private struct Observer {
        weak var owner: AnyObject?
        let handler: (AppearanceMode) -> Void
    }
    private var observers: [Observer] = []

    /// - Parameters:
    ///   - settings: 设置存储。测试注入独立实例（不挂后端即纯内存，不碰真实存储）。
    ///   - apply: 应用外观的动作。默认写 `NSApp.appearance`；测试注入记录闭包，
    ///     因为设置 `NSApp.appearance` 会影响同进程的其他测试与截图测试。
    init(
        settings: SettingsStore = SettingsStore(),
        apply: @escaping (NSAppearance?) -> Void = { NSApp.appearance = $0 }
    ) {
        self.settings = settings
        self.apply = apply
        super.init()
        // 外观分区变化（设置页、菜单、载入、恢复默认）都走这里：先应用，再通知观察者。
        settings.observe(self) { [weak self] section in
            guard section == .appearance, let self else { return }
            self.modeDidChange()
        }
    }

    /// 当前模式。未设置或存了无法识别的值时为 `.system`（由设置解码保证）。
    /// setter 写入设置；值真正变化时应用并通知观察者。
    var mode: AppearanceMode {
        get { settings.appearance.mode }
        set { settings.updateAppearance { $0.mode = newValue } }
    }

    private func modeDidChange() {
        let current = mode
        apply(current.nsAppearance)
        // 先清掉已释放的；顺便避免回调里再注册观察者时遍历被修改的数组。
        observers.removeAll { $0.owner == nil }
        for observer in observers { observer.handler(current) }
    }

    /// 启动时调用一次，把已保存的模式应用到界面。
    func applyStoredMode() {
        apply(mode.nsAppearance)
    }

    /// 注册模式变化回调（外观分区变化时触发，含设置载入与恢复默认；`applyStoredMode()` 不触发）。
    /// 选择回调而不是 `NotificationCenter`：主线程隔离下闭包更直接，也便于测试。
    /// `owner` 只用于生命周期判断（弱引用），不要传临时对象。
    func observe(_ owner: AnyObject, _ handler: @escaping (AppearanceMode) -> Void) {
        observers.append(Observer(owner: owner, handler: handler))
    }

    // MARK: 菜单

    /// 生成"外观"子菜单：三项单选，当前模式打勾，选择即切换。
    /// 每个菜单项的勾选在 `validateMenuItem` 里刷新，所以多处菜单（主菜单、设置页）不会失同步。
    func makeMenu() -> NSMenu {
        let menu = NSMenu(title: String(localized: "Appearance"))
        for mode in AppearanceMode.allCases {
            let item = NSMenuItem(title: mode.displayName, action: #selector(selectAppearance(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = mode.rawValue
            item.state = mode == self.mode ? .on : .off
            menu.addItem(item)
        }
        return menu
    }

    @objc func selectAppearance(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let selected = AppearanceMode(rawValue: raw) else { return }
        mode = selected
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(selectAppearance(_:)),
           let raw = menuItem.representedObject as? String {
            menuItem.state = raw == mode.rawValue ? .on : .off
        }
        return true
    }
}
