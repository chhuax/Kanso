import AppKit
import KansoBridge
import KansoUI
import TerminalKit
import XCTest
@testable import Kanso

/// "显示"页、字体解析，以及显示 / 外观设置应用到所有终端视图（含新开的会话）。
@MainActor
final class DisplaySettingsTests: XCTestCase {
    private var controllers: [WorkspaceController] = []

    override func tearDown() async throws {
        for c in controllers { c.shutdown() }
        controllers = []
    }

    private func makePage(
        families: [String] = ["Menlo", "Monaco"]
    ) -> (DisplaySettingsViewController, SettingsStore, MemorySettingsBackend) {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        let page = DisplaySettingsViewController(settings: settings, fontFamilies: { families })
        _ = page.view
        return (page, settings, backend)
    }

    // MARK: 页面

    func testPageReflectsSettingsAndDrivesThem() {
        let (page, settings, _) = makePage()
        XCTAssertEqual(page.sizeField.value, 14)
        XCTAssertEqual(page.lineHeightField.value, 1.2)
        XCTAssertEqual(page.scrollbackField.value, 10_000)
        // 默认闪动竖线。
        XCTAssertEqual(page.cursorShapeCards.selectedIndex, 2)
        XCTAssertTrue(page.blinkRow.isChecked)
        XCTAssertTrue(page.contrastRow.isChecked)
        XCTAssertEqual(page.contrastField.value, 4.5)

        page.sizeField.nudge(1)
        XCTAssertEqual(settings.display.fontSize, 15)
        page.lineHeightField.nudge(1)
        XCTAssertEqual(settings.display.lineHeight, 1.25, accuracy: 0.0001)
        page.cursorShapeCards.select(0)
        XCTAssertEqual(settings.display.cursorShape, .block)
        page.blinkRow.toggle()
        XCTAssertFalse(settings.display.cursorBlink)
        page.scrollbackField.nudge(1)
        XCTAssertEqual(settings.display.scrollbackLines, 11_000)
        page.contrastRow.toggle()
        XCTAssertFalse(settings.display.minimumContrastEnabled)
        XCTAssertFalse(page.contrastField.isEnabled, "关闭保护后比例输入不可用")
        page.contrastRow.toggle()
        page.contrastField.nudge(1)
        XCTAssertEqual(settings.display.minimumContrastRatio, 5.0)
    }

    func testPageFollowsExternalChanges() {
        let (page, settings, _) = makePage()
        settings.updateDisplay { $0.fontSize = 20; $0.cursorShape = .underline; $0.scrollbackLines = 5000 }
        XCTAssertEqual(page.sizeField.value, 20)
        XCTAssertEqual(page.cursorShapeCards.selectedIndex, 1)
        XCTAssertEqual(page.scrollbackField.value, 5000)
        // 恢复默认后页面回到默认值。
        try? settings.resetToDefaults()
        XCTAssertEqual(page.sizeField.value, 14)
        XCTAssertEqual(page.lineHeightField.value, 1.2)
        XCTAssertEqual(page.cursorShapeCards.selectedIndex, 2)
    }

    func testSemanticColoringPersistsAndReachesCore() throws {
        let (page, settings, backend) = makePage()
        XCTAssertTrue(page.semanticRow.isChecked)
        page.semanticRow.toggle()
        XCTAssertFalse(settings.display.semanticColoring)
        XCTAssertEqual(backend.terminalOptionsLog.last?.semanticColoring, false)
        let encoded = try JSONEncoder().encode(settings.display)
        XCTAssertFalse(try JSONDecoder().decode(DisplaySettings.self, from: encoded).semanticColoring)
        XCTAssertTrue(try JSONDecoder().decode(DisplaySettings.self, from: Data("{}".utf8)).semanticColoring)
    }

