import AppKit
import CoreText
import KansoBridge
import XCTest
@testable import TerminalKit

/// 手动时钟：代替真实计时器，测试里用 `tick()` 触发一次相位翻转。
@MainActor
private final class ManualBlinkClock {
    private(set) var scheduleCount = 0
    private(set) var cancelCount = 0
    private(set) var intervals: [TimeInterval] = []
    private var handlers: [Int: @MainActor () -> Void] = [:]

    var scheduler: CursorBlinker.Scheduler {
        { [weak self] interval, fire in
            guard let self else { return {} }
            scheduleCount += 1
            intervals.append(interval)
            let id = scheduleCount
            handlers[id] = fire
            return { [weak self] in
                self?.cancelCount += 1
                self?.handlers[id] = nil
            }
        }
    }

    /// 一个周期过去：触发所有仍在运行的计时器。
    func tick() {
        for fire in Array(handlers.values) { fire() }
    }

    var activeTimers: Int { handlers.count }
}

/// 光标闪烁（terminal.md §15）：可见时按 ~530 ms 周期翻转，按键 / 输出重置为可见，失焦停止。
@MainActor
final class CursorBlinkTests: XCTestCase {
    func testToggleEveryIntervalOnlyWhileActive() {
        let clock = ManualBlinkClock()
        let blinker = CursorBlinker(scheduler: clock.scheduler)
        var changes: [Bool] = []
        blinker.onChange = { changes.append($0) }
        XCTAssertTrue(blinker.isVisible)
        XCTAssertEqual(clock.activeTimers, 0, "未激活时不计时")

        blinker.setActive(true)
        XCTAssertEqual(clock.intervals, [0.53])
        clock.tick()
        XCTAssertFalse(blinker.isVisible)
        clock.tick()
        XCTAssertTrue(blinker.isVisible)
        XCTAssertEqual(changes, [false, true])
    }

    func testResetMakesVisibleAndRestartsFullPeriod() {
        let clock = ManualBlinkClock()
        let blinker = CursorBlinker(scheduler: clock.scheduler)
        blinker.setActive(true)
        clock.tick()
        XCTAssertFalse(blinker.isVisible)

        blinker.reset()
        XCTAssertTrue(blinker.isVisible, "有按键 / 输出：立即可见")
        XCTAssertEqual(clock.cancelCount, 1, "旧周期作废")
        XCTAssertEqual(clock.scheduleCount, 2, "重新计时")
        XCTAssertEqual(clock.activeTimers, 1)
        clock.tick()
        XCTAssertFalse(blinker.isVisible)
    }

    func testDeactivateStopsTimerAndForcesVisible() {
        let clock = ManualBlinkClock()
        let blinker = CursorBlinker(scheduler: clock.scheduler)
        var changes: [Bool] = []
        blinker.onChange = { changes.append($0) }
        blinker.setActive(true)
        clock.tick()
        blinker.setActive(false)
        XCTAssertTrue(blinker.isVisible, "失焦后固定为可见")
        XCTAssertEqual(clock.activeTimers, 0)
        XCTAssertEqual(changes, [false, true])
        // 停用后再触发不会翻转。
        clock.tick()
        XCTAssertTrue(blinker.isVisible)
    }

    func testRepeatedStateIsNoOpAndResetWhenInactiveDoesNotSchedule() {
        let clock = ManualBlinkClock()
        let blinker = CursorBlinker(scheduler: clock.scheduler)
        blinker.reset()
        XCTAssertEqual(clock.scheduleCount, 0)
        blinker.setActive(true)
        blinker.setActive(true)
        XCTAssertEqual(clock.scheduleCount, 1, "重复激活不重置相位")
        blinker.setActive(false)
        blinker.setActive(false)
        XCTAssertEqual(clock.cancelCount, 1)
    }

    // MARK: 接入视图

