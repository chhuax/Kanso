import AppKit
import KansoBridge
import KansoUI
import TerminalKit
import XCTest
@testable import Kanso

@MainActor
final class GutterModeTests: XCTestCase {
    private var controllers: [WorkspaceController] = []

    /// 内存设置后端 + 已连接的存储。不碰真实存储。
    private func withSettings(_ body: (SettingsStore, MemorySettingsBackend) throws -> Void) rethrows {
        let backend = MemorySettingsBackend()
        try body(SettingsStore.attached(to: backend), backend)
    }

    override func tearDown() async throws {
        for c in controllers { c.shutdown() }
        controllers = []
    }

    // MARK: 持久化

    func testDefaultIsOff() {
        withSettings { settings, _ in
            let controller = GutterModeController(settings: settings)
            XCTAssertEqual(controller.mode, .off)
            // 第一次打开显示两者。
            controller.toggleTimeline()
            XCTAssertEqual(controller.mode, .both)
        }
    }

    func testInvalidStoredValueFallsBackToOff() {
        let backend = MemorySettingsBackend()
        backend.values["timeline"] = #"{"v":1,"mode":"bogus"}"#
        XCTAssertEqual(GutterModeController(settings: .attached(to: backend)).mode, .off)
    }

    func testModePersistsAcrossInstances() {
        withSettings { settings, backend in
            let controller = GutterModeController(settings: settings)
            for mode in GutterMode.allCases {
                controller.mode = mode
                XCTAssertEqual(GutterModeController(settings: .attached(to: backend)).mode, mode)
            }
            XCTAssertTrue(backend.values["timeline"]?.contains(#""mode":"both""#) == true)
        }
    }

    func testObserverFiresOnlyOnChange() {
        withSettings { settings, _ in
            let controller = GutterModeController(settings: settings)
            var seen: [GutterMode] = []
            controller.observe(self) { seen.append($0) }
            controller.mode = .off
            controller.mode = .timestamps
            controller.mode = .timestamps
            XCTAssertEqual(seen, [.timestamps])
        }
    }

    func testFfiMapping() {
        XCTAssertEqual(GutterMode.off.ffiMode, .off)
        XCTAssertEqual(GutterMode.lineNumbers.ffiMode, .lineNumbers)
        XCTAssertEqual(GutterMode.timestamps.ffiMode, .time)
        XCTAssertEqual(GutterMode.both.ffiMode, .both)
    }

    // MARK: 时间轴开关

    func testToggleTimelineRestoresLastVisibleMode() {
        withSettings { settings, _ in
            let controller = GutterModeController(settings: settings)
            XCTAssertFalse(controller.isTimelineOn)
            controller.mode = .lineNumbers
            controller.toggleTimeline()
            XCTAssertEqual(controller.mode, .off)
            XCTAssertFalse(controller.isTimelineOn)
            controller.toggleTimeline()
            XCTAssertEqual(controller.mode, .lineNumbers)
        }
    }

    func testLastVisibleModePersistsAcrossInstances() {
        withSettings { settings, backend in
            let first = GutterModeController(settings: settings)
            first.mode = .timestamps
            first.toggleTimeline()
            XCTAssertEqual(first.mode, .off)
            // 模拟重启：关闭状态下重建，再打开应恢复关闭前的模式。
            let second = GutterModeController(settings: .attached(to: backend))
            XCTAssertEqual(second.mode, .off)
            second.toggleTimeline()
            XCTAssertEqual(second.mode, .timestamps)
        }
    }

    func testInvalidLastVisibleFallsBackToBoth() {
        for last in ["bogus", "off"] {
            let backend = MemorySettingsBackend()
            backend.values["timeline"] = #"{"v":1,"mode":"off","lastVisible":"\#(last)"}"#
            XCTAssertEqual(GutterModeController(settings: .attached(to: backend)).lastVisibleMode, .both)
        }
    }

    func testStatusBarTextOnlyForAbnormalConnection() {
        func text(_ c: TabConnection) -> String {
            TabDescriptions.statusBarText(for: TabItem(id: 1, kind: .local, name: "L", cwd: "/tmp", connection: c))
        }
        XCTAssertEqual(text(.connected), "")
        XCTAssertFalse(text(.connecting).isEmpty)
        XCTAssertEqual(text(.closed(reason: "Exited with code 1")), "Exited with code 1")
        XCTAssertFalse(text(.closed(reason: nil)).isEmpty)
        XCTAssertEqual(TabDescriptions.statusBarText(for: nil), "")
    }

    func testToggleFromStoredOffRestoresBoth() {
        let backend = MemorySettingsBackend()
        backend.values["timeline"] = #"{"v":1,"mode":"off"}"#
        do {
            let controller = GutterModeController(settings: .attached(to: backend))
            controller.toggleTimeline()
            XCTAssertEqual(controller.mode, .both)
        }
    }

    func testStatusBarTimelineItemTogglesController() {
        withSettings { settings, _ in
            let controller = GutterModeController(settings: settings)
            let bar = StatusBarView(frame: NSRect(x: 0, y: 0, width: 800, height: 28))
            bar.onToggleTimeline = { controller.toggleTimeline() }
            controller.observe(bar) { [weak bar] in bar?.setTimelineOn($0 != .off) }
            bar.setTimelineOn(controller.isTimelineOn)
            XCTAssertFalse(bar.isTimelineOn)
            bar.clickTimelineItem()
            XCTAssertEqual(controller.mode, .both)
            XCTAssertTrue(bar.isTimelineOn)
            bar.clickTimelineItem()
            XCTAssertEqual(controller.mode, .off)
            XCTAssertFalse(bar.isTimelineOn)
        }
    }

    // MARK: 菜单

    func testMenuHasFourItemsInOrderAndChecksCurrent() {
        withSettings { settings, _ in
            let controller = GutterModeController(settings: settings)
            let menu = controller.makeMenu()
            XCTAssertEqual(menu.items.count, 4)
            XCTAssertEqual(menu.items.map(\.state), [.on, .off, .off, .off])

            _ = controller.perform(#selector(GutterModeController.selectMode(_:)), with: menu.items[1])
            XCTAssertEqual(controller.mode, .lineNumbers)
            // 外部改了模式（如状态栏开关）后，校验时刷新勾选。
            controller.mode = .off
            for item in menu.items { _ = controller.validateMenuItem(item) }
            XCTAssertEqual(menu.items.map(\.state), [.on, .off, .off, .off])
        }
    }

    // MARK: 应用到视图

    func testModeAppliesToExistingAndNewViews() throws {
        try withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            let dispatcher = CoreEventDispatcher()
            let core = KansoCore.newMock(listener: dispatcher)
            let controller = WorkspaceController(
                core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true), gutter: gutter
            )
            controllers.append(controller)
            let first = try controller.openLocalShell()
            let firstView = try XCTUnwrap(controller.host(for: first)).view
            // 无 Metal 设备时视图没有渲染器，gutterMode 读回恒为默认值，无法验证。
            try XCTSkipIf(firstView.renderer == nil, "没有 Metal 设备")

            gutter.mode = .timestamps
            XCTAssertEqual(firstView.gutterMode, .timestamps)

            let second = try controller.split(axis: .horizontal)
            let secondView = try XCTUnwrap(controller.host(for: second)).view
            XCTAssertEqual(secondView.gutterMode, .timestamps)

            gutter.mode = .off
            XCTAssertEqual(firstView.gutterMode, .off)
            XCTAssertEqual(secondView.gutterMode, .off)
        }
    }

    func testNewViewUsesStoredModeAtLaunch() throws {
        let backend = MemorySettingsBackend()
        backend.values["timeline"] = #"{"v":1,"mode":"lineNumbers"}"#
        do {
            let settings = SettingsStore.attached(to: backend)
            let dispatcher = CoreEventDispatcher()
            let core = KansoCore.newMock(listener: dispatcher)
            let controller = WorkspaceController(
                core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true),
                gutter: GutterModeController(settings: settings), settings: settings
            )
            controllers.append(controller)
            let id = try controller.openLocalShell()
            let view = try XCTUnwrap(controller.host(for: id)).view
            try XCTSkipIf(view.renderer == nil, "没有 Metal 设备")
            XCTAssertEqual(view.gutterMode, .lineNumbers)
        }
    }

