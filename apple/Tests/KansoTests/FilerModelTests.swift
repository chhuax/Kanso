import KansoUI
@testable import Kanso
import XCTest

/// view model 层的验收用例（filer.md §13.1–§13.3）：全部用替身，不碰真实文件系统与 core。
@MainActor
final class FilerModelTests: XCTestCase {
    private func settle() async { try? await Task.sleep(nanoseconds: 60_000_000) }

    // MARK: 排序、统计、路径

    func testSortDirectoriesFirstCaseInsensitiveNotNatural() {
        let entries = [FilerEntry.file("b"), .file("A"), .dir("zeta"), .file("a10"), .file("a2"), .dir("Alpha"), .file("a")]
        XCTAssertEqual(FilerSort.sorted(entries).map(\.name), ["Alpha", "zeta", "A", "a", "a10", "a2", "b"])
    }

    func testStatsCountLinksSeparately() {
        let stats = FilerStats([.dir("d"), .file("f"), .file("g"), .link("l")])
        XCTAssertEqual(stats, FilerStats(directories: 1, files: 2, links: 1))
        XCTAssertEqual(FilerStats().total, 0)
    }

    func testPathMath() {
        XCTAssertEqual(FilerPath.normalize("/a//b/./c/.."), "/a/b")
        XCTAssertEqual(FilerPath.parent(of: "/"), "/")
        XCTAssertEqual(FilerPath.parent(of: "/a"), "/")
        XCTAssertEqual(FilerPath.resolve("sub", base: "/a/b", home: nil), "/a/b/sub")
        XCTAssertEqual(FilerPath.resolve("../x", base: "/a/b", home: nil), "/a/x")
        XCTAssertEqual(FilerPath.resolve("~/p", base: "/a", home: "/Users/me"), "/Users/me/p")
        XCTAssertEqual(FilerPath.resolve("~/p", base: "/a", home: nil), "/a/~/p")
        XCTAssertNil(FilerPath.resolve("  ", base: "/a", home: nil))
        XCTAssertTrue(FilerPath.isSameOrDescendant("/a/b", of: "/a"))
        XCTAssertFalse(FilerPath.isSameOrDescendant("/a/bc", of: "/a/b"))
        XCTAssertEqual(FilerPath.breadcrumbs("/a/b").map(\.path), ["/", "/a", "/a/b"])
    }

    func testRateMeterUsesRecentWindowNotLifetimeAverage() {
        var m = RateMeter()
        m.record(bytes: 0, at: 0)
        m.record(bytes: 1000, at: 1)
        m.record(bytes: 1100, at: 3)   // 最近 1 秒只前进 100 字节/2s 窗口内
        m.record(bytes: 1200, at: 4)
        XCTAssertEqual(try XCTUnwrap(m.bytesPerSecond), 100, accuracy: 1)
        XCTAssertNil(RateMeter().bytesPerSecond)
    }

    func testStatusProgressTexts() {
        var s = FilerTransferStatus(kind: .upload, name: "a", index: 1, count: 1)
        XCTAssertNil(s.fraction, "总量未知是不确定进度")
        s.totalBytes = 0
        XCTAssertNil(s.fraction, "零总量不能显示成传完了")
        s.totalBytes = 200; s.doneBytes = 50
        XCTAssertEqual(s.fraction, 0.25)
        s.phase = .completed(skipped: 0)
        XCTAssertEqual(s.fraction, 1)
        XCTAssertNotEqual(FilerTransferStatus(kind: .copy, name: "a", index: 1, count: 1).actionText,
                          FilerTransferStatus(kind: .upload, name: "a", index: 1, count: 1).actionText)
    }

    // MARK: 跟随、记忆、断线

