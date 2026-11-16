import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

final class FakeGitEngine: KansoGitPanelProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var revision: UInt64 = 0
    private var reads: [String] = []
    private var diffReads: [String] = []
    private var commits: [String] = []
    private var discards: [String?] = []
    var readBody: @Sendable (UInt64, String) throws -> FfiGitSummary? = { _, _ in GitFixtures.summary }
    var diffBody: @Sendable (String) throws -> FfiGitDiff? = { _ in GitFixtures.diff }
    var commitBody: @Sendable () throws -> Void = {}
    var discardBody: @Sendable () throws -> Void = {}

    var readPaths: [String] { lock.withLock { reads } }
    var diffPaths: [String] { lock.withLock { diffReads } }
    var commitMessages: [String] { lock.withLock { commits } }
    var discardedPaths: [String?] { lock.withLock { discards } }
    func begin() -> UInt64 { lock.withLock { revision += 1; return revision } }
    func isCurrent(request: UInt64) -> Bool { lock.withLock { request == revision } }
    func read(request: UInt64, cwd: String) throws -> FfiGitSummary? {
        lock.withLock { reads.append(cwd) }
        return try readBody(request, cwd)
    }
    func diff(request: UInt64, path: String) throws -> FfiGitDiff? {
        lock.withLock { diffReads.append(path) }
        return try diffBody(path)
    }
    func commit(request: UInt64, message: String) throws {
        lock.withLock { commits.append(message) }
        try commitBody()
    }
    func discard(request: UInt64, path: String?) throws {
        lock.withLock { discards.append(path) }
        try discardBody()
    }
}

enum GitFixtures {
    static let files = [
        FfiGitFile(path: "apple/App/MainWindow.swift", original: nil, status: "M", added: 18, deleted: 5),
        FfiGitFile(path: "crates/core/git.rs", original: "crates/core/changes.rs", status: "R", added: 9, deleted: 3),
        FfiGitFile(path: "docs/改动 [示例].md", original: nil, status: "U", added: 12, deleted: 0),
        FfiGitFile(path: "resources/banner.png", original: nil, status: "M", added: nil, deleted: nil),
    ]
    static let summary = FfiGitSummary(root: "/fixture/project", branch: "feature/git-panel", files: files, added: 39, deleted: 8)
    static let diff = FfiGitDiff(lines: [
        FfiGitLine(kind: " ", number: 38, text: "    let workspace: Workspace"),
        FfiGitLine(kind: "-", number: 39, text: "    let title = \"Kanso\""),
        FfiGitLine(kind: "+", number: 39, text: "    let title = workspace.name"),
        FfiGitLine(kind: "+", number: 40, text: "    showChanges(for: workspace)"),
        FfiGitLine(kind: " ", number: 41, text: "}"),
        FfiGitLine(kind: " ", number: 58, text: "func refresh() {"),
        FfiGitLine(kind: "-", number: 58, text: "    reload(force: false)"),
        FfiGitLine(kind: "+", number: 59, text: "    reload(force: true)"),
        FfiGitLine(kind: " ", number: 60, text: "}"),
    ], notice: nil, truncated: false)

    static func tab(_ id: UInt64 = 1, cwd: String = "/fixture/project", kind: TabKind = .local, connection: TabConnection = .connected) -> TabItem {
        TabItem(id: id, kind: kind, name: "fixture", cwd: cwd, connection: connection)
    }
}

@MainActor
final class GitPanelModelTests: XCTestCase {
    func testGitMessagesRenderInEnglishAndChinese() {
        let messages = [
            ("git-summary-expired", "The repository summary is out of date. Refresh to continue.", "仓库摘要已过期，请刷新"),
            ("git-commit-message-invalid", "The commit message must not be empty or exceed 16 KiB.", "提交说明不能为空且不能超过 16 KiB"),
            ("git-entry-unavailable", "This entry is no longer available.", "此条目已不可用"),
            ("git-diff-unavailable", "No text diff is available for binary files, non-regular files, or content exceeding 512 KiB.", "二进制文件、非普通文件或内容超过 512 KiB，无法提供文本差异"),
            ("git-diff-empty-file", "New empty file. No text diff is available.", "新增空文件，没有文本差异"),
            ("git-diff-binary", "Binary file. No text diff is available.", "二进制文件，无法提供文本差异"),
            ("git-diff-no-content-change", "No net content changes relative to the current commit.", "相对当前提交没有净内容变化"),
            ("git-diff-metadata-only", "Only metadata changed, or no text diff is available.", "仅元数据变化或无法提供文本差异")
        ]
        for (key, english, chinese) in messages {
            let message = FfiMessage(key: key, args: [])
            XCTAssertEqual(CoreMessageText.text(message, languages: ["en"]), english)
            XCTAssertEqual(CoreMessageText.text(message, languages: ["zh-Hans"]), chinese)
        }
    }

    func testOnlyConnectedLocalShellAndClearPreviousRepository() async {
        let engine = FakeGitEngine()
        let model = GitPanelModel(engine: engine)
        for tab in [nil, GitFixtures.tab(kind: .ssh), GitFixtures.tab(kind: .sftp), GitFixtures.tab(connection: .closed(reason: nil)), GitFixtures.tab(cwd: "")] {
            model.follow(tab)
        }
        XCTAssertTrue(engine.readPaths.isEmpty)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        XCTAssertEqual(model.summary, GitFixtures.summary)
        model.follow(GitFixtures.tab(kind: .ssh))
        XCTAssertNil(model.summary)
        XCTAssertTrue(model.expanded.isEmpty)
    }

