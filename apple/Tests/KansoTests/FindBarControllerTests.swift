import AppKit
import KansoBridge
import XCTest
@testable import Kanso

/// 假搜索后端：记录调用，返回预设结果或抛预设错误。
@MainActor
private final class FakeSearchTarget: TerminalSearchTarget {
    var searches: [(text: String, caseSensitive: Bool, regex: Bool)] = []
    var nexts: [Bool] = []
    var closeCount = 0
    var statusCount = 0
    var searchResult: Result<FfiSearchStatus, Error> = .success(FfiSearchStatus(total: 17, current: 3))
    var nextResult: Result<FfiSearchStatus, Error> = .success(FfiSearchStatus(total: 17, current: 4))

    func search(_ text: String, caseSensitive: Bool, regex: Bool) throws -> FfiSearchStatus {
        searches.append((text, caseSensitive, regex))
        return try searchResult.get()
    }

    func next(backwards: Bool) throws -> FfiSearchStatus {
        nexts.append(backwards)
        return try nextResult.get()
    }

    func close() { closeCount += 1 }

    func status() throws -> FfiSearchStatus {
        statusCount += 1
        return try searchResult.get()
    }
}

/// 手动调度器：不 sleep，由测试显式触发到期。
@MainActor
private final class ManualScheduler {
    private(set) var pending: [Int: @MainActor () -> Void] = [:]
    private var nextID = 0
    private(set) var scheduledCount = 0

    var scheduler: FindScheduler {
        { [unowned self] _, work in
            let id = nextID
            nextID += 1
            scheduledCount += 1
            pending[id] = work
            return { [unowned self] in pending[id] = nil }
        }
    }

    /// 触发所有已到期的任务（任务里新排的不在本轮触发）。
    func fire() {
        let tasks = pending
        pending = [:]
        for (_, work) in tasks.sorted(by: { $0.key < $1.key }) { work() }
    }
}

@MainActor
final class FindBarControllerTests: XCTestCase {
    private func make() -> (FindBarController, FakeSearchTarget, ManualScheduler) {
        let target = FakeSearchTarget()
        let manual = ManualScheduler()
        let controller = FindBarController(target: target, scheduler: manual.scheduler)
        return (controller, target, manual)
    }

    private func type(_ text: String, into controller: FindBarController) {
        controller.view.searchField.stringValue = text
        controller.queryChanged()
    }

    func testStartsHidden() {
        let (controller, _, _) = make()
        XCTAssertFalse(controller.isVisible)
    }

