import AppKit
import TerminalKit

/// 设置页（标签）。`rawValue` 无持久化含义，只用于菜单入口指定要打开哪一页。
enum SettingsTab: Int, CaseIterable {
    case general
    case display
    case keyboard
    case mcp

    var title: String {
        switch self {
        case .general: String(localized: "General")
        case .display: String(localized: "Display")
        case .keyboard: String(localized: "Keyboard Shortcuts")
        case .mcp: String(localized: "MCP")
        }
    }

    var symbol: String {
        switch self {
        case .general: "gearshape"
        case .display: "textformat.size"
        case .keyboard: "keyboard"
        case .mcp: "point.3.connected.trianglepath.dotted"
        }
    }
}

/// 设置窗口：通用 / 显示 / 键盘快捷键三页。
///
/// 单例：通过 `show(...)` 打开，重复调用只前置（并可切到指定页）。窗口不可缩放，位置由 autosave 记忆，
/// 标题随选中页变化；高度随当前页内容自适应，宽度固定。
@MainActor
final class SettingsWindowController: NSWindowController, NSWindowDelegate {
    private let settings: SettingsStore
    private static var shared: SettingsWindowController?

    /// 语言与地区设置页（macOS 14+ 的系统设置扩展 URL）。
    static let languageSettingsURL = URL(string: "x-apple.systempreferences:com.apple.Localization-Settings.extension")!

    let generalViewController: GeneralSettingsViewController
    let displayViewController: DisplaySettingsViewController
    let keyboardViewController: KeyboardSettingsViewController
    let mcpViewController: McpSettingsViewController?
    private let tabs = SettingsTabViewController()
    private let pageOrder: [SettingsTab]

    /// 菜单的入口（⌘,、视图 → 显示设置… / 键盘快捷键…）。
    /// 有协调器且 `mcp.supported` 为真时才加入 MCP 页；不看服务能不能 start。
    static func show(
        settings: SettingsStore, appearance: AppearanceController, keybindings: KeybindingController,
        tab: SettingsTab = .general, mcp: McpCoordinator? = nil
    ) {
        settings.reloadThemes()
        let controller = shared ?? SettingsWindowController(
            settings: settings, appearance: appearance, keybindings: keybindings, mcp: mcp
        )
        shared = controller
        controller.select(tab)
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }

    /// - Parameters:
    ///   - openURL: 打开外部 URL 的动作；测试注入记录闭包，不真的打开系统设置。
    ///   - conflictPrompter: 快捷键冲突的询问方式；测试注入自动应答的实现，不弹窗。
    ///   - autosaveName: 窗口位置记忆名；测试传 nil，避免写入用户真实的 `UserDefaults.standard`。
    init(
        settings: SettingsStore,
        appearance: AppearanceController,
        keybindings: KeybindingController,
        openURL: @escaping (URL) -> Void = { NSWorkspace.shared.open($0) },
        conflictPrompter: KeybindingConflictPrompter = AlertKeybindingConflictPrompter(),
        autosaveName: String? = "KansoSettingsWindow",
        mcp: McpCoordinator? = nil
    ) {
        self.settings = settings
        generalViewController = GeneralSettingsViewController(appearance: appearance, openURL: openURL)
        displayViewController = DisplaySettingsViewController(settings: settings)
        keyboardViewController = KeyboardSettingsViewController(keybindings: keybindings, prompter: conflictPrompter)
        if let mcp, mcp.supported {
            mcpViewController = McpSettingsViewController(mcp: mcp)
        } else {
            mcpViewController = nil
        }

        tabs.tabStyle = .toolbar
        // 子页未设 title 时系统会把窗口标题推成「未命名」；关掉传播，由我们显式设标题。
        tabs.canPropagateSelectedChildViewControllerTitle = false
        var pages: [(SettingsTab, NSViewController)] = [
            (.general, generalViewController), (.display, displayViewController),
            (.keyboard, keyboardViewController),
        ]
        if let mcpViewController {
            pages.append((.mcp, mcpViewController))
        }
        pageOrder = pages.map(\.0)
        for (tab, controller) in pages {
            controller.title = tab.title
            let item = NSTabViewItem(viewController: controller)
            item.label = tab.title
            item.image = .symbol(tab.symbol, description: tab.title)
            tabs.addTabViewItem(item)
        }

        // 没有 .resizable：设置页尺寸由内容决定。
        let window = NSWindow(contentViewController: tabs)
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.title = SettingsTab.general.title
        super.init(window: window)
        window.delegate = self

        if let autosaveName {
            // 没有已保存的位置时居中；setFrameAutosaveName 必须在这之后调用，否则会把居中前的位置存下来。
            // 只恢复原点：高度随当前页内容，不能沿用上次别的页留下的高度。
            var frame = window.frame
            if window.setFrameUsingName(autosaveName) {
                frame.origin = window.frame.origin
            } else {
                window.center()
                frame.origin = window.frame.origin
            }
            window.setFrame(frame, display: false)
            window.setFrameAutosaveName(autosaveName)
        } else {
            window.center()
        }
        resizeToFitCurrentPage()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) 不支持") }

    /// 切到指定页、更新标题，并按该页内容调整窗口高度。
    func select(_ tab: SettingsTab) {
        guard let index = pageOrder.firstIndex(of: tab) else { return }
        tabs.selectedTabViewItemIndex = index
        window?.title = tab.title
        resizeToFitCurrentPage()
    }

    func windowDidBecomeKey(_ notification: Notification) {
        settings.reloadThemes()
    }

