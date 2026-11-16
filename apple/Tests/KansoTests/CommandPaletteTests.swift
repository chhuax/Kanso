import AppKit
import KansoUI
import XCTest
@testable import Kanso

@MainActor
final class CommandPaletteTests: XCTestCase {
    // MARK: - 模糊匹配

    func testFuzzyEmptyQueryMatches() {
        XCTAssertEqual(CommandPaletteFuzzy.score(query: "", in: "anything"), 0)
        XCTAssertEqual(CommandPaletteFuzzy.score(query: "   ", in: "x"), nil)
        // 模型侧会 trim；底层空串匹配。
        XCTAssertEqual(CommandPaletteFuzzy.score(query: "", in: ""), 0)
    }

    func testFuzzySubsequenceAndCase() {
        XCTAssertNotNil(CommandPaletteFuzzy.score(query: "abc", in: "aXbYc"))
        XCTAssertNotNil(CommandPaletteFuzzy.score(query: "CMD", in: "command"))
        XCTAssertNil(CommandPaletteFuzzy.score(query: "xyz", in: "abc"))
        XCTAssertNil(CommandPaletteFuzzy.score(query: "ba", in: "ab"))
    }

    func testFuzzyPrefersTighterMatch() {
        let ranked = CommandPaletteFuzzy.ranked(["configure", "cfg", "config"], query: "cfg") { $0 }
        XCTAssertEqual(ranked.map(\.item), ["cfg", "config", "configure"])
    }

    // MARK: - 收集与过滤

    func testQueriesNormalizeOnlyTheQueryAndHeightMeasuresVisibleRows() {
        var normalizations = 0
        let model = CommandPaletteModel(normalize: { text in
            normalizations += 1
            return CommandPaletteFuzzy.normalize(text)
        })
        var snapshot = sampleSnapshot()
        snapshot.files = (0..<10_000).map { .init(name: "file \($0)", path: "/file/\($0)") }
        model.replaceSnapshot(snapshot)
        let preparedCount = normalizations
        for index in 0..<100 { model.query = "file \(index)" }
        XCTAssertEqual(normalizations, preparedCount + 100)
        model.query = "file"
        let controller = CommandPaletteViewController(model: model)
        controller.loadViewIfNeeded()
        XCTAssertEqual(controller.preferredContentSize.height, CommandPaletteViewController.maxHeight)
        XCTAssertLessThan(controller.measuredRowCount, 25)
        XCTAssertGreaterThan(model.rows.count, 10_000)
    }

    func testDirectoryEnumerationRunsOffMainAndMergesIntoCurrentQuery() async {
        let model = CommandPaletteModel()
        model.replaceSnapshot(sampleSnapshot())
        let started = expectation(description: "后台枚举已开始")
        let gate = DispatchSemaphore(value: 0)
        let loader = Task {
            await CommandPaletteFileLister.load(directory: "/fixture", list: { _ in
                XCTAssertFalse(Thread.isMainThread)
                started.fulfill()
                gate.wait()
                return [.init(name: "needle.txt", path: "/fixture/needle.txt")]
            })
        }
        await fulfillment(of: [started], timeout: 3)
        XCTAssertFalse(model.rows.isEmpty)
        model.query = "needle"
        XCTAssertTrue(model.rows.isEmpty)
        gate.signal()
        model.replaceFiles(await loader.value)
        XCTAssertEqual(model.query, "needle")
        XCTAssertEqual(model.snapshot.files.map(\.name), ["needle.txt"])
        XCTAssertEqual(model.rows.count, 2)
    }

    private func sampleSnapshot() -> CommandPaletteSnapshot {
        CommandPaletteSnapshot(
            tabs: [
                .init(id: 1, title: "~/proj", detail: "⌘1"),
                .init(id: 2, title: "prod-web", detail: "⌘2"),
            ],
            sessions: [
                .init(id: "s1", title: "db-primary", detail: "deploy@db1:22"),
                .init(id: "s2", title: "build-box", detail: "builder@ci:22"),
            ],
            commands: [
                .init(id: "kanso.command.find", title: "Find…", shortcut: "⌘F"),
                .init(id: "kanso.command.toggleFiler", title: "Toggle File Browser", shortcut: "⌥⌘→"),
                .init(id: "kanso.command.commandPalette", title: "Command Palette", shortcut: "⇧⌘P"),
            ],
            files: [
                .init(name: "main.swift", path: "/tmp/demo/main.swift"),
                .init(name: "README.md", path: "/tmp/demo/README.md"),
                .init(name: "build.sh", path: "/tmp/demo/build.sh"),
            ]
        )
    }

    func testFilterGroupsFourKinds() {
        let model = CommandPaletteModel()
        model.replaceSnapshot(sampleSnapshot())
        XCTAssertEqual(sectionKinds(model), [.tabs, .sessions, .commands, .files])
        // 组内按标题本地化排序（空查询分数相同）。
        XCTAssertEqual(hitTitles(model), [
            "~/proj", "prod-web",
            "build-box", "db-primary",
            "Command Palette", "Find…", "Toggle File Browser",
            "build.sh", "main.swift", "README.md",
        ])
    }