    func testFollowLoadsHomeAndStreamsBatches() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.file("b", in: "/home"), .dir("a", in: "/home"), .file("c", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1))
        await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/home")
        XCTAssertEqual(model.entries.map(\.name), ["a", "b", "c"])
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.source, .local)
        XCTAssertEqual(model.stats, FilerStats(directories: 1, files: 2, links: 0))
    }

    func testEachTabRemembersItsLastDirectoryAndShowsItsSource() async {
        let (model, env, _) = FilerFixtures.make()
        let a = FakeDataSource(remote: false), b = FakeDataSource(remote: true), c = FakeDataSource(remote: true)
        a.home = "/Users/me"; b.home = "/root"; c.home = "/srv"
        a.dirs["/Users/me/work"] = [.file("x", in: "/Users/me/work")]
        b.dirs["/var/log"] = []
        env.sources = [1: a, 2: b, 3: c]
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/Users/me/work"); await model.waitUntilIdle()
        model.follow(FilerFixtures.tab(2, kind: .ssh, name: "web")); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/root", "新标签从自己的家目录开始")
        XCTAssertEqual(model.source?.title, "web · root@web.example.com")
        model.open(directory: "/var/log"); await model.waitUntilIdle()
        model.follow(FilerFixtures.tab(3, kind: .ssh, name: "db")); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/srv")
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/Users/me/work")
        XCTAssertEqual(model.source, .local)
        model.follow(FilerFixtures.tab(2, kind: .ssh, name: "web")); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/var/log")
    }

    func testSftpTabMakesPanelUnavailableAndRestoresOnReturn() async {
        let (model, env, _) = FilerFixtures.make()
        env.sources[1] = FakeDataSource(remote: true)
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "s")); await model.waitUntilIdle()
        model.follow(FilerFixtures.tab(9, kind: .sftp, name: "files"))
        XCTAssertEqual(model.availability, .unavailableSftp)
        XCTAssertNotNil(model.unavailableReason)
        XCTAssertFalse(model.canWrite)
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "s")); await model.waitUntilIdle()
        XCTAssertEqual(model.availability, .available)
        XCTAssertEqual(model.directory, "/home")
    }

    func testDisconnectKeepsIdentityListAndDisablesWrites_ReconnectRefreshes() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: true)
        ds.dirs["/home"] = [.file("f", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web")); await model.waitUntilIdle()
        XCTAssertTrue(model.canWrite)
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web", connection: .closed(reason: "lost")))
        XCTAssertTrue(model.isStale)
        XCTAssertEqual(model.source?.isRemote, true, "断线后仍是远端身份，不退回本地")
        XCTAssertEqual(model.entries.map(\.name), ["f"])
        XCTAssertEqual(model.directory, "/home")
        XCTAssertFalse(model.canWrite)
        XCTAssertFalse(model.canUploadOrDownload)
        model.beginCreate(directory: false)
        XCTAssertEqual(model.editing, .none)
        XCTAssertTrue(model.notice?.isError == true)
        let before = ds.lists.count
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web")); await model.waitUntilIdle()
        XCTAssertEqual(ds.lists.count, before + 1, "重连后重新读取当前目录")
        XCTAssertFalse(model.isStale)
    }

    func testHiddenFilesToggleAffectsVisibleEntriesAndStats() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.file(".hidden", in: "/home"), .file("shown", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        XCTAssertEqual(model.visibleEntries.count, 2, "默认显示隐藏文件")
        model.showsHiddenFiles = false
        XCTAssertEqual(model.visibleEntries.map(\.name), ["shown"])
        XCTAssertEqual(model.stats.files, 1)
    }

    // MARK: 导航

    func testNavigateInputRelativeTildeAndRootParent() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/a/b"); await model.waitUntilIdle()
        model.navigate(toInput: "c"); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/a/b/c")
        model.navigate(toInput: "~"); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, NSHomeDirectory())
        model.open(directory: "/x"); await model.waitUntilIdle()
        model.goUp(); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/")
        let count = ds.lists.count
        model.goUp()
        XCTAssertEqual(model.directory, "/")
        XCTAssertEqual(ds.lists.count, count, "根目录的上一级停在根目录")
        model.goHome(); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/home")
    }

    func testEscRestoresLastSuccessfulDirectory() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.errors["/bad"] = FilerError(.notFound, path: "/bad")
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/bad"); await model.waitUntilIdle()
        XCTAssertEqual(model.restoredPathText, "/home")
    }

    func testRapidNavigationOnlyLastResultIsAdopted() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/x"] = [.file("x1", in: "/x")]
        ds.dirs["/y"] = [.file("y1", in: "/y")]
        ds.gates["/x"] = Gate()
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/x")
        model.open(directory: "/y"); await model.waitUntilIdle()
        ds.gates["/x"]?.open()
        await settle()
        XCTAssertEqual(model.directory, "/y")
        XCTAssertEqual(model.entries.map(\.name), ["y1"])
        XCTAssertEqual(model.phase, .loaded)
    }

    func testStaleFailureDoesNotClearNewDirectory() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.errors["/x"] = FilerError(.permissionDenied, path: "/x")
        ds.gates["/x"] = Gate()
        ds.dirs["/y"] = [.file("y1", in: "/y")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/x")
        model.open(directory: "/y"); await model.waitUntilIdle()
        ds.gates["/x"]?.open(); await settle()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.entries.map(\.name), ["y1"])
    }

    func testReadFailureShowsErrorNotEmptyListing() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.file("old", in: "/home")]
        ds.errors["/secret"] = FilerError(.permissionDenied, path: "/secret")
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/secret"); await model.waitUntilIdle()
        XCTAssertEqual(model.phase, .failed(FilerError(.permissionDenied, path: "/secret")))
        XCTAssertTrue(model.entries.isEmpty, "旧条目不能挂在新路径下")
        XCTAssertFalse(model.canWrite)
    }

    func testErrorKindsAreDistinct() {
        let kinds: [FilerFailureKind] = [.permissionDenied, .notFound, .notADirectory, .diskUnavailable, .notEmpty]
        XCTAssertEqual(Set(kinds.map { FilerError($0).reason }).count, kinds.count)
        XCTAssertEqual(FilerError.fromErrno(EACCES, path: nil).kind, .permissionDenied)
        XCTAssertEqual(FilerError.fromErrno(ENOENT, path: nil).kind, .notFound)
        XCTAssertEqual(FilerError.fromErrno(ENOTDIR, path: nil).kind, .notADirectory)
        XCTAssertEqual(FilerError.fromErrno(EIO, path: nil).kind, .diskUnavailable)
    }

    func testLinkActivationConfirmsTargetType() async {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        let toDir = FilerEntry.link("ld", in: "/home"), toFile = FilerEntry.link("lf", in: "/home"), dangling = FilerEntry.link("lx", in: "/home")
        ds.dirs["/home"] = [toDir, toFile, dangling]
        ds.linkTargets[toDir.path] = .success(.directory(path: "/target"))
        ds.linkTargets[toFile.path] = .success(.file)
        ds.linkTargets[dangling.path] = .failure(FilerError(.notFound, path: dangling.path))
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.activate(toDir); await settle(); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/target")
        model.open(directory: "/home"); await model.waitUntilIdle()
        model.activate(toFile); await settle()
        XCTAssertEqual(prompter.opened, [toFile.path])
        model.activate(dangling); await settle()
        XCTAssertTrue(model.notice?.isError == true)
    }

    func testRemoteFileOpenDelegatesToOriginalSession() async {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: true)
        ds.dirs["/home"] = [.file("f", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "w")); await model.waitUntilIdle()
        var requested: (FilerEntry, FilerTabSnapshot)?
        model.onOpenRemoteFile = { entry, tab, _, _ in requested = (entry, tab) }
        model.activate(.file("f", in: "/home"))
        XCTAssertTrue(prompter.opened.isEmpty)
        XCTAssertTrue(env.engine.starts.isEmpty)
        XCTAssertEqual(requested?.0.path, "/home/f")
        XCTAssertEqual(requested?.1.session, 1)
    }

    // MARK: 选择

    func testSelectionSurvivesBatchInsertsAndIsResetOnRefresh() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.file("m", in: "/home"), .file("n", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.setSelection(paths: ["/home/n"])
        // 后续批次插入到前面：选中的仍是同一个对象（按路径记录）。
        ds.dirs["/home"] = [.file("a", in: "/home"), .file("n", in: "/home")]
        XCTAssertEqual(model.selectedEntries.map(\.name), ["n"])
        model.refresh()
        XCTAssertTrue(model.selectedPaths.isEmpty, "刷新重置旧选项")
        await model.waitUntilIdle()
        model.setSelection(paths: ["/home/missing"])
        XCTAssertTrue(model.selectedPaths.isEmpty, "不存在的条目不能被选中")
    }

    // MARK: 新建 / 重命名

    private func writableModel() async -> (FilerModel, FakeDataSource, FakePrompter) {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.file("exists", in: "/home"), .dir("sub", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        return (model, ds, prompter)
    }

    func testCreateNameValidation() async {
        let (model, ds, _) = await writableModel()
        model.beginCreate(directory: false)
        var closed = await model.commitEditing(name: "   ", viaBlur: false)
        XCTAssertTrue(closed); XCTAssertEqual(model.editing, .none)
        XCTAssertTrue(ds.ops.isEmpty, "空名称不创建")

        model.beginCreate(directory: false)
        closed = await model.commitEditing(name: "a/b", viaBlur: false)
        XCTAssertFalse(closed, "Enter 时保留输入行")
        XCTAssertTrue(ds.ops.isEmpty)

        closed = await model.commitEditing(name: "..", viaBlur: false)
        XCTAssertFalse(closed)
        closed = await model.commitEditing(name: " exists ", viaBlur: true)
        XCTAssertTrue(closed, "失焦时放弃")
        XCTAssertEqual(model.editing, .none)
        XCTAssertTrue(ds.ops.isEmpty)
        XCTAssertTrue(model.notice?.isError == true)
    }

    func testCreateSuccessTrimsNameAndRefreshes() async {
        let (model, ds, _) = await writableModel()
        model.beginCreate(directory: true)
        _ = await model.commitEditing(name: "  newdir ", viaBlur: false)
        XCTAssertEqual(ds.ops, ["mkdir /home/newdir"])
        await model.waitUntilIdle()
        XCTAssertEqual(ds.lists.last, "/home")
        model.beginCreate(directory: false)
        model.cancelEditing()
        XCTAssertEqual(model.editing, .none)
    }

    func testCreateFailureShowsReasonAndDropsRow() async {
        let (model, ds, _) = await writableModel()
        ds.failCreate = FilerError(.alreadyExists, path: "/home/race")
        model.beginCreate(directory: false)
        let closed = await model.commitEditing(name: "race", viaBlur: false)
        XCTAssertTrue(closed)
        XCTAssertEqual(model.editing, .none)
        XCTAssertTrue(model.notice?.text.contains("race") == true)
    }

    func testRenameValidatesAndSkipsUnchangedName() async {
        let (model, ds, _) = await writableModel()
        model.setSelection(paths: ["/home/exists"])
        model.beginRename()
        XCTAssertEqual(model.editing, .rename(.file("exists", in: "/home")))
        _ = await model.commitEditing(name: "exists", viaBlur: false)
        XCTAssertTrue(ds.ops.isEmpty, "名称不变不执行")
        model.setSelection(paths: ["/home/exists"]); model.beginRename()
        _ = await model.commitEditing(name: "sub", viaBlur: false)
        XCTAssertTrue(ds.ops.isEmpty, "与已有项同名被拒绝")
        _ = await model.commitEditing(name: "renamed", viaBlur: false)
        XCTAssertEqual(ds.ops, ["rename /home/exists -> /home/renamed"])
    }

    // MARK: 删除

    func testDeleteConfirmationTextsAndBatchConfirmsOnce() async {
        let (model, ds, prompter) = await writableModel()
        model.setSelection(paths: ["/home/exists"])
        await model.deleteSelected()
        XCTAssertEqual(prompter.deleteRequests.count, 1)
        XCTAssertEqual(prompter.deleteRequests[0].title, FilerDeleteRequest(items: [.init(name: "x", path: "/x", kind: .file)], source: .local).title)
        XCTAssertEqual(prompter.deleteRequests[0].source, .local)
        XCTAssertEqual(ds.ops, ["remove /home/exists"])

        // 三种类型的说明各不相同。
        let texts = [FilerEntry.Kind.file, .directory, .link].map {
            FilerDeleteRequest(items: [.init(name: "n", path: "/n", kind: $0)], source: .local)
        }
        XCTAssertEqual(Set(texts.map(\.title)).count, 3)
        XCTAssertEqual(Set(texts.map(\.warning)).count, 3)
        XCTAssertTrue(FilerDeleteRequest(items: [.init(name: "a", path: "/a", kind: .file), .init(name: "b", path: "/b", kind: .file)], source: .local).title.contains("2"))

        prompter.deleteAnswer = false
        model.setSelection(paths: ["/home/sub"])
        await model.deleteSelected()
        XCTAssertEqual(ds.ops.count, 1, "取消不删除")
    }

    func testDeleteNonEmptyDirectoryRefusedAndBatchAsksOnce() async {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/home"] = [.dir("full", in: "/home"), .file("f", in: "/home")]
        ds.dirs["/home/full"] = [.file("x", in: "/home/full")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.setSelection(paths: ["/home/full", "/home/f"])
        await model.deleteSelected()
        XCTAssertEqual(prompter.deleteRequests.count, 1)
        XCTAssertEqual(prompter.deleteRequests[0].items.count, 2)
        XCTAssertTrue(model.notice?.isError == true)
        XCTAssertEqual(model.notice?.text.contains(FilerError(.notEmpty).reason), true)
        XCTAssertTrue(ds.ops.contains("remove /home/f"), "其余项仍执行")
    }

    func testCopyTextFailureIsVisible() async {
        let (model, _, prompter) = await writableModel()
        model.copyPath(of: [.file("a", in: "/home"), .file("b", in: "/home")])
        XCTAssertEqual(prompter.pasted, ["/home/a\n/home/b"])
        prompter.pasteboardOK = false
        model.copyName(of: [.file("a", in: "/home")])
        XCTAssertTrue(model.notice?.isError == true)
    }

    // MARK: ⌘J

    private func remoteReady() async -> (FilerModel, FakeEnv, FakeDataSource) {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: true)
        ds.dirs["/home"] = [.file("home-file", in: "/home")]
        ds.dirs["/srv/app"] = [.file("app.log", in: "/srv/app")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web")); await model.waitUntilIdle()
        return (model, env, ds)
    }

    func testLocateReadsTargetDirectlyWithoutHomeDetour() async {
        let (model, env, ds) = await remoteReady()
        env.cwd[1] = .success("/srv/app")
        var shown = 0
        var messages: [FilerNotice] = []
        model.onRequestShow = { shown += 1 }
        model.onStatusMessage = { messages.append($0) }
        let homeCallsBefore = ds.homeCalls
        await model.locateWorkingDirectory()
        await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/srv/app")
        XCTAssertEqual(model.entries.map(\.name), ["app.log"])
        XCTAssertEqual(ds.homeCalls, homeCallsBefore, "不先读家目录")
        XCTAssertEqual(shown, 1)
        XCTAssertEqual(messages.last?.isError, false)
        // 同一路径再定位一次也重新读取。
        let reads = ds.lists.count
        await model.locateWorkingDirectory(); await model.waitUntilIdle()
        XCTAssertEqual(ds.lists.count, reads + 1)
    }

    func testLocateFailureDoesNotTouchBrowsingDirectory() async {
        let (model, env, _) = await remoteReady()
        env.cwd[1] = .failure(FilerError(.other("no shell found")))
        var messages: [FilerNotice] = []
        model.onStatusMessage = { messages.append($0) }
        await model.locateWorkingDirectory()
        XCTAssertEqual(model.directory, "/home", "失败不拿家目录冒充")
        XCTAssertEqual(messages.last?.isError, true)
        XCTAssertTrue(messages.last?.text.contains("no shell found") == true)
    }

    func testLocateSftpAndDisconnectedExplainReason() async {
        let (model, _, _) = FilerFixtures.make()
        var messages: [FilerNotice] = []
        model.onStatusMessage = { messages.append($0) }
        model.follow(FilerFixtures.tab(5, kind: .sftp, name: "files"))
        await model.locateWorkingDirectory()
        XCTAssertEqual(messages.count, 1)
        model.follow(FilerFixtures.tab(6, kind: .ssh, name: "x", connection: .closed(reason: nil)))
        await model.locateWorkingDirectory()
        XCTAssertEqual(messages.count, 2)
        XCTAssertTrue(messages.allSatisfy(\.isError))
    }

    func testLateLocateResultDoesNotHijackNewTab() async {
        let (model, env, _) = FilerFixtures.make()
        let a = FakeDataSource(remote: true), b = FakeDataSource(remote: true)
        a.home = "/ha"; b.home = "/hb"
        a.dirs["/ha"] = []; b.dirs["/hb"] = [.file("b", in: "/hb")]
        env.sources = [1: a, 2: b]
        env.cwd[1] = .success("/late")
        env.cwdGates[1] = Gate()
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "a")); await model.waitUntilIdle()
        let locate = Task { await model.locateWorkingDirectory(session: 1) }
        await settle()
        model.follow(FilerFixtures.tab(2, kind: .ssh, name: "b")); await model.waitUntilIdle()
        env.cwdGates[1]?.open()
        await locate.value
        XCTAssertEqual(model.tab?.session, 2)
        XCTAssertEqual(model.directory, "/hb", "迟到的结果不作用于后来切换到的标签")
        XCTAssertFalse(b.lists.contains("/late"))
        // 原标签之后被跟随时使用定位得到的目录。
        a.dirs["/late"] = []
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "a")); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/late")
    }

    func testLocalTabWithForeignHostReportDoesNotUseRemotePathAsLocal() async {
        let (model, env, _) = FilerFixtures.make()
        env.sources[1] = FakeDataSource(remote: false)
        env.cwd[1] = .success("/srv/app")
        var messages: [FilerNotice] = []
        model.onStatusMessage = { messages.append($0) }
        model.follow(FilerFixtures.tab(1, cwdHost: "other-server")); await model.waitUntilIdle()
        await model.locateWorkingDirectory()
        XCTAssertEqual(model.directory, "/home")
        XCTAssertEqual(messages.last?.isError, true)
        model.follow(FilerFixtures.tab(1, cwdHost: "MY-MAC.local")); await model.waitUntilIdle()
        await model.locateWorkingDirectory(); await model.waitUntilIdle()
        XCTAssertEqual(model.directory, "/srv/app")
    }

    func testLocateWithHiddenPanelLoadsOnlyAfterRequestingShow() async {
        let (model, env, ds) = await remoteReady()
        env.cwd[1] = .success("/srv/app")
        model.isVisible = false
        await model.locateWorkingDirectory(); await model.waitUntilIdle()
        XCTAssertEqual(ds.lists.last, "/srv/app")
        XCTAssertTrue(model.isVisible)
    }

    // MARK: 传输批次与冲突

    private func uploadModel() async -> (FilerModel, FakeEnv, FakePrompter) {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: true)
        ds.dirs["/home"] = []
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web")); await model.waitUntilIdle()
        return (model, env, prompter)
    }

    private func urls(_ names: String...) -> [URL] { names.map { URL(fileURLWithPath: "/tmp/src/\($0)") } }

    func testUploadRunsItemsInOrderToLockedDestination() async {
        let (model, env, prompter) = await uploadModel()
        prompter.uploadSources = urls("a", "b", "c")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.map(\.item.name), ["a", "b", "c"])
        XCTAssertTrue(env.engine.starts.allSatisfy { $0.item.destination == "/home" && $0.policy == .fail && $0.item.kind == .upload })
        XCTAssertEqual(model.transferStatus?.phase, .completed(skipped: 0))
    }

    func testSecondItemFailureStopsRemainingAndKeepsError() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.script["b"] = [.failed(message: "disk full", alreadyExists: false)]
        prompter.uploadSources = urls("a", "b", "c")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.map(\.item.name), ["a", "b"], "失败后不再开始后面的项")
        XCTAssertEqual(model.transferStatus?.phase, .failed("disk full"))
        XCTAssertTrue(model.notice?.text.contains("disk full") == true, "详细错误保留")
        XCTAssertTrue(model.notice?.isError == true)
    }

    func testCancelledItemStopsBatchWithoutFailure() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.script["a"] = [.cancelled]
        prompter.uploadSources = urls("a", "b")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.count, 1)
        XCTAssertEqual(model.transferStatus?.phase, .cancelled)
        XCTAssertFalse(model.notice?.isError == true)
    }

    func testConflictAsksThenRetriesWithChosenPolicy() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.script["a"] = [.failed(message: "exists", alreadyExists: true), .completed(skipped: 0)]
        prompter.conflictAnswers = [.init(choice: .overwrite, applyToRest: false)]
        prompter.uploadSources = urls("a")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(prompter.conflictRequests.map(\.itemName), ["a"])
        XCTAssertEqual(env.engine.starts.map(\.policy), [.fail, .overwrite])
    }

    func testApplyToRestUsesChoiceForLaterConflictsWithoutAsking() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.script["a"] = [.failed(message: "exists", alreadyExists: true), .completed(skipped: 0)]
        env.engine.script["b"] = [.completed(skipped: 2)]
        prompter.conflictAnswers = [.init(choice: .skip, applyToRest: true)]
        prompter.uploadSources = urls("a", "b", "c")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(prompter.conflictRequests.count, 1, "勾选后其余冲突不再询问")
        XCTAssertEqual(env.engine.starts.map(\.policy), [.fail, .skip, .skip, .skip])
        XCTAssertEqual(model.notice?.text.isEmpty, false, "跳过数被汇报")
    }

    func testConflictCancelStopsBatch() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.script["a"] = [.failed(message: "exists", alreadyExists: true)]
        prompter.conflictAnswers = [.init(choice: .cancel, applyToRest: true)]
        prompter.uploadSources = urls("a", "b")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.count, 1)
        XCTAssertEqual(model.transferStatus?.phase, .cancelled)
    }

    func testNewBatchRejectedWhileOneRuns() async {
        let (model, env, prompter) = await uploadModel()
        prompter.uploadSources = urls("a")
        env.engine.extraEvents = []
        await model.chooseAndUpload(directories: false)
        XCTAssertTrue(model.hasActiveBatch)
        XCTAssertEqual(model.dropVerdict(urls: urls("z"), isFromSelf: false), .reject(reason: String(localized: "Busy. Wait for the current transfer to finish.")))
        prompter.uploadSources = urls("b")
        await model.chooseAndUpload(directories: false)
        XCTAssertTrue(model.notice?.isError == true)
        await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.map(\.item.name), ["a"])
    }

    func testDownloadSingleFileRenameAndExistingTargetUsesOverwrite() async throws {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: true)
        ds.dirs["/home"] = [.file("r.txt", in: "/home"), .dir("d", in: "/home")]
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "w")); await model.waitUntilIdle()
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("filer-dl-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        // 取消保存面板：没有传输副作用。
        model.setSelection(paths: ["/home/r.txt"])
        await model.downloadSelected()
        XCTAssertTrue(env.engine.starts.isEmpty)
        // 改名保存。
        prompter.saveURL = dir.appendingPathComponent("other.txt")
        await model.downloadSelected(); await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.last?.item.renamedTo, "other.txt")
        XCTAssertEqual(env.engine.starts.last?.item.destination, dir.path)
        // 目标已存在（用户在保存面板里确认过替换）：直接覆盖，不再询问。
        let existing = dir.appendingPathComponent("r.txt")
        FileManager.default.createFile(atPath: existing.path, contents: Data())
        prompter.saveURL = existing
        await model.downloadSelected(); await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.last?.policy, .overwrite)
        // 目录 / 多选：选父目录，保留名称。
        model.setSelection(paths: ["/home/d", "/home/r.txt"])
        prompter.downloadDirectory = dir
        await model.downloadSelected(); await model.waitForBatch()
        XCTAssertEqual(env.engine.starts.suffix(2).map(\.item.name), ["d", "r.txt"])
    }

    func testProgressUpdatesCarryRateAndOrigin() async {
        let (model, env, prompter) = await uploadModel()
        env.engine.extraEvents = [.progress(done: 40, total: 100)]
        prompter.uploadSources = urls("a")
        await model.chooseAndUpload(directories: false)
        await model.waitForBatch()
        XCTAssertTrue(model.transferStatus?.origin.contains("web") == true)
        XCTAssertEqual(model.transferStatus?.name, "a")
    }

    // MARK: 拖入判定

    func testDropVerdicts() async {
        let (model, env, _) = await uploadModel()
        XCTAssertEqual(model.dropVerdict(urls: urls("a"), isFromSelf: false), .accept(hint: String(localized: "Drop to upload")))
        XCTAssertEqual(model.dropVerdict(urls: urls("a"), isFromSelf: true), .ignore)
        if case .reject = model.dropVerdict(urls: [], isFromSelf: false) {} else { XCTFail("没有文件应拒绝") }
        model.follow(FilerFixtures.tab(1, kind: .ssh, name: "web", connection: .closed(reason: nil)))
        if case .reject = model.dropVerdict(urls: urls("a"), isFromSelf: false) {} else { XCTFail("断线应拒绝") }
        model.follow(FilerFixtures.tab(8, kind: .sftp, name: "f"))
        if case .reject = model.dropVerdict(urls: urls("a"), isFromSelf: false) {} else { XCTFail("纯 SFTP 应拒绝") }
        model.follow(nil)
        if case .reject = model.dropVerdict(urls: urls("a"), isFromSelf: false) {} else { XCTFail("无标签应拒绝") }
        _ = env
    }

    func testLocalDropCopiesAndRejectsSelfAndDescendant() async {
        let (model, env, _) = FilerFixtures.make()
        let ds = FakeDataSource(remote: false)
        ds.dirs["/work"] = []
        env.sources[1] = ds
        model.follow(FilerFixtures.tab(1)); await model.waitUntilIdle()
        model.open(directory: "/work"); await model.waitUntilIdle()
        XCTAssertEqual(model.dropVerdict(urls: [URL(fileURLWithPath: "/other/a")], isFromSelf: false),
                       .accept(hint: String(localized: "Drop to copy here")))
        XCTAssertEqual(model.dropVerdict(urls: [URL(fileURLWithPath: "/work/a")], isFromSelf: false),
                       .reject(reason: String(localized: "Already in this folder")))
        XCTAssertEqual(model.dropVerdict(urls: [URL(fileURLWithPath: "/work")], isFromSelf: false),
                       .reject(reason: String(localized: "Already in this folder").isEmpty ? "" : FilerModel.localCopyProblem("/work", into: "/work")!))
        XCTAssertEqual(FilerModel.localCopyProblem("/work", into: "/work/sub"), String(localized: "Can’t copy a folder into itself"))
        // 混合：已在此目录的跳过，其余复制。
        XCTAssertTrue(model.performDrop(urls: [URL(fileURLWithPath: "/work/a"), URL(fileURLWithPath: "/other/b")], isFromSelf: false))
        await model.waitForBatch()
        XCTAssertEqual(env.copyEngine.starts.map(\.item.name), ["b"])
        XCTAssertEqual(env.copyEngine.starts.first?.item.kind, .copy)
        XCTAssertTrue(model.notice?.text.isEmpty == false, "说明跳过数量")
        XCTAssertTrue(env.engine.starts.isEmpty)
    }

    func testDropGeometryUsesPointsNotPixels() {
        let panel = CGRect(x: 190, y: 0, width: 320, height: 600)
        // 2x 屏幕：松手点（点坐标）(300, 100) 在面板内。
        XCTAssertTrue(FilerDropGeometry.contains(locationInWindow: CGPoint(x: 300, y: 100), panelFrameInWindow: panel))
        // 如果把点坐标再按 2x 缩小（旧版缺陷），会落到面板左边。
        let wrong = FilerDropGeometry.points(fromPixels: CGPoint(x: 300, y: 100), scale: 2)
        XCTAssertFalse(FilerDropGeometry.contains(locationInWindow: wrong, panelFrameInWindow: panel))
        // 像素坐标（600, 200）换算后是同一个点。
        XCTAssertEqual(FilerDropGeometry.points(fromPixels: CGPoint(x: 600, y: 200), scale: 2), CGPoint(x: 300, y: 100))
    }

    // MARK: file promise

    func testFilePromiseDownloadsOnlyWhenDelivered_ReportsOnce() async {
        let engine = FakeEngine()
        let delivery = FilePromiseDelivery(entry: .file("r.bin", in: "/srv"), engine: engine)
        await Task.yield()
        XCTAssertTrue(engine.starts.isEmpty, "拖动途中、取消拖动、没有接收者时不下载")
        var results: [Error?] = []
        delivery.deliver(to: URL(fileURLWithPath: "/tmp/dest/r.bin")) { results.append($0) }
        for _ in 0..<5 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 30_000_000)
        XCTAssertEqual(engine.starts.count, 1)
        XCTAssertEqual(engine.starts[0].item.destination, "/tmp/dest")
        XCTAssertNil(engine.starts[0].item.renamedTo)
        XCTAssertEqual(results.count, 1)
        XCTAssertNil(results[0] as NSError?)
        // 再次交付不会重复下载或重复报告。
        delivery.deliver(to: URL(fileURLWithPath: "/tmp/dest/r.bin")) { results.append($0) }
        XCTAssertEqual(engine.starts.count, 1)
        XCTAssertTrue(delivery.isFinished)
    }

    func testFilePromiseFailureAndCancelAreReportedToReceiver() async {
        for (result, expectCancel) in [(FilerTransferResult.failed(message: "boom", alreadyExists: false), false), (.cancelled, true)] {
            let engine = FakeEngine()
            engine.script["f"] = [result]
            let delivery = FilePromiseDelivery(entry: .file("f"), engine: engine)
            var results: [Error?] = []
            delivery.deliver(to: URL(fileURLWithPath: "/tmp/x/f-2")) { results.append($0) }
            try? await Task.sleep(nanoseconds: 30_000_000)
            XCTAssertEqual(results.count, 1)
            let error = results[0] as NSError?
            XCTAssertNotNil(error, "失败 / 取消必须告诉接收方")
            XCTAssertEqual(error?.code == NSUserCancelledError, expectCancel)
            XCTAssertEqual(engine.starts[0].item.renamedTo, "f-2", "接收方改了名：下载后改名到它给的位置")
        }
    }

    func testSelfDragMarkerIsPerPasteboardNotGlobal() {
        let pb = NSPasteboard(name: NSPasteboard.Name("filer-test-\(UUID().uuidString)"))
        pb.clearContents()
        pb.writeObjects([FilerPasteboard.localItem(path: "/tmp/x")])
        XCTAssertTrue(FilerPasteboard.isFromFiler(pb))
        XCTAssertEqual(FilerPasteboard.fileURLs(pb).map(\.path), ["/tmp/x"])
        pb.clearContents()
        pb.setString("/tmp/y", forType: .string)
        XCTAssertFalse(FilerPasteboard.isFromFiler(pb), "标记不会残留到之后的拖放")
    }
}
