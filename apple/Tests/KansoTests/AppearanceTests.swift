import AppKit
import TerminalKit
import XCTest
@testable import Kanso

@MainActor
final class AppearanceTests: XCTestCase {
    func testDefaultIsSystem() {
        XCTAssertEqual(AppearanceController(apply: { _ in }).mode, .system)
    }

    func testInvalidStoredValueFallsBackToSystem() {
        let backend = MemorySettingsBackend()
        backend.values["appearance"] = #"{"v":1,"mode":"sepia"}"#
        let settings = SettingsStore.attached(to: backend)
        XCTAssertEqual(AppearanceController(settings: settings, apply: { _ in }).mode, .system)
    }

    func testSetModePersistsAndApplies() {
        let backend = MemorySettingsBackend()
        var applied: [NSAppearance.Name?] = []
        let controller = AppearanceController(settings: .attached(to: backend), apply: { applied.append($0?.name) })
        // attach 时已按载入的值（默认 system）应用过一次；这里只看之后的切换。
        applied.removeAll()
        controller.mode = .dark
        controller.mode = .light
        controller.mode = .system
        XCTAssertEqual(applied, [.darkAqua, .aqua, nil])
        controller.mode = .dark
        // 重启后从 core 设置恢复。
        XCTAssertEqual(AppearanceController(settings: .attached(to: backend), apply: { _ in }).mode, .dark)
    }

    /// 设置载入（core 创建之后）时应用已保存的模式：窗口第一次绘制前就是正确的外观。
    func testAttachAppliesStoredMode() {
        let backend = MemorySettingsBackend()
        backend.values["appearance"] = #"{"v":1,"mode":"dark"}"#
        let settings = SettingsStore()
        var applied: [NSAppearance.Name?] = []
        // 控制器要被持有：设置存储只弱引用观察者。
        let controller = AppearanceController(settings: settings, apply: { applied.append($0?.name) })
        XCTAssertTrue(applied.isEmpty)
        settings.attach(backend: backend)
        XCTAssertEqual(applied.last ?? nil, .darkAqua)
        XCTAssertEqual(controller.mode, .dark)
    }

    func testApplyStoredMode() {
        let backend = MemorySettingsBackend()
        backend.values["appearance"] = #"{"v":1,"mode":"dark"}"#
        var applied: [NSAppearance.Name?] = []
        let controller = AppearanceController(settings: .attached(to: backend), apply: { applied.append($0?.name) })
        applied.removeAll()
        controller.applyStoredMode()
        XCTAssertEqual(applied, [.darkAqua])
    }

    func testMenuCheckmarksAndSelection() {
        let controller = AppearanceController(apply: { _ in })
        let menu = controller.makeMenu()
        XCTAssertEqual(menu.items.count, 3)
        XCTAssertEqual(menu.items.map(\.state), [.on, .off, .off])

        _ = controller.perform(#selector(AppearanceController.selectAppearance(_:)), with: menu.items[2])
        XCTAssertEqual(controller.mode, .dark)
        // 勾选在校验时刷新（菜单显示前系统会调 validateMenuItem）。
        for item in menu.items { _ = controller.validateMenuItem(item) }
        XCTAssertEqual(menu.items.map(\.state), [.off, .off, .on])
    }

    func testObserverNotifiedOnChange() {
        let controller = AppearanceController(apply: { _ in })
        let owner = NSObject()
        var seen: [AppearanceMode] = []
        controller.observe(owner) { seen.append($0) }
        controller.mode = .light
        XCTAssertEqual(seen, [.light])
    }

    /// 实时生效的链路由 effectiveAppearance 驱动：给窗口（不是 NSApp）设外观，视图与终端配色随之变化。
    func testEffectiveAppearanceDrivesTerminalAppearance() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
            styleMask: [.titled], backing: .buffered, defer: false
        )
        let view = NSView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        window.contentView = view

        window.appearance = NSAppearance(named: .darkAqua)
        XCTAssertEqual(view.effectiveAppearance.name, .darkAqua)
        XCTAssertEqual(TerminalAppearance(view.effectiveAppearance), .dark)

        window.appearance = NSAppearance(named: .aqua)
        XCTAssertEqual(view.effectiveAppearance.name, .aqua)
        XCTAssertEqual(TerminalAppearance(view.effectiveAppearance), .light)
    }
}