    func testNumberFieldClampsAndRejectsGarbage() {
        let (page, settings, _) = makePage()
        page.lineHeightField.field.stringValue = "3"
        page.lineHeightField.commitText()
        XCTAssertEqual(settings.display.lineHeight, 2)
        page.lineHeightField.field.stringValue = "0.5"
        page.lineHeightField.commitText()
        XCTAssertEqual(settings.display.lineHeight, 1)
        page.lineHeightField.field.stringValue = "abc"
        page.lineHeightField.commitText()
        XCTAssertEqual(settings.display.lineHeight, 1)
        page.sizeField.field.stringValue = "500"
        page.sizeField.commitText()
        XCTAssertEqual(settings.display.fontSize, 36, "越界输入夹到上限")
        page.sizeField.field.stringValue = "2"
        page.sizeField.commitText()
        XCTAssertEqual(settings.display.fontSize, 9, "越界输入夹到下限")
        page.sizeField.field.stringValue = "abc"
        page.sizeField.commitText()
        XCTAssertEqual(settings.display.fontSize, 9, "无法解析的输入不改变设置")
        XCTAssertEqual(page.sizeField.field.stringValue, "9", "文字还原为当前值")
        page.scrollbackField.field.stringValue = "5"
        page.scrollbackField.commitText()
        XCTAssertEqual(settings.display.scrollbackLines, 1_000)
        page.scrollbackField.field.stringValue = "999999999"
        page.scrollbackField.commitText()
        XCTAssertEqual(settings.display.scrollbackLines, 1_000_000)
        // 到边界后对应方向的按钮不可用。
        XCTAssertFalse(page.scrollbackField.plusButton.isEnabled)
    }

    func testFontMenuListsSystemOptionAndFamiliesAndAppliesChoice() {
        let (page, settings, _) = makePage(families: ["Menlo", "Monaco"])
        let menu = page.makeFontMenu()
        XCTAssertEqual(menu.items.filter { !$0.isSeparatorItem }.map(\.title), [String(localized: "System Monospaced"), "Menlo", "Monaco"])
        XCTAssertEqual(menu.items.first?.state, .on, "字体族为空时系统字体打勾")

        _ = menu.items[2].target?.perform(menu.items[2].action, with: menu.items[2])
        XCTAssertEqual(settings.display.fontFamily, "Menlo")
        XCTAssertEqual(page.fontField.stringValue, "Menlo")
        let again = page.makeFontMenu()
        XCTAssertEqual(again.items.first { $0.state == .on }?.title, "Menlo")

        // 选"系统等宽字体"清空。
        let system = again.items[0]
        _ = system.target?.perform(system.action, with: system)
        XCTAssertEqual(settings.display.fontFamily, "")
    }

    func testTypedFontNameIsCommittedAndUnknownNameWarns() {
        let (page, settings, _) = makePage()
        page.fontField.stringValue = "  Menlo  "
        page.setFontFamily(page.fontField.stringValue)
        XCTAssertEqual(settings.display.fontFamily, "Menlo")
        XCTAssertTrue(page.fontWarning.isHidden)

        page.setFontFamily("NoSuchFontFamily-Kanso")
        XCTAssertEqual(settings.display.fontFamily, "NoSuchFontFamily-Kanso", "允许手填任意名称")
        XCTAssertFalse(page.fontWarning.isHidden, "找不到的字体要提示，不能静默换成别的")
        page.setFontFamily("")
        XCTAssertTrue(page.fontWarning.isHidden)
    }

    // MARK: 字体解析

    func testFontResolver() {
        let system = TerminalFontResolver.font(family: "", size: 14)
        XCTAssertFalse(system.isFallback)
        XCTAssertEqual(system.font.familyName, "Menlo")
        XCTAssertEqual(system.font.pointSize, 14)
        XCTAssertTrue(system.font.isFixedPitch)

        let menlo = TerminalFontResolver.font(family: "Menlo", size: 15)
        XCTAssertFalse(menlo.isFallback)
        XCTAssertEqual(menlo.font.familyName, "Menlo")
        XCTAssertEqual(menlo.font.pointSize, 15)

        // 字体全名 / PostScript 名也能用（手填私有构建的 Nerd Font 时常见）。
        XCTAssertFalse(TerminalFontResolver.font(family: "Menlo-Regular", size: 13).isFallback)

        let missing = TerminalFontResolver.font(family: "NoSuchFontFamily-Kanso", size: 13)
        XCTAssertTrue(missing.isFallback)
        XCTAssertTrue(missing.font.isFixedPitch, "回落到系统等宽字体")
    }

