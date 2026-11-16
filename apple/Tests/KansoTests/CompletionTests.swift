import AppKit
import KansoBridge
@testable import Kanso
@testable import TerminalKit
import XCTest

@MainActor
final class CompletionTests: XCTestCase {
    private let values = [
        FfiCompletionCandidate(replacement: "git status", category: .history, deleteCodepoints: 3),
        FfiCompletionCandidate(replacement: "git log --oneline", category: .git, deleteCodepoints: 3)
    ]

    func testCategoryLabelsHaveEnglishAndChineseTranslations() throws {
        let labels: [(FfiCompletionCategory, String.LocalizationValue, String, String)] = [
            (.directory, "Directory", "Directory", "目录"),
            (.file, "File", "File", "文件"),
            (.history, "History", "History", "历史"),
            (.kubernetes, "Kubernetes", "Kubernetes", "Kubernetes"),
            (.docker, "Docker", "Docker", "Docker"),
            (.git, "Git", "Git", "Git"),
            (.systemd, "systemd", "systemd", "systemd")
        ]
        for (language, chinese) in [("en", false), ("zh-Hans", true)] {
            let path = try XCTUnwrap(Bundle(for: CompletionPopupView.self).path(forResource: language, ofType: "lproj"))
            let bundle = try XCTUnwrap(Bundle(path: path))
            for (category, key, english, translated) in labels {
                XCTAssertEqual(category.localizedLabel, String(localized: key))
                XCTAssertEqual(String(localized: key, bundle: bundle), chinese ? translated : english)
            }
        }
    }

    func testPassiveKeysSelectionEscapeAndTab() {
        var model = CompletionModel()
        if case .forward = model.key(.tab) {} else { XCTFail("无请求时 Tab 应交给 shell") }
        let request = model.begin()
        if case .consume = model.key(.tab) {} else { XCTFail("等待时扣住 Tab") }
        model.receive(values, generation: request, finished: true)
        XCTAssertNil(model.selected)
        if case .forward = model.key(.up) {} else { XCTFail("被动时 ↑ 交给 shell") }
        if case .forward = model.key(.other) {} else { XCTFail("普通按键交给 shell") }
        _ = model.key(.down)
        XCTAssertEqual(model.selected, 0)
        _ = model.key(.down)
        _ = model.key(.down)
        XCTAssertEqual(model.selected, 1)
        _ = model.key(.up)
        XCTAssertEqual(model.selected, 0)
        _ = model.key(.other)
        XCTAssertNil(model.selected)
        if case .accept(let value) = model.key(.tab) { XCTAssertEqual(value, values[0]) } else { XCTFail("Tab 接受首项") }
        let next = model.begin()
        model.receive(values, generation: next, finished: true)
        _ = model.key(.down)
        _ = model.key(.down)
        if case .accept(let value) = model.key(.tab) { XCTAssertEqual(value, values[1]) } else { XCTFail("Tab 接受选中项") }
        _ = model.begin()
        _ = model.key(.escape)
        XCTAssertFalse(model.waiting)
        XCTAssertTrue(model.candidates.isEmpty)
    }

    func testOnlyNewestRequestAndImeAreAccepted() {
        var model = CompletionModel()
        let old = model.begin()
        let current = model.begin()
        model.receive(values, generation: old, finished: true)
        XCTAssertTrue(model.candidates.isEmpty)
        XCTAssertTrue(model.waiting)
        model.receive(values, generation: current, finished: false)
        _ = model.key(.down)
        if case .forward = model.key(.tab, composing: true) {} else { XCTFail("组字期间不能拦 Tab") }
        model.receive(values, generation: current, finished: true)
        XCTAssertTrue(model.candidates.isEmpty)
    }

    func testReplacementUsesOneBackspacePerUnicodeCodepoint() {
        let sink = RecordingSink()
        let original = "中e\u{301}👩‍💻"
        let value = FfiCompletionCandidate(replacement: "中文\\ 文件/", category: .directory, deleteCodepoints: UInt32(original.unicodeScalars.count))
        CompletionModel.replace(value, through: sink)
        XCTAssertEqual(sink.keys.count, original.unicodeScalars.count)
        XCTAssertTrue(sink.keys.allSatisfy { $0 == TerminalKeyEvent(key: .backspace) })
        XCTAssertEqual(sink.texts, [value.replacement])
        XCTAssertEqual(sink.events.last, .text(value.replacement))
    }

