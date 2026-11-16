import KansoBridge
import KansoUI
import TerminalKit
import XCTest
@testable import Kanso

/// 设置的编解码、迁移、持久化与恢复默认（store-import.md §9）。全部用内存后端或 mock core，不碰真实存储。
@MainActor
final class SettingsStoreTests: XCTestCase {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) -> T? {
        SettingsCodec.decode(type, from: json)
    }

    // MARK: 编解码

    func testUnknownFieldsAreIgnored() throws {
        let json = #"""
        {"v":1,"fontFamily":"Menlo","fontSize":15,"cursorShape":"bar","cursorBlink":true,
         "scrollbackLines":5000,"minimumContrastEnabled":false,"minimumContrastRatio":7,
         "futureField":{"nested":[1,2,3]},"anotherOne":"x"}
        """#
        let display = try XCTUnwrap(decode(DisplaySettings.self, json))
        XCTAssertEqual(display.fontFamily, "Menlo")
        XCTAssertEqual(display.fontSize, 15)
        XCTAssertEqual(display.cursorShape, .bar)
        XCTAssertTrue(display.cursorBlink)
        XCTAssertEqual(display.scrollbackLines, 5000)
        XCTAssertFalse(display.minimumContrastEnabled)
        XCTAssertEqual(display.minimumContrastRatio, 7)

        XCTAssertNotNil(decode(AppearanceSettings.self, #"{"v":1,"mode":"dark","extra":true}"#))
        XCTAssertNotNil(decode(TimelineSettings.self, #"{"v":1,"mode":"off","extra":[]}"#))
        XCTAssertNotNil(decode(KeybindingSettings.self, #"{"v":1,"bindings":{},"extra":0}"#))
    }

    func testMissingFieldsUseDefaults() throws {
        XCTAssertEqual(try XCTUnwrap(decode(DisplaySettings.self, "{}")), DisplaySettings())
        let partial = try XCTUnwrap(decode(DisplaySettings.self, #"{"fontSize":20}"#))
        XCTAssertEqual(partial.fontSize, 20)
        XCTAssertEqual(partial.lineHeight, 1.2)
        XCTAssertEqual(partial.scrollbackLines, DisplaySettings().scrollbackLines)
        XCTAssertEqual(try XCTUnwrap(decode(AppearanceSettings.self, "{}")), AppearanceSettings())
        XCTAssertEqual(try XCTUnwrap(decode(TimelineSettings.self, "{}")), TimelineSettings())
        XCTAssertEqual(try XCTUnwrap(decode(KeybindingSettings.self, "{}")), KeybindingSettings())
    }

    /// 单个字段类型不对只影响它自己，不会让整个分区回落默认值，更不会报错。
    func testWrongTypedFieldFallsBackToItsDefaultOnly() throws {
        let display = try XCTUnwrap(decode(
            DisplaySettings.self, #"{"fontSize":"big","cursorShape":42,"scrollbackLines":3000,"cursorBlink":true}"#
        ))
        XCTAssertEqual(display.fontSize, DisplaySettings().fontSize)
        XCTAssertEqual(display.cursorShape, DisplaySettings().cursorShape)
        XCTAssertEqual(display.scrollbackLines, 3000)
        XCTAssertTrue(display.cursorBlink)
    }

    func testOutOfRangeValuesAreClamped() throws {
        let low = try XCTUnwrap(decode(
            DisplaySettings.self, #"{"fontSize":1,"lineHeight":0.5,"scrollbackLines":5,"minimumContrastRatio":0}"#
        ))
        XCTAssertEqual(low.fontSize, 9)
        XCTAssertEqual(low.lineHeight, 1)
        XCTAssertEqual(low.scrollbackLines, 1_000)
        XCTAssertEqual(low.minimumContrastRatio, 1)
        let high = try XCTUnwrap(decode(
            DisplaySettings.self, #"{"fontSize":500,"lineHeight":3,"scrollbackLines":99999999,"minimumContrastRatio":99}"#
        ))
        XCTAssertEqual(high.fontSize, 36)
        XCTAssertEqual(high.lineHeight, 2)
        XCTAssertEqual(high.scrollbackLines, 1_000_000)
        XCTAssertEqual(high.minimumContrastRatio, 21)
    }

    func testLineHeightRoundTripsAndRejectsWrongType() throws {
        var display = DisplaySettings()
        display.lineHeight = 1.35
        let encoded = try JSONEncoder().encode(display)
        XCTAssertEqual(try JSONDecoder().decode(DisplaySettings.self, from: encoded).lineHeight, 1.35)
        XCTAssertEqual(try XCTUnwrap(decode(DisplaySettings.self, #"{"lineHeight":"bad"}"#)).lineHeight, 1.2)
    }

    func testNonObjectJsonDecodesToNilSoStoreUsesDefaults() {
        XCTAssertNil(decode(DisplaySettings.self, "[1,2,3]"))
        XCTAssertNil(decode(DisplaySettings.self, "not json"))
        let backend = MemorySettingsBackend()
        backend.values["display"] = "[1,2,3]"
        backend.values["appearance"] = "garbage"
        let store = SettingsStore.attached(to: backend)
        XCTAssertEqual(store.display, DisplaySettings())
        XCTAssertEqual(store.appearance, AppearanceSettings())
    }

    func testWrittenSettingsCarryVersionAndRoundTrip() throws {
        var display = DisplaySettings()
        display.fontFamily = "JetBrains Mono"
        display.cursorShape = .underline
        let json = SettingsCodec.encode(display)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        XCTAssertEqual(object["v"] as? Int, SettingsCodec.version)
        XCTAssertEqual(try XCTUnwrap(decode(DisplaySettings.self, json)), display)

        var appearance = AppearanceSettings()
        appearance.mode = .dark
        appearance.lightTheme = "kanso-dark"
        XCTAssertEqual(try XCTUnwrap(decode(AppearanceSettings.self, SettingsCodec.encode(appearance))), appearance)

        var timeline = TimelineSettings()
        timeline.mode = .lineNumbers
        timeline.lastVisible = .timestamps
        XCTAssertEqual(try XCTUnwrap(decode(TimelineSettings.self, SettingsCodec.encode(timeline))), timeline)

        var keys = KeybindingSettings()
        keys.overrides = ["copy": "ctrl+shift+c", "find": ""]
        XCTAssertEqual(try XCTUnwrap(decode(KeybindingSettings.self, SettingsCodec.encode(keys))), keys)
    }

    /// 新版本写的设置（版本号更大、带未知字段）旧版本照样能读已知字段。
    func testNewerVersionStillReadable() throws {
        let display = try XCTUnwrap(decode(DisplaySettings.self, #"{"v":99,"fontSize":20,"newThing":{"a":1}}"#))
        XCTAssertEqual(display.fontSize, 20)
    }

    func testUnknownThemeIdIsKeptButResolvesToDefault() throws {
        let appearance = try XCTUnwrap(decode(AppearanceSettings.self, #"{"lightTheme":"solarized","darkTheme":"kanso-light"}"#))
        XCTAssertEqual(appearance.lightTheme, "solarized")
        XCTAssertEqual(appearance.resolvedLightTheme, .kansoLight)
        XCTAssertEqual(appearance.resolvedDarkTheme, .kansoLight)
    }

    // MARK: 持久化与通知

    func testUpdatesAreWrittenToBackendUnderSectionKeys() {
        let backend = MemorySettingsBackend()
        let store = SettingsStore.attached(to: backend)
        store.updateDisplay { $0.fontSize = 18 }
        store.updateAppearance { $0.mode = .dark }
        store.updateTimeline { $0.mode = .lineNumbers }
        store.updateKeybindings { $0.overrides["copy"] = "" }
        XCTAssertEqual(Set(backend.values.keys), ["display", "appearance", "timeline", "keybindings"])
        let reloaded = SettingsStore.attached(to: backend)
        XCTAssertEqual(reloaded.display.fontSize, 18)
        XCTAssertEqual(reloaded.appearance.mode, .dark)
        XCTAssertEqual(reloaded.timeline.mode, .lineNumbers)
        XCTAssertEqual(reloaded.keybindings.overrides["copy"], "")
    }

    func testUnchangedUpdateDoesNotWriteOrNotify() {
        let backend = MemorySettingsBackend()
        let store = SettingsStore.attached(to: backend)
        var seen: [SettingsSection] = []
        let owner = NSObject()
        store.observe(owner) { seen.append($0) }
        store.updateDisplay { $0.fontSize = DisplaySettings().fontSize }
        XCTAssertTrue(seen.isEmpty)
        XCTAssertNil(backend.values["display"])
        store.updateDisplay { $0.fontSize = 20 }
        XCTAssertEqual(seen, [.display])
    }

    func testWriteFailureKeepsInMemoryValue() {
        let backend = MemorySettingsBackend()
        let store = SettingsStore.attached(to: backend)
        backend.failWrites = true
        store.updateDisplay { $0.fontSize = 22 }
        XCTAssertEqual(store.display.fontSize, 22)
    }

    func testTerminalOptionsArePushedOnAttachAndOnDisplayChange() {
        let backend = MemorySettingsBackend()
        backend.values["display"] = #"{"v":1,"scrollbackLines":2000,"cursorShape":"bar","cursorBlink":true}"#
        let store = SettingsStore.attached(to: backend)
        // 载入后立即把用户保存的选项下发给 core：core 的默认值未必与它一致。
        XCTAssertEqual(backend.terminalOptionsLog.last, FfiTerminalOptions(scrollbackLines: 2000, cursorShape: .bar, cursorBlink: true, semanticColoring: true))
        store.updateDisplay { $0.scrollbackLines = 50_000; $0.cursorShape = .underline; $0.cursorBlink = false }
        XCTAssertEqual(backend.terminalOptionsLog.last,
                       FfiTerminalOptions(scrollbackLines: 50_000, cursorShape: .underline, cursorBlink: false, semanticColoring: true))
        // 只改字体不影响终端选项，但重复下发无害；这里只确认值仍是最新的。
        store.updateDisplay { $0.fontSize = 20 }
        XCTAssertEqual(backend.terminalOptionsLog.last?.scrollbackLines, 50_000)
    }

    // MARK: UserDefaults 迁移

    private func withLegacyDefaults(_ body: (UserDefaults) throws -> Void) rethrows {
        let name = "com.chhuax.kanso.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        try body(defaults)
    }

    func testMigratesLegacyDefaultsWhenCoreHasNothing() {
        withLegacyDefaults { defaults in
            defaults.set("dark", forKey: SettingsStore.LegacyKey.appearanceMode)
            defaults.set("off", forKey: SettingsStore.LegacyKey.gutterMode)
            defaults.set("timestamps", forKey: SettingsStore.LegacyKey.gutterLastVisible)
            let backend = MemorySettingsBackend()
            let store = SettingsStore()
            store.attach(backend: backend, legacyDefaults: defaults)

            XCTAssertEqual(store.appearance.mode, .dark)
            XCTAssertEqual(store.timeline.mode, .off)
            XCTAssertEqual(store.timeline.lastVisible, .timestamps)
            // 已写进 core 设置。
            XCTAssertNotNil(backend.values["appearance"])
            XCTAssertNotNil(backend.values["timeline"])
            // 旧 key 迁移后删除：否则恢复默认之后下次启动又会被迁回来。
            for key in [SettingsStore.LegacyKey.appearanceMode, SettingsStore.LegacyKey.gutterMode,
                        SettingsStore.LegacyKey.gutterLastVisible] {
                XCTAssertNil(defaults.object(forKey: key), key)
            }
        }
    }

    func testCoreWinsOverLegacyDefaults() {
        withLegacyDefaults { defaults in
            defaults.set("dark", forKey: SettingsStore.LegacyKey.appearanceMode)
            defaults.set("off", forKey: SettingsStore.LegacyKey.gutterMode)
            let backend = MemorySettingsBackend()
            backend.values["appearance"] = #"{"v":1,"mode":"light"}"#
            backend.values["timeline"] = #"{"v":1,"mode":"lineNumbers"}"#
            let store = SettingsStore()
            store.attach(backend: backend, legacyDefaults: defaults)
            XCTAssertEqual(store.appearance.mode, .light)
            XCTAssertEqual(store.timeline.mode, .lineNumbers)
        }
    }

    func testMigrationIsOneShotAndSurvivesRestartAfterReset() throws {
        try withLegacyDefaults { defaults in
            defaults.set("dark", forKey: SettingsStore.LegacyKey.appearanceMode)
            let backend = MemorySettingsBackend()
            let first = SettingsStore()
            first.attach(backend: backend, legacyDefaults: defaults)
            XCTAssertEqual(first.appearance.mode, .dark)

            // 用户恢复默认设置，然后重启：不应再从（已删除的）旧偏好迁移出"深色"。
            try first.resetToDefaults()
            let second = SettingsStore()
            second.attach(backend: backend, legacyDefaults: defaults)
            XCTAssertEqual(second.appearance.mode, .system)
        }
    }

    func testInvalidLegacyValuesFallBackToDefaults() {
        withLegacyDefaults { defaults in
            defaults.set("sepia", forKey: SettingsStore.LegacyKey.appearanceMode)
            defaults.set("bogus", forKey: SettingsStore.LegacyKey.gutterMode)
            let store = SettingsStore()
            store.attach(backend: MemorySettingsBackend(), legacyDefaults: defaults)
            XCTAssertEqual(store.appearance.mode, .system)
            XCTAssertEqual(store.timeline.mode, .off)
        }
    }

    // MARK: 恢复默认

    func testResetClearsSettingsNotifiesAndRestoresTerminalOptions() throws {
        let backend = MemorySettingsBackend()
        let store = SettingsStore.attached(to: backend)
        store.updateDisplay { $0.fontSize = 20; $0.scrollbackLines = 77_000 }
        store.updateAppearance { $0.mode = .dark }
        store.updateKeybindings { $0.overrides["copy"] = "ctrl+shift+c" }
        var seen: Set<SettingsSection> = []
        let owner = NSObject()
        store.observe(owner) { seen.insert($0) }

        try store.resetToDefaults()
        XCTAssertEqual(store.display, DisplaySettings())
        XCTAssertEqual(store.appearance, AppearanceSettings())
        XCTAssertEqual(store.keybindings, KeybindingSettings())
        XCTAssertTrue(backend.values.isEmpty)
        XCTAssertEqual(seen, Set(SettingsSection.allCases))
        XCTAssertEqual(backend.terminalOptionsLog.last, DisplaySettings().terminalOptions)
    }

    /// 恢复默认设置不动会话数据（用 mock core 的真实设置表与数据表验证，store-import.md §9）。
    func testResetLeavesProfilesGroupsAndSnippetsUntouched() throws {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        let profilesBefore = core.profiles()
        let groupsBefore = core.groups()
        XCTAssertFalse(profilesBefore.isEmpty)

        let store = SettingsStore()
        store.attach(backend: core)
        store.updateDisplay { $0.fontSize = 24 }
        store.updateKeybindings { $0.overrides["find"] = "" }
        XCTAssertNotNil(core.setting(key: "display"))

        try store.resetToDefaults()
        XCTAssertNil(core.setting(key: "display"))
        XCTAssertNil(core.setting(key: "keybindings"))
        XCTAssertEqual(core.profiles().map(\.id), profilesBefore.map(\.id))
        XCTAssertEqual(core.groups().map(\.id), groupsBefore.map(\.id))
        XCTAssertEqual(store.display.fontSize, DisplaySettings().fontSize)
    }

    func testResetFlowOnlyResetsAfterConfirmation() async throws {
        let backend = MemorySettingsBackend()
        let store = SettingsStore.attached(to: backend)
        store.updateDisplay { $0.fontSize = 20 }

        let cancelled = await SettingsResetFlow.run(settings: store, confirm: { false }, onFailure: { _ in XCTFail() })
        XCTAssertFalse(cancelled)
        XCTAssertEqual(store.display.fontSize, 20)

        let done = await SettingsResetFlow.run(settings: store, confirm: { true }, onFailure: { _ in XCTFail() })
        XCTAssertTrue(done)
        XCTAssertEqual(store.display.fontSize, DisplaySettings().fontSize)
    }

    func testResetFailureIsReportedAndKeepsSettings() async {
        let failing = FailingResetBackend()
        let store = SettingsStore.attached(to: failing)
        store.updateDisplay { $0.fontSize = 20 }
        var reported = false
        let done = await SettingsResetFlow.run(settings: store, confirm: { true }, onFailure: { _ in reported = true })
        XCTAssertFalse(done)
        XCTAssertTrue(reported)
        XCTAssertEqual(store.display.fontSize, 20)
    }
}

private final class FailingResetBackend: SettingsBackend, @unchecked Sendable {
    struct Failure: Error {}
    func setting(key: String) -> String? { nil }
    func setSetting(key: String, json: String) throws {}
    func resetSettings() throws { throw Failure() }
    func setTerminalOptions(options: FfiTerminalOptions) {}
}