    func testLazyMultipleDiffsAndSameCountsRefreshesBody() async {
        let engine = FakeGitEngine()
        let model = GitPanelModel(engine: engine)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        XCTAssertTrue(engine.diffPaths.isEmpty)
        model.toggle(GitFixtures.files[0].path)
        model.toggle(GitFixtures.files[1].path)
        await model.waitUntilIdle()
        XCTAssertEqual(model.expanded.count, 2)
        XCTAssertEqual(engine.diffPaths.count, 2)
        engine.diffBody = { _ in FfiGitDiff(lines: [.init(kind: "+", number: 1, text: "same length new text")], notice: nil, truncated: false) }
        model.refresh()
        await model.waitUntilIdle()
        XCTAssertEqual(engine.diffPaths.count, 4)
        XCTAssertEqual(model.diffs[GitFixtures.files[0].path]?.lines.first?.text, "same length new text")
        model.toggle(GitFixtures.files[0].path)
        XCTAssertEqual(model.expanded.count, 1)
        XCTAssertNil(model.diffs[GitFixtures.files[0].path])
    }

    func testOldSummaryCannotRestorePreviousSession() async {
        let started = expectation(description: "旧请求已开始")
        let release = DispatchSemaphore(value: 0)
        let engine = FakeGitEngine()
        engine.readBody = { _, cwd in
            if cwd == "/fixture/old" { started.fulfill(); release.wait() }
            return cwd == "/fixture/old" ? GitFixtures.summary : nil
        }
        let model = GitPanelModel(engine: engine)
        model.follow(GitFixtures.tab(cwd: "/fixture/old"))
        await fulfillment(of: [started], timeout: 5)
        model.follow(GitFixtures.tab(2, cwd: "/fixture/new"))
        await model.waitUntilIdle()
        release.signal()
        await Task.yield()
        XCTAssertNil(model.summary)
        XCTAssertFalse(model.isLoading)
    }

    func testOldDiffCannotRestorePreviousSession() async {
        let started = expectation(description: "旧差异请求已开始")
        let release = DispatchSemaphore(value: 0)
        let engine = FakeGitEngine()
        engine.diffBody = { _ in
            started.fulfill()
            release.wait()
            return GitFixtures.diff
        }
        let model = GitPanelModel(engine: engine)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        model.toggle(GitFixtures.files[0].path)
        await fulfillment(of: [started], timeout: 5)
        let oldDiff = Task { await model.waitUntilIdle() }
        await Task.yield()
        model.follow(GitFixtures.tab(kind: .ssh))
        release.signal()
        await oldDiff.value
        XCTAssertNil(model.summary)
        XCTAssertTrue(model.diffs.isEmpty)
        XCTAssertTrue(model.expanded.isEmpty)
    }

    func testCleanNotRepositoryAndFailureRemainDistinct() async {
        let engine = FakeGitEngine()
        let model = GitPanelModel(engine: engine)
        engine.readBody = { _, _ in FfiGitSummary(root: "/fixture/clean", branch: "main", files: [], added: 0, deleted: 0) }
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        XCTAssertEqual(model.summary?.files, [])
        XCTAssertNil(model.error)
        engine.readBody = { _, _ in nil }
        model.refresh()
        await model.waitUntilIdle()
        XCTAssertNil(model.summary)
        XCTAssertNil(model.error)
        engine.readBody = { _, _ in throw FfiCoreError.Internal(detail: "git unavailable") }
        model.refresh()
        await model.waitUntilIdle()
        XCTAssertNil(model.summary)
        XCTAssertTrue(model.error?.contains("git unavailable") == true)
    }

    func testCommitValidationFailureRetainsMessageAndSuccessClearsIt() async {
        let engine = FakeGitEngine()
        let model = GitPanelModel(engine: engine)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        model.message = " \n "
        XCTAssertFalse(model.canCommit)
        await model.commit()
        model.message = String(repeating: "中", count: 6000)
        XCTAssertFalse(model.canCommit)
        XCTAssertTrue(engine.commitMessages.isEmpty)
        model.message = "保留说明"
        engine.commitBody = { throw FfiCoreError.Internal(detail: "hook failed") }
        await model.commit()
        await model.waitUntilIdle()
        XCTAssertEqual(model.message, "保留说明")
        XCTAssertTrue(model.error?.contains("暂存内容") == true)
        XCTAssertTrue(model.error?.contains("hook failed") == true)
        model.dismissError()
        XCTAssertNil(model.error)
        engine.commitBody = {}
        await model.commit()
        await model.waitUntilIdle()
        XCTAssertEqual(model.message, "")
        XCTAssertEqual(engine.commitMessages.count, 2)
    }

    func testWriteLocksRefreshListAndDefersTargetChange() async {
        let engine = FakeGitEngine()
        let started = expectation(description: "丢弃已开始")
        let release = DispatchSemaphore(value: 0)
        engine.discardBody = { started.fulfill(); release.wait() }
        let model = GitPanelModel(engine: engine)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        let write = Task { await model.discard(GitFixtures.files[0].path) }
        await fulfillment(of: [started], timeout: 5)
        XCTAssertTrue(model.isWriting)
        XCTAssertFalse(model.canWrite)
        model.refresh()
        model.toggle(GitFixtures.files[0].path)
        model.follow(GitFixtures.tab(2, cwd: "/fixture/next"))
        XCTAssertTrue(model.expanded.isEmpty)
        XCTAssertEqual(engine.readPaths.count, 1)
        release.signal()
        await write.value
        await model.waitUntilIdle()
        XCTAssertFalse(model.isWriting)
        XCTAssertEqual(engine.readPaths.last, "/fixture/next")
        XCTAssertEqual(engine.discardedPaths, [GitFixtures.files[0].path])
        engine.discardBody = {}
        await model.discard(nil)
        await model.waitUntilIdle()
        XCTAssertNil(engine.discardedPaths.last!)
    }
}