    private func makeView(clock: ManualBlinkClock) -> (TerminalView, MockFrameSource, gate: GateBox) {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 400, height: 200), blinkScheduler: clock.scheduler)
        let gate = GateBox()
        view.blinkGateOverride = { gate.open }
        let source = MockFrameSource(cols: 40, rows: 10)
        view.frameSource = source
        return (view, source, gate)
    }

    private final class GateBox {
        var open = true
    }

    func testViewBlinksOnlyForBlinkingVisibleCursorAndStopsWhenUnfocused() {
        let clock = ManualBlinkClock()
        let (view, source, gate) = makeView(clock: clock)
        // 还没有帧：不闪烁。
        view.updateBlinking()
        XCTAssertEqual(clock.activeTimers, 0)

        // mock 帧的光标可见且要求闪烁（FRAME_CURSOR_BLINK）。
        view.pullFrameIfNeeded()
        XCTAssertEqual(clock.activeTimers, 1)
        clock.tick()
        XCTAssertFalse(view.cursorBlinker.isVisible)
        if let renderer = view.renderer { XCTAssertFalse(renderer.isCursorPhaseVisible) }

        // 新输出：重置为可见。
        source.advance()
        view.setNeedsFrame()
        view.pullFrameIfNeeded()
        XCTAssertTrue(view.cursorBlinker.isVisible)
        if let renderer = view.renderer { XCTAssertTrue(renderer.isCursorPhaseVisible) }

        // 按键：重置为可见（这里让拦截点吞掉按键，只验证闪烁重置）。
        clock.tick()
        XCTAssertFalse(view.cursorBlinker.isVisible)
        view.keyInterceptor = { _ in true }
        view.keyDown(with: InputFixtures.key(.keyDown, VK.a, characters: "a"))
        XCTAssertTrue(view.cursorBlinker.isVisible)

        // 窗口失焦（或视图不可见）：停止闪烁并保持可见。
        clock.tick()
        gate.open = false
        view.updateBlinking()
        XCTAssertEqual(clock.activeTimers, 0)
        XCTAssertTrue(view.cursorBlinker.isVisible)
        if let renderer = view.renderer { XCTAssertTrue(renderer.isCursorPhaseVisible) }
    }

    func testViewStopsTimerWhenRemovedFromWindow() {
        let clock = ManualBlinkClock()
        let (view, _, _) = makeView(clock: clock)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 400, height: 200), styleMask: [.borderless],
                              backing: .buffered, defer: false)
        window.contentView = view
        view.pullFrameIfNeeded()
        XCTAssertEqual(clock.activeTimers, 1)
        // 计时器由 run loop 强持有：视图离开窗口必须显式停掉，否则会话关闭后计时器仍在空转。
        window.contentView = NSView()
        XCTAssertEqual(clock.activeTimers, 0)
    }

    /// 渲染层：隐藏相位不画光标，且不需要新帧。
    func testRendererSkipsCursorInHiddenPhase() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let frame = try XCTUnwrap(FrameSnapshot.mock(cols: 20, rows: 6, seq: 0))
        XCTAssertTrue(frame.flags.contains(.cursorVisible))
        let size = CGSize(width: 400, height: 200)
        renderer.isCursorPhaseVisible = true
        let shown = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .dark))
        renderer.isCursorPhaseVisible = false
        let hidden = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .dark))
        XCTAssertNotEqual(pixels(shown), pixels(hidden))
    }

    // MARK: 配色

    /// 浅色外观绑定深色配色后，渲染结果与"深色外观 + 默认深色配色"逐像素一致。
    func testPaletteBindingPerAppearance() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let frame = try XCTUnwrap(FrameSnapshot.mock(cols: 20, rows: 6, seq: 0))
        let size = CGSize(width: 400, height: 200)
        let darkDefault = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .dark))
        let lightDefault = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .light))
        XCTAssertNotEqual(pixels(darkDefault), pixels(lightDefault))

        renderer.lightPalette = .kansoDark
        let lightWithDark = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .light))
        XCTAssertEqual(pixels(lightWithDark), pixels(darkDefault))
        // 深色外观不受影响。
        let darkAgain = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: 1, appearance: .dark))
        XCTAssertEqual(pixels(darkAgain), pixels(darkDefault))
    }

    func testBuiltInThemeCatalog() {
        XCTAssertEqual(TerminalTheme.builtIn.map(\.id), ["kanso-light", "kanso-dark"])
        XCTAssertEqual(TerminalTheme.builtIn.map(\.name), ["Kanso Light", "Kanso Dark"])
        XCTAssertEqual(TerminalTheme.theme(id: "kanso-dark"), .kansoDark)
        XCTAssertNil(TerminalTheme.theme(id: "nope"))
    }

    private func pixels(_ image: CGImage) -> Data? {
        image.dataProvider?.data as Data?
    }
}

