import AppKit
import KansoUI
import TerminalKit
@testable import Kanso
import XCTest

/// 面板视图、状态栏条目、菜单与终端拖入的结构测试（不启动 App，不弹模态框）。
@MainActor
final class FilerPanelTests: XCTestCase {
    private func makePanel(remote: Bool = false, entries: [FilerEntry]? = nil) async -> (FilerPanelView, FilerModel, FakeEnv, FakePrompter) {
        let (model, env, prompter) = FilerFixtures.make()
        let ds = FakeDataSource(remote: remote)
        ds.dirs["/home"] = entries ?? [.dir("src", in: "/home"), .file("a.txt", in: "/home"), .link("ln", in: "/home"), .file(".hidden", in: "/home")]
        env.sources[1] = ds
        let panel = FilerPanelView(model: model)
        panel.frame = NSRect(x: 0, y: 0, width: 320, height: 500)
        model.follow(remote ? FilerFixtures.tab(1, kind: .ssh, name: "web") : FilerFixtures.tab(1))
        await model.waitUntilIdle()
        panel.layoutSubtreeIfNeeded()
        return (panel, model, env, prompter)
    }

    func testPanelShowsRowsAndStatsFooter() async {
        let (panel, _, _, _) = await makePanel()
        XCTAssertEqual(panel.visibleRowCount, 4)
        XCTAssertEqual(panel.footerText, FilerStats(directories: 1, files: 2, links: 1).text)
        XCTAssertEqual(panel.pathBar.text, "/home")
    }

    func testProgressAndSelectionDoNotRebuildDirectoryOrReloadTable() async {
        let entries = (0..<10000).map { FilerEntry.file("file\($0)", in: "/home") }
        let (panel, model, _, _) = await makePanel(entries: entries)
        let reloads = panel.listingReloadCount
        let revision = model.listingRevision
        for index in 0..<100 {
            model.setSelection(paths: [entries[index].path])
            model.showRemoteEditStatus(nil, download: FilerTransferStatus(kind: .download, name: "file", index: 1, count: 1))
        }
        XCTAssertEqual(panel.listingReloadCount, reloads)
        XCTAssertEqual(model.listingRevision, revision)
        XCTAssertEqual(model.selectedEntries, [entries[99]])
        model.showsHiddenFiles.toggle()
        XCTAssertEqual(panel.listingReloadCount, reloads + 1)
    }