    func testInstallPinsToTopRightAndHidden() {
        let (controller, _, _) = make()
        let container = NSView(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        controller.install(in: container)
        container.layoutSubtreeIfNeeded()
        XCTAssertTrue(controller.view.superview === container)
        XCTAssertTrue(controller.view.isHidden)
        XCTAssertEqual(controller.view.frame.width, FindBarView.barWidth)
        XCTAssertEqual(controller.view.frame.maxX, 800 - DesignTokens.Spacing.sm)
        // 非翻转坐标系下，顶部对齐即 maxY = 高度 - 间距。
        XCTAssertEqual(controller.view.frame.maxY, 600 - DesignTokens.Spacing.sm)
    }

    func testTypingSearchesWithOptions() {
        let (controller, target, _) = make()
        controller.view.caseSensitiveButton.state = .on
        controller.view.regexButton.state = .on
        type("foo", into: controller)
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(target.searches[0].text, "foo")
        XCTAssertTrue(target.searches[0].caseSensitive)
        XCTAssertTrue(target.searches[0].regex)
        XCTAssertEqual(controller.statusText, "3/17")
    }

    func testEmptyTextClosesAndClearsStatus() {
        let (controller, target, _) = make()
        type("foo", into: controller)
        type("", into: controller)
        XCTAssertEqual(target.closeCount, 1)
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(controller.statusText, "")
        XCTAssertFalse(controller.statusIsError)
    }

    func testEnterAndShiftEnterDirection() {
        let (controller, target, _) = make()
        controller.show()
        type("foo", into: controller)
        XCTAssertTrue(controller.handleCommand(#selector(NSResponder.insertNewline(_:)), shift: false))
        XCTAssertTrue(controller.handleCommand(#selector(NSResponder.insertNewline(_:)), shift: true))
        XCTAssertTrue(controller.handleCommand(#selector(NSResponder.insertNewlineIgnoringFieldEditor(_:)), shift: true))
        XCTAssertEqual(target.nexts, [false, true, true])
        XCTAssertFalse(controller.handleCommand(#selector(NSResponder.moveLeft(_:)), shift: false))
    }

    func testEscapeClosesHidesAndNotifies() {
        let (controller, target, _) = make()
        controller.show()
        var dismissed = 0
        controller.onDismiss = { dismissed += 1 }
        XCTAssertTrue(controller.handleCommand(#selector(NSResponder.cancelOperation(_:)), shift: false))
        XCTAssertEqual(target.closeCount, 1)
        XCTAssertFalse(controller.isVisible)
        XCTAssertEqual(dismissed, 1)
    }

    func testCloseButtonDismisses() {
        let (controller, target, _) = make()
        controller.show()
        var dismissed = 0
        controller.onDismiss = { dismissed += 1 }
        controller.view.closeButton.performClick(nil)
        XCTAssertEqual(target.closeCount, 1)
        XCTAssertFalse(controller.isVisible)
        XCTAssertEqual(dismissed, 1)
    }

    func testStatusTexts() {
        let (controller, target, _) = make()
        type("foo", into: controller)
        XCTAssertEqual(controller.statusText, "3/17")

        target.searchResult = .success(FfiSearchStatus(total: 17, current: nil))
        type("foo", into: controller)
        XCTAssertEqual(controller.statusText, "–/17")

        target.searchResult = .success(FfiSearchStatus(total: 0, current: nil))
        type("foo", into: controller)
        XCTAssertEqual(controller.statusText, String(localized: "No Results"))
        XCTAssertFalse(controller.statusIsError)
    }

    func testInvalidRegexShowsError() {
        let (controller, target, _) = make()
        target.searchResult = .failure(FfiCoreError.User(message: FfiMessage(key: "search-invalid-regex", args: [])))
        type("(", into: controller)
        XCTAssertEqual(controller.statusText, String(localized: "Invalid Regular Expression"))
        XCTAssertTrue(controller.statusIsError)
    }

    func testOtherErrorClearsStatusSilently() {
        let (controller, target, _) = make()
        type("foo", into: controller)
        target.searchResult = .failure(FfiCoreError.NoSuchSession)
        type("foo", into: controller)
        XCTAssertEqual(controller.statusText, "")
        XCTAssertFalse(controller.statusIsError)
    }

    func testFindNextWhenHiddenShowsFirst() {
        let (controller, target, _) = make()
        controller.view.searchField.stringValue = "foo"
        controller.findNext()
        XCTAssertTrue(controller.isVisible)
        // 显示时先用现有文字恢复高亮，再前进。
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(target.nexts, [false])
    }

    func testFindNextWithEmptyTextDoesNothing() {
        let (controller, target, _) = make()
        controller.findPrevious()
        XCTAssertTrue(target.nexts.isEmpty)
        XCTAssertTrue(target.searches.isEmpty)
    }

    func testShowWithExistingTextResearches() {
        let (controller, target, _) = make()
        controller.view.searchField.stringValue = "foo"
        controller.show()
        XCTAssertEqual(target.searches.count, 1)
    }

    func testRefreshStatusThrottles() {
        let (controller, target, manual) = make()
        controller.show()
        type("foo", into: controller)
        XCTAssertEqual(target.searches.count, 1)

        for _ in 0..<10 { controller.refreshStatus() }
        // 第一次立即执行，其余只记尾随标记。
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(target.statusCount, 1)

        target.searchResult = .success(FfiSearchStatus(total: 20, current: 3))
        manual.fire()
        // 窗口结束：尾随执行 1 次。
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(target.statusCount, 2)
        XCTAssertEqual(controller.statusText, "3/20")

        // 窗口里没有新调用，再到期不再执行。
        manual.fire()
        XCTAssertEqual(target.searches.count, 1)
        XCTAssertEqual(target.statusCount, 2)
        XCTAssertTrue(manual.pending.isEmpty)
    }

    func testRefreshStatusWhenHiddenOrEmptyDoesNothing() {
        let (controller, target, manual) = make()
        controller.view.searchField.stringValue = "foo"
        controller.refreshStatus()
        XCTAssertTrue(target.searches.isEmpty)

        controller.show()
        target.searches.removeAll()
        controller.view.searchField.stringValue = ""
        controller.refreshStatus()
        XCTAssertTrue(target.searches.isEmpty)
        XCTAssertEqual(manual.scheduledCount, 0)
        XCTAssertEqual(target.statusCount, 0)
    }
}