    func testCompletionInterceptorNeverRunsDuringComposition() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 500, height: 300))
        let ime = FakeInputMethod()
        view.inputMethod = ime
        var intercepted = 0
        view.completionKeyInterceptor = { _ in intercepted += 1; return true }
        view.setMarkedText("中文", selectedRange: NSRange(location: 2, length: 0), replacementRange: NSRange(location: NSNotFound, length: 0))
        view.keyDown(with: InputFixtures.key(.keyDown, VK.escape, characters: "\u{1b}"))
        XCTAssertEqual(intercepted, 0)
        XCTAssertEqual(ime.handledEvents.count, 1)
    }

    func testSettingsDefaultPersistenceAndWriteFailure() {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        XCTAssertFalse(settings.completion.enabled)
        settings.updateCompletion { $0.enabled = true }
        XCTAssertTrue(SettingsStore.attached(to: backend).completion.enabled)
        backend.failWrites = true
        settings.updateCompletion { $0.enabled = false }
        XCTAssertFalse(settings.completion.enabled)
        XCTAssertTrue(SettingsStore.attached(to: backend).completion.enabled)
        XCTAssertFalse(SettingsCodec.decode(CompletionSettings.self, from: "{}").map(\.enabled) ?? true)
    }

    func testHostDiscoveryWaitsForDirectoryOwnershipAndIsCached() {
        var discoveries = 0
        func hosts() -> Set<String> {
            discoveries += 1
            return ["my-host"]
        }
        let reader = CompletionSourceReader(remote: false, localHosts: hosts(), localHome: "/test-home",
            workingDirectory: { "/cwd" }, homeDirectory: { "/test-home" },
            listDirectory: { _ in [] }, readConfigs: { [] })
        XCTAssertEqual(discoveries, 0)
        for host in [nil, ""] as [String?] {
            let state = FfiCompletionState(input: "cat f", startColumn: 0, directory: "/cwd", directoryHost: host)
            XCTAssertTrue(reader.initial(state).allowPaths)
        }
        XCTAssertEqual(discoveries, 0)
        let local = FfiCompletionState(input: "cat f", startColumn: 0, directory: "/cwd", directoryHost: "my-host")
        XCTAssertTrue(reader.initial(local).allowPaths)
        let remote = FfiCompletionState(input: "cat f", startColumn: 0, directory: "/cwd", directoryHost: "other-host")
        XCTAssertFalse(reader.initial(remote).allowPaths)
        XCTAssertEqual(discoveries, 1)
    }

    func testSourceCachesExpiryAndHostOwnership() async {
        var time: TimeInterval = 0
        var reads = 0
        var cwdReads = 0
        var configReads = 0
        let reader = CompletionSourceReader(remote: false, localHosts: ["my-host"], localHome: "/test-home",
            workingDirectory: { cwdReads += 1; return "/cwd" }, homeDirectory: { "/remote-home" },
            listDirectory: { _ in reads += 1; return (0..<2500).map { FfiCompletionEntry(name: "file\($0)", directory: false) } },
            readConfigs: { configReads += 1; return ["contexts: []"] })
        reader.now = { time }
        let state = FfiCompletionState(input: "cat f", startColumn: 2, directory: nil, directoryHost: nil)
        func plan(_ sources: FfiCompletionSources) -> FfiCompletionSuggestions {
            FfiCompletionSuggestions(candidates: [], directory: sources.cwd.map { $0 + "/" }, needsKubeNames: true, needsCwd: sources.cwd == nil, needsHome: false)
        }
        let request = plan(reader.initial(state))
        let sources = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual(sources.entries.count, 2000)
        _ = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual([reads, cwdReads, configReads], [1, 1, 1])
        time = 1.6
        _ = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual([reads, cwdReads, configReads], [2, 1, 1])
        time = 2.1
        _ = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual(cwdReads, 2)
        time = 31
        _ = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual(configReads, 2)
        reader.invalidate()
        _ = await reader.enrich(state, request: request, plan: plan)
        XCTAssertEqual(configReads, 3)
        XCTAssertFalse(reader.initial(FfiCompletionState(input: "cat /", startColumn: 0, directory: "/srv", directoryHost: "foreign")).allowPaths)
        XCTAssertTrue(reader.initial(FfiCompletionState(input: "cat /", startColumn: 0, directory: "/cwd", directoryHost: "MY-HOST")).allowPaths)
    }

    func testLocalDirectoryReaderStopsAtTwoThousandEntries() async throws {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        for index in 0..<2100 { FileManager.default.createFile(atPath: directory.appendingPathComponent("file\(index)").path, contents: Data()) }
        var count = 0
        for try await batch in LocalFilerDataSource().list(path: directory.path, maxEntries: 2000) { count += batch.count }
        XCTAssertEqual(count, 2000)
    }

    func testSourceDiscoveryDoesNotRepeatFullCompletion() async throws {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 300), styleMask: [.borderless], backing: .buffered, defer: false)
        let view = TerminalView(frame: window.contentView!.bounds)
        window.contentView!.addSubview(view)
        XCTAssertTrue(window.makeFirstResponder(view))
        let reader = CompletionSourceReader(remote: true, localHosts: [], localHome: nil,
            workingDirectory: { "/cwd" }, homeDirectory: { "/home" },
            listDirectory: { _ in [.init(name: "file", directory: false)] }, readConfigs: { [] })
        let state = FfiCompletionState(input: "cat f", startColumn: 0, directory: nil, directoryHost: nil)
        var computations = 0
        var plans = 0
        let candidates = values
        let controller = CompletionController(view: view, sink: RecordingSink(), reader: reader,
            state: { state }, compute: { _, sources in
                computations += 1
                return .init(candidates: candidates, directory: sources.cwd.map { $0 + "/" }, needsKubeNames: false,
                             needsCwd: sources.cwd == nil, needsHome: sources.home == nil)
            }, plan: { _, sources in
                plans += 1
                return .init(candidates: [], directory: sources.cwd.map { $0 + "/" }, needsKubeNames: false, needsCwd: false, needsHome: false)
            })
        controller.enabled = true
        controller.visible = true
        for _ in 0..<100 where controller.model.waiting { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(controller.model.waiting)
        XCTAssertEqual(computations, 2)
        XCTAssertEqual(plans, 1)
        controller.invalidate()
        window.orderOut(nil)
    }

    func testLocalAndRemoteDeadlinesKeepFastResultsAndRejectLateSources() async throws {
        for remote in [false, true] {
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 300), styleMask: [.borderless], backing: .buffered, defer: false)
            let view = TerminalView(frame: window.contentView!.bounds)
            window.contentView!.addSubview(view)
            XCTAssertTrue(window.makeFirstResponder(view))
            var continuation: CheckedContinuation<[FfiCompletionEntry], Never>?
            let reader = CompletionSourceReader(remote: remote, localHosts: [], localHome: nil,
                workingDirectory: { "/cwd" }, homeDirectory: { "/home" },
                listDirectory: { _ in await withCheckedContinuation { continuation = $0 } }, readConfigs: { [] })
            let state = FfiCompletionState(input: "cat f", startColumn: 0, directory: "/cwd", directoryHost: nil)
            let fast = values[0]
            let late = values[1]
            let controller = CompletionController(view: view, sink: RecordingSink(), reader: reader,
                state: { state }, compute: { _, sources in
                    FfiCompletionSuggestions(candidates: sources.entries.isEmpty ? [fast] : [late], directory: "/cwd/", needsKubeNames: false, needsCwd: false, needsHome: false)
                }, plan: { _, _ in XCTFail("已知目录不应重新探测补全"); return .init(candidates: [], directory: nil, needsKubeNames: false, needsCwd: false, needsHome: false) })
            controller.enabled = true
            controller.visible = true
            XCTAssertEqual(controller.model.candidates, [fast])
            XCTAssertTrue(controller.model.waiting)
            try await Task.sleep(for: .milliseconds(400))
            XCTAssertEqual(controller.model.waiting, remote)
            if remote { try await Task.sleep(for: .milliseconds(1300)) }
            XCTAssertFalse(controller.model.waiting)
            XCTAssertEqual(controller.model.candidates, [fast])
            XCTAssertNotNil(continuation)
            continuation?.resume(returning: [FfiCompletionEntry(name: "file", directory: false)])
            try await Task.sleep(for: .milliseconds(20))
            XCTAssertEqual(controller.model.candidates, [fast])
            controller.invalidate()
            window.orderOut(nil)
        }
    }

    func testPassivePanelAlignsWithInputAndFlipsAboveCursor() {
        let panel = FloatingPanel(content: NSView(), size: NSSize(width: 240, height: 100), passive: true)
        let bounds = NSRect(x: 0, y: 0, width: 800, height: 600)
        let anchor = NSRect(x: 120, y: 300, width: 1, height: 20)
        panel.position(below: anchor, within: bounds)
        XCTAssertEqual(panel.frame.minX, anchor.minX)
        XCTAssertEqual(panel.frame.maxY, anchor.minY - 6)
        let bottom = NSRect(x: 120, y: 20, width: 1, height: 20)
        panel.position(below: bottom, within: bounds)
        XCTAssertEqual(panel.frame.minY, bottom.maxY + 6)
        XCTAssertFalse(panel.canBecomeKey)
        panel.dismiss()
    }
}