    // MARK: AI 会话的时间轴（terminal.md §2、ui-shell.md §9）

    func testAgentSessionDefaultsToHiddenWithoutTouchingGlobal() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            gutter.mode = .timestamps
            XCTAssertEqual(gutter.effectiveMode(for: 1), .timestamps)

            XCTAssertEqual(gutter.agentReported(session: 1, agent: .claude), .off)
            XCTAssertEqual(gutter.override(for: 1), .off)
            XCTAssertEqual(gutter.effectiveMode(for: 1), .off)
            XCTAssertFalse(gutter.isTimelineOn(for: 1))
            // 全局与其他会话不受影响。
            XCTAssertEqual(gutter.mode, .timestamps)
            XCTAssertEqual(gutter.effectiveMode(for: 2), .timestamps)
            XCTAssertNil(gutter.override(for: 2))
            XCTAssertTrue(gutter.isTimelineOn(for: nil))
        }
    }

    func testSameAgentRepeatedReportKeepsUserChoice() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            XCTAssertEqual(gutter.agentReported(session: 1, agent: .codex), .off)
            // 用户在这个会话里打开时间轴：只改这个会话，恢复上次非关闭的模式。
            XCTAssertEqual(gutter.toggleTimeline(for: 1), .both)
            XCTAssertEqual(gutter.mode, .off, "全局不受影响")
            // agentIdle ↔ agentWorking 反复上报同一个工具：保留用户选择，不返回新模式。
            XCTAssertNil(gutter.agentReported(session: 1, agent: .codex))
            XCTAssertNil(gutter.agentReported(session: 1, agent: .codex))
            XCTAssertEqual(gutter.override(for: 1), .both)
            // 再点一次关掉。
            XCTAssertEqual(gutter.toggleTimeline(for: 1), .off)
        }
    }

    func testToggleInAgentSessionOnlyAffectsThatSession() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            gutter.mode = .lineNumbers
            var globalChanges: [GutterMode] = []
            gutter.observe(self) { globalChanges.append($0) }
            _ = gutter.agentReported(session: 1, agent: .claude)

            XCTAssertEqual(gutter.toggleTimeline(for: 1), .lineNumbers, "恢复全局记住的非关闭模式")
            XCTAssertEqual(gutter.override(for: 1), .lineNumbers)
            XCTAssertTrue(globalChanges.isEmpty, "AI 会话里的开关不能改全局")
            XCTAssertEqual(gutter.mode, .lineNumbers)

            // 没有临时设置的会话 / 没有活动会话：切换全局。
            XCTAssertNil(gutter.toggleTimeline(for: 2))
            XCTAssertEqual(gutter.mode, .off)
            XCTAssertNil(gutter.toggleTimeline(for: nil))
            XCTAssertEqual(gutter.mode, .lineNumbers)
            XCTAssertEqual(globalChanges, [.off, .lineNumbers])
        }
    }

    func testAgentExitDropsOverrideAndReturnsToGlobal() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            gutter.mode = .timestamps
            _ = gutter.agentReported(session: 1, agent: .gemini)
            _ = gutter.toggleTimeline(for: 1)
            XCTAssertEqual(gutter.agentEnded(session: 1), .timestamps)
            XCTAssertNil(gutter.override(for: 1))
            XCTAssertEqual(gutter.effectiveMode(for: 1), .timestamps)
            // 普通会话反复上报 idle / commandRunning：什么也不用做。
            XCTAssertNil(gutter.agentEnded(session: 1))
            XCTAssertNil(gutter.agentEnded(session: 9))
            // 退出后再次启动工具：重新默认隐藏（不沿用上次的用户选择）。
            XCTAssertEqual(gutter.agentReported(session: 1, agent: .gemini), .off)
        }
    }

    func testSwitchingToAnotherAgentResetsTemporarySetting() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            _ = gutter.agentReported(session: 1, agent: .claude)
            _ = gutter.toggleTimeline(for: 1)
            XCTAssertEqual(gutter.override(for: 1), .both)
            XCTAssertEqual(gutter.agentReported(session: 1, agent: .codex), .off)
            XCTAssertEqual(gutter.override(for: 1), .off)
        }
    }

    func testGlobalChangeDoesNotTouchAgentSession() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            _ = gutter.agentReported(session: 1, agent: .claude)
            gutter.mode = .lineNumbers
            XCTAssertEqual(gutter.effectiveMode(for: 1), .off)
            XCTAssertEqual(gutter.effectiveMode(for: 2), .lineNumbers)
        }
    }

    func testForgetSessionDropsOverride() {
        withSettings { settings, _ in
            let gutter = GutterModeController(settings: settings)
            _ = gutter.agentReported(session: 1, agent: .claude)
            gutter.forget(session: 1)
            XCTAssertNil(gutter.override(for: 1))
        }
    }

    /// 控制器层：投递 core 事件，临时设置随活动变化，并且只作用于该会话。
    func testControllerAppliesAgentActivityToSessionOnly() async throws {
        do {
            let gutter = GutterModeController(settings: SettingsStore.attached(to: MemorySettingsBackend()))
            // 全局设为可见模式，才能区分"会话临时关闭"和"全局本来就关"。
            gutter.mode = .both
            let dispatcher = CoreEventDispatcher()
            let core = KansoCore.newMock(listener: dispatcher)
            let controller = WorkspaceController(
                core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true), gutter: gutter
            )
            controllers.append(controller)
            let agentSession = try controller.openLocalShell()
            let plainSession = try controller.split(axis: .horizontal)

            func deliver(_ activity: FfiActivity, to session: SessionID, until check: @escaping () -> Bool) async {
                let done = expectation(description: "事件已处理")
                done.assertForOverFulfill = false
                controller.addObserver { if check() { done.fulfill() } }
                dispatcher.onEvent(event: .activityChanged(session: session, activity: activity))
                await fulfillment(of: [done], timeout: 5)
            }

            await deliver(.agentIdle(agent: .claude), to: agentSession) { gutter.override(for: agentSession) == .off }
            XCTAssertNil(gutter.override(for: plainSession))
            XCTAssertEqual(gutter.mode, .both, "不影响全局")
            if let view = controller.host(for: agentSession)?.view, view.renderer != nil {
                XCTAssertEqual(view.gutterMode, .off)
                XCTAssertEqual(controller.host(for: plainSession)?.view.gutterMode, .both)
            }

            // 用户在活动 AI 会话里点状态栏开关：只改这个会话。
            controller.workspace.activate(agentSession)
            controller.toggleTimeline()
            XCTAssertEqual(gutter.override(for: agentSession), .both)
            XCTAssertEqual(gutter.mode, .both)

            // 同一工具再次上报（agentWorking）：保留用户选择。
            await deliver(.agentWorking(agent: .claude), to: agentSession) { true }
            XCTAssertEqual(gutter.override(for: agentSession), .both)

            // 全局模式变化：AI 会话保持自己的设置。
            gutter.mode = .lineNumbers
            XCTAssertEqual(gutter.override(for: agentSession), .both)
            if let view = controller.host(for: agentSession)?.view, view.renderer != nil {
                XCTAssertEqual(view.gutterMode, .both)
                XCTAssertEqual(controller.host(for: plainSession)?.view.gutterMode, .lineNumbers)
            }

            // 工具退出：临时设置作废，回到全局模式。
            await deliver(.idle, to: agentSession) { gutter.override(for: agentSession) == nil }
            if let view = controller.host(for: agentSession)?.view, view.renderer != nil {
                XCTAssertEqual(view.gutterMode, .lineNumbers)
            }
        }
    }

    // MARK: 当前目录条目与时钟

    func testDirectoryTextRules() {
        let home = "/Users/me"
        XCTAssertEqual(TabDescriptions.directoryText(for: TabItem(id: 1, kind: .local, name: "Local", cwd: "/Users/me"), homeDirectory: home), "~")
        XCTAssertEqual(TabDescriptions.directoryText(for: TabItem(id: 1, kind: .local, name: "Local", cwd: "/Users/me/src"), homeDirectory: home), "~/src")
        XCTAssertEqual(TabDescriptions.directoryText(for: TabItem(id: 1, kind: .local, name: "Local", cwd: "/etc"), homeDirectory: home), "/etc")
        XCTAssertNil(TabDescriptions.directoryText(for: TabItem(id: 1, kind: .local, name: "Local", cwd: nil), homeDirectory: home))
        // SSH：无 OSC 7 上报时隐藏，不用会话名充数；有上报时原样显示，不用本机 home 替换。
        XCTAssertNil(TabDescriptions.directoryText(for: TabItem(id: 2, kind: .ssh, name: "prod"), homeDirectory: home))
        XCTAssertEqual(TabDescriptions.directoryText(for: TabItem(id: 2, kind: .ssh, name: "prod", cwd: "/Users/me/x"), homeDirectory: home), "/Users/me/x")
        XCTAssertNil(TabDescriptions.directoryText(for: nil, homeDirectory: home))
    }

    func testStatusBarDirectoryVisibility() {
        let bar = StatusBarView(frame: NSRect(x: 0, y: 0, width: 800, height: 28))
        XCTAssertFalse(bar.isDirectoryVisible)
        bar.setDirectory("~/src")
        XCTAssertTrue(bar.isDirectoryVisible)
        XCTAssertEqual(bar.directoryText, "~/src")
        bar.setDirectory(nil)
        XCTAssertFalse(bar.isDirectoryVisible)
    }

    func testClockFormatIsFixed() {
        let utc = TimeZone(identifier: "UTC")!
        // 2026-11-14 11:52:30 UTC
        let date = Date(timeIntervalSince1970: 1_794_657_150)
        XCTAssertEqual(StatusBarView.clockString(from: date, timeZone: utc), "2026/11/14 11:52")
        let bar = StatusBarView(frame: .zero)
        XCTAssertNotNil(bar.clockText.range(of: #"^\d{4}/\d{2}/\d{2} \d{2}:\d{2}$"#, options: .regularExpression))
    }

    func testLayoutDoesNotOverlapWhenNarrow() {
        let bar = StatusBarView(frame: NSRect(x: 0, y: 0, width: 320, height: 28))
        bar.setStatus(String(repeating: "很长的状态文字", count: 20))
        bar.setDirectory("/very/long/path/that/does/not/fit/in/the/status/bar/at/all")
        bar.setLegacySshToolTip("host")
        bar.layoutSubtreeIfNeeded()
        let frames = bar.subviews.filter { !$0.isHidden }.map(\.frame).sorted { $0.minX < $1.minX }
        for (a, b) in zip(frames, frames.dropFirst()) where a.width > 0 && b.width > 0 {
            XCTAssertLessThanOrEqual(a.maxX, b.minX + 0.5)
        }
    }
}