    /// 当前选中的页。
    var selectedTab: SettingsTab {
        let index = tabs.selectedTabViewItemIndex
        guard pageOrder.indices.contains(index) else { return .general }
        return pageOrder[index]
    }

    /// 窗口里实际有的页。MCP 页只有协调器存在且 `supported` 为真时才在里面。
    var tabsInWindow: [SettingsTab] { pageOrder }

    /// 宽度保持不变，高度贴合当前页 `fittingSize`。
    func resizeToFitCurrentPage() {
        guard let window,
              tabs.selectedTabViewItemIndex >= 0,
              tabs.selectedTabViewItemIndex < tabs.tabViewItems.count,
              let page = tabs.tabViewItems[tabs.selectedTabViewItemIndex].viewController
        else { return }
        let view = page.view
        view.layoutSubtreeIfNeeded()
        let fitted = view.fittingSize
        guard fitted.width > 0, fitted.height > 0 else { return }
        window.setContentSize(NSSize(width: SettingsPageViewController.pageWidth, height: fitted.height))
    }
}

/// 让窗口标题与高度跟随选中页。
///
/// `.toolbar` 风格下系统通常会自动同步标题，但依赖该行为不可靠（首次显示与程序化切换时尤其如此），所以显式设置。
final class SettingsTabViewController: NSTabViewController {
    override func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        super.tabView(tabView, didSelect: tabViewItem)
        if let label = tabViewItem?.label { view.window?.title = label }
        (view.window?.windowController as? SettingsWindowController)?.resizeToFitCurrentPage()
    }
}

/// "通用"页：外观模式、语言说明。
@MainActor
final class GeneralSettingsViewController: SettingsPageViewController {
    private let appearance: AppearanceController
    private let openURL: (URL) -> Void

    /// 外观模式下拉框：自绘的 `KPopUpField`，与其它界面的下拉框同一套样式。
    let appearancePopUp = KPopUpField()
    let lightThemePopUp = KPopUpField()
    let darkThemePopUp = KPopUpField()
    let themesFolderButton = KButton(title: String(localized: "Open Themes Folder"), symbol: "folder")
    let languageButton = KButton(title: String(localized: "Open Language & Region Settings"), style: .secondary)
    let languageLabel = SettingsPageViewController.caption(String(
        localized: "The interface language follows the system. To set a language for Kanso only, go to System Settings › General › Language & Region › Applications."
    ))

    init(appearance: AppearanceController, openURL: @escaping (URL) -> Void) {
        self.appearance = appearance
        self.openURL = openURL
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) 不支持") }

    override func loadView() {
        appearancePopUp.setItems(AppearanceMode.allCases.map(\.displayName))
        appearancePopUp.onChange = { [weak self] index in self?.appearancePicked(at: index) }
        appearancePopUp.setAccessibilityLabel(String(localized: "Appearance"))
        let modeCard = KCard(title: String(localized: "Appearance"), symbol: "circle.lefthalf.filled")
        modeCard.addRow(LabeledField(title: String(localized: "Mode"), control: appearancePopUp))

        let settings = appearance.settings
        for (field, title, key) in [
            (lightThemePopUp, String(localized: "Light Terminal Theme"), \AppearanceSettings.lightTheme),
            (darkThemePopUp, String(localized: "Dark Terminal Theme"), \AppearanceSettings.darkTheme),
        ] {
            field.setAccessibilityLabel(title)
            field.onOpen = { settings.reloadThemes() }
            field.onChange = { index in
                let id = settings.themes[index].id
                settings.updateAppearance { $0[keyPath: key] = id }
            }
            modeCard.addRow(LabeledField(title: title, control: field))
        }
        themesFolderButton.onPress = { [weak self] in
            if let directory = settings.themesDirectory { self?.openURL(directory) }
        }
        themesFolderButton.setContentHuggingPriority(.required, for: .horizontal)
        modeCard.addRow(NSStackView(views: [themesFolderButton, NSView()]))
        languageButton.onPress = { [weak self] in self?.openLanguageSettings(nil) }
        languageButton.setContentHuggingPriority(.required, for: .horizontal)
        let languageCard = KCard(title: String(localized: "Language"), symbol: "globe")
        languageCard.addRow(languageLabel)
        let buttonRow = NSStackView(views: [languageButton, NSView()])
        buttonRow.orientation = .horizontal
        languageCard.addRow(buttonRow)

        view = makePageView([modeCard, languageCard])
        reload()
        appearance.observe(self) { [weak self] _ in self?.reload() }
    }

    private func reload() {
        appearancePopUp.selectItem(at: AppearanceMode.allCases.firstIndex(of: appearance.mode) ?? 0)
        let settings = appearance.settings
        let themes = settings.themes
        let titles = themes.map(\.name)
        lightThemePopUp.setItems(titles)
        darkThemePopUp.setItems(titles)
        lightThemePopUp.selectItem(at: themes.firstIndex { $0.id == settings.appearance.lightTheme } ?? 0)
        darkThemePopUp.selectItem(at: themes.firstIndex { $0.id == settings.appearance.darkTheme } ?? 1)
        themesFolderButton.isEnabled = settings.themesDirectory != nil
        themesFolderButton.toolTip = settings.themesDirectory?.path
    }

    private func appearancePicked(at index: Int) {
        guard AppearanceMode.allCases.indices.contains(index) else { return }
        appearance.mode = AppearanceMode.allCases[index]
    }

    @objc func openLanguageSettings(_ sender: Any?) {
        openURL(SettingsWindowController.languageSettingsURL)
    }
}