    func testInstalledMonospacedFamiliesAreAllFixedPitch() throws {
        let families = TerminalFontResolver.installedMonospacedFamilies()
        XCTAssertFalse(families.isEmpty)
        XCTAssertTrue(families.contains("Menlo"))
        XCTAssertEqual(families, families.sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending })
        // 抽查：列表里没有明显的比例字体。
        XCTAssertFalse(families.contains("Helvetica Neue"))
    }

    // MARK: 应用到终端视图

    private func makeWorkspace(settings: SettingsStore) -> WorkspaceController {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let controller = WorkspaceController(
            core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true),
            gutter: GutterModeController(settings: settings), settings: settings
        )
        controllers.append(controller)
        return controller
    }

    func testDisplaySettingsApplyToAllExistingAndNewViews() throws {
        let settings = SettingsStore.attached(to: MemorySettingsBackend())
        let controller = makeWorkspace(settings: settings)
        let first = try controller.openLocalShell()
        let firstView = try XCTUnwrap(controller.host(for: first)).view
        try XCTSkipIf(firstView.renderer == nil, "没有 Metal 设备")

        settings.updateDisplay {
            $0.fontFamily = "Menlo"
            $0.fontSize = 18
            $0.lineHeight = 1.5
            $0.minimumContrastEnabled = false
            $0.minimumContrastRatio = 7
        }
        XCTAssertEqual(firstView.font.familyName, "Menlo")
        XCTAssertEqual(firstView.font.pointSize, 18)
        XCTAssertEqual(firstView.lineHeight, 1.5)
        XCTAssertFalse(firstView.isMinimumContrastEnabled)
        XCTAssertEqual(firstView.minimumContrastRatio, 7)

        // 新开的会话（拆分）用当前设置，不是默认值。
        let second = try controller.split(axis: .horizontal)
        let secondView = try XCTUnwrap(controller.host(for: second)).view
        XCTAssertEqual(secondView.font.familyName, "Menlo")
        XCTAssertEqual(secondView.font.pointSize, 18)
        XCTAssertEqual(secondView.lineHeight, 1.5)
        XCTAssertFalse(secondView.isMinimumContrastEnabled)
        XCTAssertEqual(secondView.minimumContrastRatio, 7)

        // 再改一次：两个视图同步。
        settings.updateDisplay { $0.fontSize = 11; $0.lineHeight = 1.1; $0.minimumContrastEnabled = true }
        for view in [firstView, secondView] {
            XCTAssertEqual(view.font.pointSize, 11)
            XCTAssertEqual(view.lineHeight, 1.1)
            XCTAssertTrue(view.isMinimumContrastEnabled)
        }
    }

    func testViewsStartWithStoredSettingsAtLaunch() throws {
        let backend = MemorySettingsBackend()
        backend.values["display"] = #"{"v":1,"fontFamily":"Menlo","fontSize":21}"#
        backend.values["appearance"] = #"{"v":1,"lightTheme":"kanso-dark","darkTheme":"kanso-light"}"#
        let controller = makeWorkspace(settings: .attached(to: backend))
        let id = try controller.openLocalShell()
        let view = try XCTUnwrap(controller.host(for: id)).view
        try XCTSkipIf(view.renderer == nil, "没有 Metal 设备")
        XCTAssertEqual(view.font.pointSize, 21)
        XCTAssertEqual(view.lightPalette.background, TerminalPalette.kansoDark.background)
        XCTAssertEqual(view.darkPalette.background, TerminalPalette.kansoLight.background)
    }

    func testFileThemeDiscoverySelectionAndRestore() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let settings = SettingsStore()
        let backend = MemorySettingsBackend()
        settings.attach(backend: backend, themesDirectory: directory)
        let workspace = makeWorkspace(settings: settings)
        let session = try workspace.openLocalShell()
        let first = try XCTUnwrap(workspace.host(for: session)).view
        let appearance = AppearanceController(settings: settings, apply: { _ in })
        var opened: [URL] = []
        let page = GeneralSettingsViewController(appearance: appearance, openURL: { opened.append($0) })
        _ = page.view
        page.themesFolderButton.performClick(nil)
        XCTAssertEqual(opened, [directory])

        let file = directory.appendingPathComponent("ocean.json")
        func writeTheme(background: String) throws {
            let json: [String: Any] = [
                "version": 1, "name": "Ocean", "foreground": "#FAFAFA", "background": background,
                "cursor": "#ABCEDF", "cursorText": "#101010", "selectionBackground": "#394552",
                "ansi": Array(repeating: "#909090", count: 16), "semantic": Array(repeating: "#C0C0C0", count: 13),
            ]
            try JSONSerialization.data(withJSONObject: json).write(to: file)
        }
        try writeTheme(background: "#123456")
        // 顺序：内置配色、随程序附带的配色（如 Classic Console）、用户文件配色。附带配色会随版本增加，
        // 所以按名字取下标，不写死位置。
        let titles = page.darkThemePopUp.makeMenu().items.map(\.title)
        XCTAssertEqual(Array(titles.prefix(2)), ["Kanso Light", "Kanso Dark"])
        XCTAssertTrue(titles.contains("Classic Console"))
        XCTAssertEqual(titles.last, "Ocean")
        page.darkThemePopUp.userSelect(at: try XCTUnwrap(titles.firstIndex(of: "Ocean")))
        XCTAssertEqual(settings.appearance.darkTheme, "file:ocean")
        XCTAssertEqual(page.lightThemePopUp.selectedTitle, "Kanso Light")

        if first.renderer != nil { XCTAssertEqual(first.darkPalette.background, RGB8(hex: 0x123456)) }
        let split = try workspace.split(axis: .horizontal)
        let second = try XCTUnwrap(workspace.host(for: split)).view
        XCTAssertEqual(second.darkPalette.background, first.darkPalette.background)
        let restarted = SettingsStore()
        restarted.attach(backend: backend, themesDirectory: directory)
        func darkBackground(_ store: SettingsStore) -> RGB8 {
            store.appearance.resolvedDarkTheme(in: store.themes).palette.background
        }
        XCTAssertEqual(darkBackground(restarted), RGB8(hex: 0x123456))
        try writeTheme(background: "#ABCDEF")
        _ = page.darkThemePopUp.makeMenu()
        XCTAssertEqual(darkBackground(settings), RGB8(hex: 0xABCDEF))
        if first.renderer != nil { XCTAssertEqual(first.darkPalette.background, RGB8(hex: 0xABCDEF)) }
        try FileManager.default.removeItem(at: file)
        _ = page.darkThemePopUp.makeMenu()
        XCTAssertEqual(page.darkThemePopUp.selectedTitle, "Kanso Dark")
        XCTAssertEqual(first.darkPalette.background, TerminalPalette.kansoDark.background)
        XCTAssertEqual(settings.appearance.darkTheme, "file:ocean")
        try writeTheme(background: "#234567")
        _ = page.darkThemePopUp.makeMenu()
        XCTAssertEqual(page.darkThemePopUp.selectedTitle, "Ocean")
        XCTAssertEqual(darkBackground(settings), RGB8(hex: 0x234567))
        if second.renderer != nil { XCTAssertEqual(second.darkPalette.background, RGB8(hex: 0x234567)) }
    }

    func testThemeChoicesApplyToViewPalettes() throws {
        let settings = SettingsStore.attached(to: MemorySettingsBackend())
        let controller = makeWorkspace(settings: settings)
        let id = try controller.openLocalShell()
        let view = try XCTUnwrap(controller.host(for: id)).view
        try XCTSkipIf(view.renderer == nil, "没有 Metal 设备")
        XCTAssertEqual(view.lightPalette.background, TerminalPalette.kansoLight.background)
        XCTAssertEqual(view.darkPalette.background, TerminalPalette.kansoDark.background)

        settings.updateAppearance { $0.lightTheme = "kanso-dark" }
        XCTAssertEqual(view.lightPalette.background, TerminalPalette.kansoDark.background)
        // 当前外观取哪套由 effectiveAppearance 决定：给视图所在窗口设外观，验证 currentPalette 跟随。
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100), styleMask: [.titled],
                              backing: .buffered, defer: false)
        let host = NSView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        window.contentView = host
        host.addSubview(view)
        window.appearance = NSAppearance(named: .aqua)
        XCTAssertEqual(view.currentPalette.background, TerminalPalette.kansoDark.background)
        window.appearance = NSAppearance(named: .darkAqua)
        XCTAssertEqual(view.currentPalette.background, TerminalPalette.kansoDark.background)
        settings.updateAppearance { $0.darkTheme = "kanso-light" }
        XCTAssertEqual(view.currentPalette.background, TerminalPalette.kansoLight.background)
    }
}
