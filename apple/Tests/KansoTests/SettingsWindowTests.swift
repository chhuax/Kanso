import AppKit
import XCTest
@testable import Kanso

@MainActor
final class SettingsWindowTests: XCTestCase {
    /// 自动应答的冲突询问：不弹窗。
    final class StubPrompter: KeybindingConflictPrompter {
        var answer = false
        private(set) var asked: [(chord: KeyChord, existing: KeyCommand, new: KeyCommand)] = []
        func confirmReplace(chord: KeyChord, existing: KeyCommand, new: KeyCommand, window: NSWindow?) async -> Bool {
            asked.append((chord, existing, new))
            return answer
        }
    }

    private func withController(
        _ body: (SettingsWindowController, AppearanceController, SettingsStore, () -> [URL]) -> Void
    ) {
        let settings = SettingsStore()
        let appearance = AppearanceController(settings: settings, apply: { _ in })
        let keybindings = KeybindingController(settings: settings, menuProvider: { nil })
        var opened: [URL] = []
        // autosaveName 传 nil：不写用户真实的窗口位置记忆。
        let controller = SettingsWindowController(
            settings: settings, appearance: appearance, keybindings: keybindings,
            openURL: { opened.append($0) }, conflictPrompter: StubPrompter(), autosaveName: nil
        )
        body(controller, appearance, settings, { opened })
    }

    func testPopUpReflectsAndDrivesAppearance() {
        withController { controller, appearance, _, _ in
            let page = controller.generalViewController
            _ = page.view
            XCTAssertEqual(page.appearancePopUp.indexOfSelectedItem, 0)

            // 外部（菜单）切换 → 弹出菜单更新。
            appearance.mode = .dark
            XCTAssertEqual(page.appearancePopUp.indexOfSelectedItem, 2)

            // 弹出菜单切换 → AppearanceController 更新。
            page.appearancePopUp.userSelect(at: 1)
            XCTAssertEqual(appearance.mode, .light)
        }
    }

    func testOpenLanguageSettingsUsesInjectedOpener() {
        withController { controller, _, _, opened in
            let general = controller.generalViewController
            _ = general.view
            general.languageButton.performClick(nil)
            XCTAssertEqual(opened().map(\.absoluteString),
                           ["x-apple.systempreferences:com.apple.Localization-Settings.extension"])
        }
    }

    func testWindowIsNotResizableAndTitled() {
        withController { controller, _, _, _ in
            XCTAssertFalse(controller.window!.styleMask.contains(.resizable))
            XCTAssertEqual(controller.window?.title, String(localized: "General"))
        }
    }

    func testHasThreePagesAndCanSelectEachWithTitle() {
        withController { controller, _, _, _ in
            XCTAssertEqual(controller.tabsInWindow.map(\.title), [
                String(localized: "General"), String(localized: "Display"),
                String(localized: "Keyboard Shortcuts"),
            ])
            for tab in controller.tabsInWindow {
                controller.select(tab)
                XCTAssertEqual(controller.selectedTab, tab)
                XCTAssertEqual(controller.window?.title, tab.title)
            }
        }
    }

    func testWindowHeightFollowsSelectedPage() {
        withController { controller, _, _, _ in
            controller.select(.general)
            let generalHeight = controller.window!.frame.height
            controller.select(.display)
            let displayHeight = controller.window!.frame.height
            controller.select(.keyboard)
            let keyboardHeight = controller.window!.frame.height
            XCTAssertGreaterThan(displayHeight, generalHeight)
            XCTAssertNotEqual(keyboardHeight, displayHeight)
            controller.select(.general)
            XCTAssertEqual(controller.window!.frame.height, generalHeight, accuracy: 0.5)
        }
    }
}