    func testHiddenProgressBarStopsTimerAndRestartsWhenShown() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 100, height: 50), styleMask: [.borderless], backing: .buffered, defer: false)
        let parent = NSView(frame: window.contentView!.bounds)
        let progress = FilerProgressBar(frame: parent.bounds)
        window.contentView!.addSubview(parent)
        parent.addSubview(progress)
        XCTAssertTrue(progress.isAnimating)
        progress.isHidden = true
        XCTAssertFalse(progress.isAnimating)
        progress.isHidden = false
        XCTAssertTrue(progress.isAnimating)
        parent.isHidden = true
        XCTAssertFalse(progress.isAnimating)
        parent.isHidden = false
        XCTAssertTrue(progress.isAnimating)
        progress.fraction = 0.5
        XCTAssertFalse(progress.isAnimating)
        progress.removeFromSuperview()
    }

    func testHiddenToggleUpdatesRowsAndFooter() async {
        let (panel, model, _, _) = await makePanel()
        model.showsHiddenFiles = false
        XCTAssertEqual(panel.visibleRowCount, 3)
        XCTAssertEqual(panel.footerText, FilerStats(directories: 1, files: 1, links: 1).text)
    }

    func testEmptyAndErrorStatesAreNotBlankLists() async {
        let (panel, model, env, _) = await makePanel(entries: [])
        XCTAssertEqual(panel.stateText, String(localized: "Empty folder"))
        (env.sources[1])?.errors["/nope"] = FilerError(.notFound, path: "/nope")
        model.open(directory: "/nope"); await model.waitUntilIdle()
        XCTAssertTrue(panel.stateText.contains(FilerError(.notFound).reason))
        XCTAssertTrue(panel.stateText.contains("/nope"))
    }

    func testRemoteFileMenuHasOpenOpenWithAndDownload() async {
        let (panel, model, _, _) = await makePanel(remote: true, entries: [.file("r.txt", in: "/home")])
        model.setSelection(paths: ["/home/r.txt"])
        let menu = try! XCTUnwrap(panel.debugMenu(row: 0))
        let titles = menu.items.map(\.title)
        XCTAssertTrue(titles.contains(String(localized: "Download")))
        let open = try! XCTUnwrap(menu.items.first { $0.title == String(localized: "Open") })
        XCTAssertTrue(open.isEnabled)
        let with = try! XCTUnwrap(menu.items.first { $0.title == String(localized: "Open With") })
        XCTAssertEqual(with.submenu?.items.last?.title, String(localized: "Other Application…"))
        XCTAssertEqual(titles.filter { $0 == String(localized: "Copy Full Path") || $0 == String(localized: "Copy Name") || $0 == String(localized: "Rename") }.count, 3)
    }

    func testLocalFileMenuHasNoDownloadAndBlankMenuHasUploadOnlyForRemote() async {
        let (panel, model, _, _) = await makePanel(entries: [.file("l.txt", in: "/home")])
        model.setSelection(paths: ["/home/l.txt"])
        XCTAssertFalse(try! XCTUnwrap(panel.debugMenu(row: 0)).items.contains { $0.title == String(localized: "Download") })
        XCTAssertFalse(try! XCTUnwrap(panel.debugMenu(row: -1)).items.contains { $0.title == String(localized: "Upload Files…") })
        let (remotePanel, _, _, _) = await makePanel(remote: true)
        let blank = try! XCTUnwrap(remotePanel.debugMenu(row: -1)).items.map(\.title)
        for t in [String(localized: "New File"), String(localized: "New Folder"), String(localized: "Upload Files…"), String(localized: "Upload Folders…"),
                  String(localized: "Copy Directory Path"), String(localized: "Refresh")] {
            XCTAssertTrue(blank.contains(t), t)
        }
    }

    func testDoubleClickPathActivatesDirectory() async {
        let (panel, model, env, _) = await makePanel()
        env.sources[1]?.dirs["/home/src"] = [.file("m", in: "/home/src")]
        model.activate(.dir("src", in: "/home")); await model.waitUntilIdle()
        XCTAssertEqual(panel.pathBar.text, "/home/src")
        XCTAssertEqual(panel.visibleRowCount, 1)
    }

    func testTransferProgressAreaShowsDirectionAndStatsReturn() async {
        let (panel, model, env, prompter) = await makePanel(remote: true)
        env.engine.extraEvents = [.progress(done: 25, total: 100)]
        prompter.uploadSources = [URL(fileURLWithPath: "/tmp/up.bin")]
        await model.chooseAndUpload(directories: false); await model.waitForBatch()
        XCTAssertTrue(panel.progressTitleText.contains("up.bin"))
        await model.waitUntilIdle()
        model.dismissTransferStatus()
        XCTAssertEqual(panel.footerText, model.stats.text)
    }

    func testBreadcrumbSegmentsFitAndNavigate() {
        let bar = FilerPathBar(frame: NSRect(x: 0, y: 0, width: 160, height: 26))
        var navigated: String?
        bar.onNavigate = { navigated = $0 }
        bar.text = "/Users/someone/projects/kanso/apple"
        bar.layoutSubtreeIfNeeded()
        let segs = bar.crumbs.visibleSegments
        XCTAssertEqual(segs.last?.path, "/Users/someone/projects/kanso/apple")
        XCTAssertLessThan(segs.count, 6, "放不下时从左侧省略")
        bar.onNavigate?("/x"); XCTAssertEqual(navigated, "/x")
    }

    func testStatusBarFilerItem() {
        let bar = StatusBarView(frame: NSRect(x: 0, y: 0, width: 800, height: 24))
        var toggles = 0
        bar.onToggleFiler = { toggles += 1 }
        bar.setFiler(expanded: true, unavailableReason: nil)
        XCTAssertTrue(bar.isFilerHighlighted)
        bar.clickFilerItem(); XCTAssertEqual(toggles, 1)
        bar.setFiler(expanded: false, unavailableReason: "reason")
        XCTAssertFalse(bar.isFilerEnabled)
        bar.clickFilerItem(); XCTAssertEqual(toggles, 1, "不可用时不响应")
        bar.showNotice("已定位", isError: false)
        XCTAssertEqual(bar.noticeMessage, "已定位")
        XCTAssertEqual(bar.statusText, "已定位")
        bar.setStatus("Connecting…")
        XCTAssertEqual(bar.statusText, "已定位", "提示优先于状态文字")
    }

    func testMenuHasFilerAndLocateItemsWithShortcuts() throws {
        let settings = SettingsStore()
        let menu = AppDelegate.makeMainMenu(appearance: AppearanceController(settings: settings, apply: { _ in }),
                                            gutter: GutterModeController(settings: settings))
        let all = menu.items.flatMap { $0.submenu?.items ?? [] }
        let toggle = try XCTUnwrap(all.first { $0.action == #selector(MainWindowController.kansoToggleFiler(_:)) })
        XCTAssertEqual(toggle.keyEquivalentModifierMask, [.command, .option])
        XCTAssertEqual(toggle.keyEquivalent, String(UnicodeScalar(UInt32(NSRightArrowFunctionKey))!.description))
        let locate = try XCTUnwrap(all.first { $0.action == #selector(MainWindowController.kansoLocateWorkingDirectory(_:)) })
        XCTAssertEqual(locate.keyEquivalent, "j")
        XCTAssertTrue(MainWindowController.instancesRespond(to: toggle.action))
        XCTAssertTrue(MainWindowController.instancesRespond(to: locate.action))
    }

    func testTerminalViewAcceptsFileURLDrops() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 100, height: 100))
        XCTAssertTrue(view.registeredDraggedTypes.contains(.fileURL))
        XCTAssertFalse(view.registeredDraggedTypes.contains(NSPasteboard.PasteboardType("com.apple.pasteboard.promised-file-url")),
                       "远端条目的 file promise 不会被终端接收")
    }
}