    func testFilterByQueryKeepsGroups() {
        let model = CommandPaletteModel()
        model.replaceSnapshot(sampleSnapshot())
        model.query = "build"
        XCTAssertEqual(sectionKinds(model), [.sessions, .files])
        XCTAssertEqual(hitTitles(model), ["build-box", "build.sh"])
        model.query = "find"
        XCTAssertEqual(hitTitles(model), ["Find…"])
        model.query = "zzz-no-match"
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testSshActiveTabYieldsNoFilesInSnapshotBuilder() {
        // 文件列表本身由呈现方按「本地 + cwd」填充；空 files 表示 SSH / 无目录。
        let model = CommandPaletteModel()
        var snap = sampleSnapshot()
        snap.files = []
        model.replaceSnapshot(snap)
        XCTAssertFalse(sectionKinds(model).contains(.files))
    }

    func testFileListerOneLevel() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("kanso-palette-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try "a".write(to: dir.appendingPathComponent("alpha.txt"), atomically: true, encoding: .utf8)
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("subdir"), withIntermediateDirectories: true)
        let files = CommandPaletteFileLister.list(directory: dir.path)
        XCTAssertEqual(Set(files.map(\.name)), ["alpha.txt", "subdir"])
        XCTAssertTrue(files.allSatisfy { $0.path.hasPrefix(dir.path) })
    }

    func testMenuCollectorSkipsDisabledAndPaletteSelf() {
        let menu = NSMenu()
        let enabled = menu.addItem(withTitle: "Find…", action: #selector(NSText.copy(_:)), keyEquivalent: "f")
        enabled.identifier = KeyCommand.find.menuIdentifier
        enabled.keyEquivalentModifierMask = .command
        let disabled = menu.addItem(withTitle: "Hidden", action: #selector(NSText.copy(_:)), keyEquivalent: "")
        disabled.isEnabled = false
        let palette = menu.addItem(withTitle: "Command Palette", action: #selector(MainWindowController.kansoCommandPalette(_:)), keyEquivalent: "p")
        palette.identifier = KeyCommand.commandPalette.menuIdentifier
        palette.keyEquivalentModifierMask = [.command, .shift]

        let collected = CommandPaletteMenuCollector.collect(from: menu)
        XCTAssertTrue(collected.contains { $0.id == KeyCommand.find.menuIdentifier.rawValue })
        XCTAssertEqual(CommandPaletteMenuCollector.shortcutDisplay(for: enabled), "⌘F")
        // 禁用项不进列表。
        XCTAssertFalse(collected.contains { $0.title == "Hidden" })
    }

    func testShortcutDisplayMatchesKeyChord() {
        let menu = NSMenu()
        let split = menu.addItem(withTitle: "Split Right", action: #selector(NSText.copy(_:)), keyEquivalent: "\\")
        split.keyEquivalentModifierMask = .command
        let arrowChar = Character(UnicodeScalar(UInt32(NSRightArrowFunctionKey))!)
        let arrow = menu.addItem(withTitle: "Toggle File Browser", action: #selector(NSText.copy(_:)),
                                 keyEquivalent: String(arrowChar))
        arrow.keyEquivalentModifierMask = [.command, .option]
        XCTAssertEqual(CommandPaletteMenuCollector.shortcutDisplay(for: split), KeyChord(key: "\\", modifiers: .command).display)
        XCTAssertEqual(CommandPaletteMenuCollector.shortcutDisplay(for: split), "⌘\\")
        XCTAssertEqual(
            CommandPaletteMenuCollector.shortcutDisplay(for: arrow),
            KeyChord(key: "right", modifiers: [.command, .option]).display
        )
        XCTAssertEqual(CommandPaletteMenuCollector.shortcutDisplay(for: arrow), "⌥⌘→")
    }

    // MARK: - 执行分发

    func testExecuteDispatchesByKind() {
        let model = CommandPaletteModel()
        model.replaceSnapshot(sampleSnapshot())
        var tabs: [SessionID] = []
        var sessions: [String] = []
        var commands: [String] = []
        var files: [String] = []
        var dismissed = 0
        model.actions = .init(
            activateTab: { tabs.append($0) },
            openSession: { sessions.append($0) },
            runCommand: { commands.append($0) },
            revealFile: { files.append($0) },
            dismiss: { dismissed += 1 }
        )

        model.query = "prod"
        XCTAssertTrue(model.executeSelection())
        XCTAssertEqual(tabs, [2])
        XCTAssertEqual(dismissed, 1)

        model.replaceSnapshot(sampleSnapshot())
        model.query = "db-pri"
        XCTAssertTrue(model.executeSelection())
        XCTAssertEqual(sessions, ["s1"])

        model.replaceSnapshot(sampleSnapshot())
        model.query = "Toggle File"
        XCTAssertTrue(model.executeSelection())
        XCTAssertEqual(commands, ["kanso.command.toggleFiler"])

        model.replaceSnapshot(sampleSnapshot())
        model.query = "main.sw"
        XCTAssertTrue(model.executeSelection())
        XCTAssertEqual(files, ["/tmp/demo/main.swift"])
        XCTAssertEqual(dismissed, 4)
    }

    func testMoveSelectionSkipsHeaders() {
        let model = CommandPaletteModel()
        model.replaceSnapshot(sampleSnapshot())
        XCTAssertEqual(model.selectedIndex, 1) // 第一个 hit（跳过 Open Tabs 头）
        model.moveSelection(by: 1)
        if case .hit(let hit) = model.rows[model.selectedIndex!] {
            XCTAssertEqual(hit.title, "prod-web")
        } else {
            XCTFail("应为 hit")
        }
        model.moveSelection(by: -10)
        if case .hit(let hit) = model.rows[model.selectedIndex!] {
            XCTAssertEqual(hit.title, "~/proj")
        } else {
            XCTFail("应为 hit")
        }
    }

    // MARK: - helpers

    private func sectionKinds(_ model: CommandPaletteModel) -> [CommandPaletteSection] {
        model.rows.compactMap {
            if case .header(let s) = $0 { return s }
            return nil
        }
    }

    private func hitTitles(_ model: CommandPaletteModel) -> [String] {
        model.rows.compactMap {
            if case .hit(let h) = $0 { return h.title }
            return nil
        }
    }
}