/// Nerd Font 回退（ui-shell.md §10）：主字体缺字形时先试已安装的 Nerd Font，再走系统回退。
/// 字体查找可注入，不依赖本机是否装了 Nerd Font（也没有任何本机字体含私有区的 Powerline 字形）。
@MainActor
final class NerdFontFallbackTests: XCTestCase {
    func testFontDiscoveryWaitsForMissingGlyph() {
        var lookups = 0
        func findFamily() -> String? {
            lookups += 1
            return nil
        }
        let fonts = TerminalFonts(base: .monospacedSystemFont(ofSize: 13, weight: .regular), scale: 2,
                                  nerdFamily: findFamily(), covers: { _, scalar in scalar.value < 128 })
        XCTAssertEqual(lookups, 0)
        _ = fonts.resolve(text: "A", face: .terminal, bold: false, italic: false)
        XCTAssertEqual(lookups, 0)
        _ = fonts.resolve(text: "\u{E0B0}", face: .terminal, bold: false, italic: false)
        _ = fonts.resolve(text: "\u{E0B1}", face: .terminal, bold: false, italic: false)
        XCTAssertEqual(lookups, 1)
    }

    private let powerline: Unicode.Scalar = "\u{E0B0}"

    private func family(_ font: CTFont) -> String {
        CTFontCopyFamilyName(font) as String
    }

    /// 假装"只有 Menlo 这套 Nerd Font 含私有区字形"：其余字形按真实字体查询。
    private let coverage: (CTFont, Unicode.Scalar) -> Bool = { font, scalar in
        if scalar.value == 0xE0B0 { return (CTFontCopyFamilyName(font) as String) == "Menlo" }
        return TerminalFonts.coverage(font, scalar)
    }

    private func makeFonts(nerd: String?) -> TerminalFonts {
        let base = NSFont(name: "Monaco", size: 13) ?? .monospacedSystemFont(ofSize: 13, weight: .regular)
        return TerminalFonts(base: base, scale: 2, nerdFamily: nerd, covers: coverage)
    }

    func testPrivateUseGlyphUsesInstalledNerdFont() {
        let fonts = makeFonts(nerd: "Menlo")
        let result = fonts.resolve(text: String(powerline), face: .terminal, bold: false, italic: false)
        XCTAssertEqual(family(result.font), "Menlo")
        XCTAssertFalse(result.isColor)
        // 回退字体按终端字号取（像素字号随倍率）。
        XCTAssertEqual(CTFontGetSize(result.font), 26)
    }

    func testBoldVariantAndCachingReturnSameFontForSameKey() {
        let fonts = makeFonts(nerd: "Menlo")
        let a = fonts.resolve(text: String(powerline), face: .terminal, bold: false, italic: false)
        let b = fonts.resolve(text: String(powerline), face: .terminal, bold: false, italic: false)
        XCTAssertTrue(CFEqual(a.font, b.font))
        let bold = fonts.resolve(text: String(powerline), face: .terminal, bold: true, italic: false)
        XCTAssertEqual(family(bold.font), "Menlo")
    }

    func testWithoutNerdFontPrivateUseGlyphDoesNotUseIt() {
        let fonts = makeFonts(nerd: nil)
        let result = fonts.resolve(text: String(powerline), face: .terminal, bold: false, italic: false)
        XCTAssertNotEqual(family(result.font), "Menlo")
    }

    func testPrimaryFontWinsWhenItHasTheGlyph() {
        let fonts = makeFonts(nerd: "Menlo")
        let result = fonts.resolve(text: "a", face: .terminal, bold: false, italic: false)
        XCTAssertEqual(family(result.font), "Monaco", "主字体有的字形不走 Nerd Font")
    }

    func testNerdFontIsTriedBeforeSystemFallback() {
        // 假装 Nerd Font 也含这个 CJK 字符：应优先于系统回退（Hiragino / PingFang）。
        let cover: (CTFont, Unicode.Scalar) -> Bool = { font, scalar in
            scalar.value == 0x3042 ? (CTFontCopyFamilyName(font) as String) == "Menlo" : TerminalFonts.coverage(font, scalar)
        }
        let base = NSFont(name: "Monaco", size: 13)!
        let fonts = TerminalFonts(base: base, scale: 1, nerdFamily: "Menlo", covers: cover)
        let result = fonts.resolve(text: "あ", face: .terminal, bold: false, italic: false)
        XCTAssertEqual(family(result.font), "Menlo")
    }

    func testRealLookupDoesNotCrashAndHasConsistentDefault() {
        // 默认查找（真实枚举本机字体）：结果因机器而异，只确认能工作且符合"含 Nerd Font"的命名规则。
        if let installed = TerminalFonts.installedNerdFamily { XCTAssertTrue(installed.contains("Nerd Font")) }
    }
}
