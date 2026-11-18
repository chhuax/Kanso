import AppKit
import KansoBridge
import KansoUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindowController: MainWindowController?
    /// 全部设置的存储（全局唯一）。core 在主窗口创建时才存在，届时由 `MainWindowController` 连接后端并载入。
    private let settings = SettingsStore()
    /// 外观模式（全局唯一）：主菜单"视图 → 外观"与设置窗口共用同一个实例，二者才能保持同步。
    private lazy var appearance = AppearanceController(settings: settings)
    /// 时间戳 / 行号栏模式（全局唯一）：会话菜单、状态栏开关与所有终端视图共用同一个实例。
    private lazy var gutter = GutterModeController(settings: settings)
    /// 快捷键改绑（全局唯一）：改写主菜单项的按键，并给终端视图提供按键拦截。
    private lazy var keybindings = KeybindingController(settings: settings)

    func applicationDidFinishLaunching(_ notification: Notification) {
        PerformanceProbe.mark("launch")
        NSApp.mainMenu = Self.makeMainMenu(appearance: appearance, gutter: gutter)
        // 菜单按默认键构造；这里应用用户的改绑（此时设置可能还没载入，载入后观察者会再应用一次）。
        keybindings.applyToMenu()
        PerformanceProbe.mark("menu")
        // 单元测试以本 App 为宿主：测试不需要窗口，也不应在跑测试时弹出窗口并创建 core。
        // 也不应用用户保存的外观：它会改 NSApp.appearance，影响截图测试。
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil { return }
        do {
            // 设置在窗口创建过程中载入（core 此时才创建），并立即应用外观与时间轴模式，窗口第一次绘制就是正确的外观。
            let controller = try MainWindowController(defaults: PerformanceProbe.defaults ?? .standard, gutter: gutter, settings: settings, keybindings: keybindings)
            mainWindowController = controller
            controller.onOpenMcpSettings = { [weak self] in self?.showSettings(tab: .mcp) }
            controller.showWindow(nil)
            controller.filer.cleanPreviousEditsAfterWindowShown()
            NSApp.activate()
            PerformanceProbe.mark("shown")
            PerformanceProbe.savePhases()
        } catch {
            // 真实 core 创建失败（数据目录不可用等）：告诉用户，然后退出。
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = String(localized: "Failed to start the terminal")
            // core 的用户错误按语言渲染；其余技术细节是英文，不本地化（只用于排查）。
            alert.informativeText = CoreMessageText.text(for: error)
            alert.runModal()
            NSApp.terminate(nil)
        }
    }

    /// 退出前确认（§8）：所有会话合并成一次确认。
    /// 确认是异步的 sheet，所以先 `.terminateLater`，用户做出选择后再 `reply`。
    /// 关窗时已经确认过（`quitConfirmed`），这里直接放行，不会问第二次。
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let controller = mainWindowController, controller.requiresQuitConfirmation else { return .terminateNow }
        Task { @MainActor in
            let allowed = await controller.confirmQuit()
            NSApp.reply(toApplicationShouldTerminate: allowed)
        }
        return .terminateLater
    }

    @objc func kansoShowSettings(_ sender: Any?) {
        showSettings(tab: .general)
    }

    @objc func kansoShowAbout(_ sender: Any?) {
        NSApp.orderFrontStandardAboutPanel(options: [.credits: Self.aboutCredits])
    }

    static var aboutCredits: NSAttributedString {
        let source = URL(string: "https://github.com/chhuax/Kanso")!
        let credits = NSMutableAttributedString(
            string: String(localized: "Licensed under GPL-3.0-or-later") + "\n\n" + source.absoluteString + "\n\n"
                + String(localized: "Open Source Licenses: Choose Open Source Licenses… in the Kanso menu to read third-party licenses and notices."),
            attributes: [.font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)]
        )
        credits.addAttribute(.link, value: source, range: (credits.string as NSString).range(of: source.absoluteString))
        return credits
    }

    @objc func kansoShowOpenSourceLicenses(_ sender: Any?) {
        NSWorkspace.shared.open(Bundle.main.url(forResource: "THIRD_PARTY_NOTICES", withExtension: "md")!)
    }

    /// 视图 → 显示设置…
    @objc func kansoShowDisplaySettings(_ sender: Any?) {
        showSettings(tab: .display)
    }

    /// 视图 → 键盘快捷键…
    @objc func kansoShowKeyboardSettings(_ sender: Any?) {
        showSettings(tab: .keyboard)
    }

    private func showSettings(tab: SettingsTab) {
        SettingsWindowController.show(
            settings: settings, appearance: appearance, keybindings: keybindings, tab: tab,
            mcp: mainWindowController?.mcp
        )
    }

    /// 帮助 → 恢复默认设置…：确认后清空设置与快捷键，不动会话、分组、Sender 命令和历史。
    @objc func kansoResetSettings(_ sender: Any?) {
        Task { @MainActor [settings] in
            _ = await SettingsResetFlow.run(settings: settings, confirm: SettingsResetFlow.confirmWithAlert,
                                            onFailure: SettingsResetFlow.presentFailure)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        mainWindowController?.shutdown()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    // MARK: - 主菜单

    /// 主菜单。除标准系统动作外，动作一律用 first responder 链上的 selector（target 为 nil）：
    /// 终端视图实现 copy: / paste:，窗口控制器实现会话类动作，由 `validateMenuItem` 决定是否可用。
    /// internal 而非 private：菜单结构测试要直接构造它（不启动 App）。
    static func makeMainMenu(appearance: AppearanceController, gutter: GutterModeController) -> NSMenu {
        let main = NSMenu()

        @discardableResult
        func add(_ menu: NSMenu, _ title: String, _ action: Selector?, _ key: String = "",
                 _ modifiers: NSEvent.ModifierFlags = [.command]) -> NSMenuItem {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: key)
            if !key.isEmpty { item.keyEquivalentModifierMask = modifiers }
            return item
        }
        // 有快捷键表的命令：按键取默认值，并打上标识，`KeybindingController` 据此改写用户的绑定。
        @discardableResult
        func addCommand(_ menu: NSMenu, _ command: KeyCommand, title: String? = nil) -> NSMenuItem {
            let chord = command.defaultChord
            let item = menu.addItem(withTitle: title ?? command.title, action: command.action,
                                    keyEquivalent: chord?.menuKeyEquivalent ?? "")
            item.keyEquivalentModifierMask = chord?.modifiers.flags ?? []
            item.identifier = command.menuIdentifier
            return item
        }
        func submenu(_ title: String) -> NSMenu {
            let item = NSMenuItem()
            main.addItem(item)
            let menu = NSMenu(title: title)
            item.submenu = menu
            return menu
        }

        // Kanso
        let appMenu = submenu("")
        add(appMenu, String(localized: "About Kanso"), #selector(AppDelegate.kansoShowAbout(_:))).target = NSApp.delegate
        add(appMenu, String(localized: "Open Source Licenses…"), #selector(AppDelegate.kansoShowOpenSourceLicenses(_:))).target = NSApp.delegate
        appMenu.addItem(.separator())
        // 设置窗口不依赖主窗口（没有窗口时也能打开），所以 target 直接是 AppDelegate。
        add(appMenu, String(localized: "Settings…"), #selector(AppDelegate.kansoShowSettings(_:)), ",").target = NSApp.delegate
        appMenu.addItem(.separator())
        add(appMenu, String(localized: "Hide Kanso"), #selector(NSApplication.hide(_:)), "h")
        add(appMenu, String(localized: "Hide Others"), #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option])
        add(appMenu, String(localized: "Show All"), #selector(NSApplication.unhideAllApplications(_:)))
        appMenu.addItem(.separator())
        add(appMenu, String(localized: "Quit Kanso"), #selector(NSApplication.terminate(_:)), "q")

        // 会话
        let sessionMenu = submenu(String(localized: "Session"))
        addCommand(sessionMenu, .newSession)
        addCommand(sessionMenu, .newLocalShell)
        sessionMenu.addItem(.separator())
        addCommand(sessionMenu, .previousSession)
        addCommand(sessionMenu, .nextSession)
        addCommand(sessionMenu, .previousSplit)
        addCommand(sessionMenu, .nextSplit)
        let goItem = sessionMenu.addItem(withTitle: String(localized: "Go to Tab"), action: nil, keyEquivalent: "")
        let goMenu = NSMenu(title: goItem.title)
        goItem.submenu = goMenu
        for n in 1...9 {
            let item = add(goMenu, String(localized: "Tab \(n)"), #selector(MainWindowController.kansoSelectTab(_:)), String(n))
            item.tag = n - 1
        }
        sessionMenu.addItem(.separator())
        let gutterItem = sessionMenu.addItem(
            withTitle: String(localized: "Timestamps and Line Numbers"), action: nil, keyEquivalent: ""
        )
        gutterItem.submenu = gutter.makeMenu()
        let transferItem = sessionMenu.addItem(withTitle: String(localized: "File Transfer"), action: nil, keyEquivalent: "")
        let transferMenu = NSMenu(title: transferItem.title)
        transferItem.submenu = transferMenu
        add(transferMenu, String(localized: "Send with XMODEM…"), #selector(MainWindowController.kansoXmodemSend(_:)))
        add(transferMenu, String(localized: "Send with XMODEM-1K…"), #selector(MainWindowController.kansoXmodem1KSend(_:)))
        add(transferMenu, String(localized: "Receive with XMODEM…"), #selector(MainWindowController.kansoXmodemReceive(_:)))
        transferMenu.addItem(.separator())
        add(transferMenu, String(localized: "Cancel Transfer"), #selector(MainWindowController.kansoCancelTerminalTransfer(_:)))
        sessionMenu.addItem(.separator())
        add(sessionMenu, String(localized: "Import OpenSSH Config…"), #selector(MainWindowController.kansoImportSshConfig(_:)))
        add(sessionMenu, String(localized: "Export Data…"), #selector(MainWindowController.kansoExportData(_:)))
        add(sessionMenu, String(localized: "Import Data…"), #selector(MainWindowController.kansoImportData(_:)))
        sessionMenu.addItem(.separator())
        addCommand(sessionMenu, .closeSession)

        // 编辑
        let editMenu = submenu(String(localized: "Edit"))
        addCommand(editMenu, .copy)
        addCommand(editMenu, .paste)
        addCommand(editMenu, .selectAll)
        editMenu.addItem(.separator())
        addCommand(editMenu, .clearBuffer)
        addCommand(editMenu, .previousCommandBlock)
        addCommand(editMenu, .nextCommandBlock)
        editMenu.addItem(.separator())
        addCommand(editMenu, .find)
        addCommand(editMenu, .findNext)
        addCommand(editMenu, .findPrevious)
        editMenu.addItem(.separator())
        addCommand(editMenu, .commandSuggestions)
        addCommand(editMenu, .clearCommandHistory)

        // 视图
        let viewMenu = submenu(String(localized: "View"))
        // 菜单标题随状态在"隐藏 / 显示标签栏"之间变化（validateMenuItem），初始为"隐藏"。
        addCommand(viewMenu, .toggleTabBar, title: String(localized: "Hide Tab Bar"))
        addCommand(viewMenu, .toggleFiler, title: String(localized: "Show File Browser"))
        addCommand(viewMenu, .toggleSender)
        addCommand(viewMenu, .locateWorkingDirectory)
        addCommand(viewMenu, .commandPalette)
        viewMenu.addItem(.separator())
        addCommand(viewMenu, .splitRight)
        addCommand(viewMenu, .splitDown)
        viewMenu.addItem(.separator())
        addCommand(viewMenu, .increaseFontSize)
        addCommand(viewMenu, .decreaseFontSize)
        addCommand(viewMenu, .resetFontSize)
        viewMenu.addItem(.separator())
        let appearanceItem = viewMenu.addItem(withTitle: String(localized: "Appearance"), action: nil, keyEquivalent: "")
        appearanceItem.submenu = appearance.makeMenu()
        viewMenu.addItem(.separator())
        add(viewMenu, String(localized: "Display Settings…"), #selector(AppDelegate.kansoShowDisplaySettings(_:))).target = NSApp.delegate
        add(viewMenu, String(localized: "Keyboard Shortcuts…"), #selector(AppDelegate.kansoShowKeyboardSettings(_:))).target = NSApp.delegate

        // 窗口
        let windowMenu = submenu(String(localized: "Window"))
        add(windowMenu, String(localized: "Minimize"), #selector(NSWindow.performMiniaturize(_:)), "m")
        add(windowMenu, String(localized: "Zoom"), #selector(NSWindow.performZoom(_:)))
        // 登记为窗口菜单后，系统会自动追加打开的窗口列表。
        NSApp.windowsMenu = windowMenu

        // 帮助
        let helpMenu = submenu(String(localized: "Help"))
        add(helpMenu, String(localized: "Restore Default Settings…"), #selector(AppDelegate.kansoResetSettings(_:))).target = NSApp.delegate
        // 登记为帮助菜单后，系统会在里面加入"搜索"框。
        NSApp.helpMenu = helpMenu

        return main
    }
}
