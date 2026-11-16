import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

@MainActor
final class SenderTests: XCTestCase {
    private var models: [SenderModel] = []

    private func makeModel() throws -> SenderModel {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        for snippet in core.snippets() { try core.deleteSnippet(id: snippet.id) }
        let model = SenderModel(core: core)
        models.append(model)
        model.isVisible = true
        model.refresh(context: nil, tabs: [])
        return model
    }

    override func tearDown() async throws {
        for model in models { model.shutdown(); model.core.shutdown() }
        models = []
    }

    private func snippet(_ name: String, text: String = "echo saved", scope: FfiSnippetScope = .global) -> FfiSnippet {
        FfiSnippet(id: UUID().uuidString.lowercased(), name: name, text: text, lineEnding: .crLf, scope: scope)
    }

    func testSenderMessagesRenderInBothLanguages() throws {
        let cases: [(String, [FfiMessageArg], String, String)] = [
            ("sender-empty-command", [], "The command cannot be blank", "命令不能为空白"),
            ("sender-busy", [], "Another command batch is being sent", "另一批命令正在发送"),
            ("sender-no-sessions", [], "No sessions available for sending", "没有可发送会话"),
            ("sender-repeat-replaced", [], "The previous repeat task has stopped", "旧重复任务已停止"),
            ("sender-target-closed", [], "Repeat task stopped: the target tab was closed", "重复任务已停止：目标标签已关闭"),
            ("sender-terminal-closed", [], "The terminal was closed", "终端已关闭"),
            ("sender-terminal-unavailable", [], "The terminal is disconnected or locked", "终端已断线或锁定"),
            ("sender-terminal-transferring", [], "The terminal is transferring files", "终端正在传输文件"),
            ("sender-write-failed", [], "Failed to write to the terminal", "终端写入失败"),
            ("sender-stopped", [.init(name: "sent", value: .int(value: 1)), .init(name: "total", value: .int(value: 2))], "Sent 1 / 2 lines", "已发送 1 / 2 行"),
            ("sender-repeat-completed", [.init(name: "rounds", value: .int(value: 2))], "Repeat task completed (2 rounds)", "重复任务完成（2 轮）"),
        ]
        for (key, args, english, chinese) in cases {
            let message = FfiMessage(key: key, args: args)
            XCTAssertEqual(CoreMessageText.text(message, languages: ["en"]), english)
            XCTAssertEqual(CoreMessageText.text(message, languages: ["zh-Hans"]), chinese)
        }
        for skipped in [Int64(0), 1, 2] {
            let message = FfiMessage(key: "sender-summary", args: [
                .init(name: "success", value: .int(value: 2 - skipped)),
                .init(name: "total", value: .int(value: 2)),
                .init(name: "skipped", value: .int(value: skipped)),
            ])
            XCTAssertEqual(CoreMessageText.text(message, languages: ["en"]), "\(2 - skipped) successful / 2 targets" + (skipped == 0 ? "" : ", \(skipped) skipped"))
            XCTAssertEqual(CoreMessageText.text(message, languages: ["zh-Hans"]), "成功 \(2 - skipped) / 共 2 个对象" + (skipped == 0 ? "" : "，跳过 \(skipped) 个对象"))
        }
        let model = try makeModel()
        model.draft.text = "echo sender"
        model.allTargets = true
        XCTAssertThrowsError(try model.core.senderStart(request: .init(text: " ", ending: .lf, target: nil, repeating: false, intervalSeconds: 1, count: 1), sessions: [])) { error in
            XCTAssertEqual(CoreMessageText.text(for: error, languages: ["en"]), "The command cannot be blank")
        }
        try model.core.senderStart(request: .init(text: "echo sender", ending: .lf, target: nil, repeating: true, intervalSeconds: 1, count: 1), sessions: [])
        let status = model.core.senderTick(sessions: [])
        XCTAssertTrue(status.hasFailures)
        XCTAssertEqual(status.notices.map(\.key), ["sender-summary", "sender-no-sessions", "sender-repeat-completed"])
    }

    func testEnterShiftEnterAndMarkedInput() throws {
        XCTAssertEqual(SenderModel.enterAction(shift: false, marked: false), .send)
        XCTAssertEqual(SenderModel.enterAction(shift: true, marked: false), .newline)
        XCTAssertEqual(SenderModel.enterAction(shift: false, marked: true), .inputMethod)
        XCTAssertEqual(SenderModel.enterAction(shift: true, marked: true), .inputMethod)
        let editor = SenderTextView(frame: NSRect(x: 0, y: 0, width: 300, height: 80))
        var sends = 0
        editor.onSend = { sends += 1 }
        func enter(_ modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
            NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0, windowNumber: 0, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36)!
        }
        editor.keyDown(with: enter())
        XCTAssertEqual(sends, 1)
        editor.keyDown(with: enter(.shift))
        XCTAssertTrue(editor.string.contains("\n"))
        XCTAssertEqual(sends, 1)
        editor.setMarkedText("输入", selectedRange: NSRange(location: 2, length: 0), replacementRange: NSRange(location: NSNotFound, length: 0))
        editor.keyDown(with: enter())
        XCTAssertEqual(sends, 1)
    }

    func testEditingSwitchCancelAndUpdateKeepDraft() throws {
        let model = try makeModel()
        model.draft = SenderDraft(text: "draft", name: "original", ending: .none)
        let original = model.draft
        let first = snippet("first")
        let second = snippet("second")
        try model.core.saveSnippet(snippet: first)
        try model.core.saveSnippet(snippet: second)
        model.reload()
        model.edit(first)
        model.edit(second)
        model.draft.text = "changed"
        model.cancelEditing()
        XCTAssertEqual(model.draft, original)
        model.edit(first)
        model.draft.text = "updated"
        model.save(scopeIndex: 0)
        XCTAssertNil(model.editing)
        XCTAssertEqual(model.draft.text, "updated")
        XCTAssertEqual(model.core.snippets().first { $0.id == first.id }?.text, "updated")
        model.edit(second)
        model.delete(second)
        XCTAssertEqual(model.draft.text, "updated")
    }

    func testExternalLibraryChangesCancelEditingAndRestoreDraft() throws {
        let model = try makeModel()
        let saved = snippet("saved")
        try model.core.saveSnippet(snippet: saved)
        model.reload()
        model.draft.text = "original draft"
        model.edit(saved)
        var imported = saved
        imported.text = "imported content"
        try model.core.saveSnippet(snippet: imported)
        model.core.notifyLibraryChanged()
        model.refresh(context: nil, tabs: [])
        XCTAssertNil(model.editing)
        XCTAssertEqual(model.draft.text, "original draft")
        XCTAssertEqual(model.visible.first?.text, "imported content")
    }

    func testPaginationVisibilityScopeFallbackAndConnectedTargets() throws {
        let model = try makeModel()
        let core = model.core
        let group = FfiGroup(id: UUID().uuidString.lowercased(), name: "Work")
        try core.saveGroup(group: group)
        let profile = SessionFixtures.ssh(UUID().uuidString.lowercased(), "Saved", group: group.id)
        try core.saveProfile(profile: profile)
        let session = try core.openSession(target: .profile(id: profile.id), size: FfiGridSize(cols: 80, rows: 24))
        let active = TabItem(id: session, kind: .ssh, name: "Saved", profileID: profile.id, connection: .connected)
        for index in 0..<25 { try core.saveSnippet(snippet: snippet(String(format: "%02d", index), scope: .profile(id: profile.id))) }
        try core.saveSnippet(snippet: snippet("global"))
        try core.saveSnippet(snippet: snippet("type", scope: .kind(kind: .ssh)))
        core.notifyLibraryChanged()
        let tabs = [active, TabItem(id: 999, kind: .sftp, name: "Files", connection: .connected), TabItem(id: 1000, kind: .local, name: "Closed", connection: .closed(reason: nil)), TabItem(id: 1001, kind: .local, name: "Local", connection: .connected, activeTransfers: 1)]
        model.refresh(context: active, tabs: tabs)
        XCTAssertEqual(model.connectedCount, 2)
        XCTAssertEqual(model.currentCount, 1)
        XCTAssertEqual(model.visible.count, 27)
        XCTAssertEqual(model.pageItems.count, 24)
        XCTAssertEqual(model.pageCount, 2)
        model.page = 1
        XCTAssertEqual(model.pageItems.count, 3)
        XCTAssertEqual(model.scopes.count, 4)
        model.draft.text = "saved here"
        model.save(scopeIndex: 3)
        model.refresh(context: nil, tabs: tabs)
        XCTAssertEqual(model.visible.map(\.name), ["global"])
        XCTAssertEqual(model.page, 0)
        XCTAssertEqual(model.defaultScopeIndex, 0)
        XCTAssertEqual(model.currentCount, 0)
        XCTAssertEqual(model.scopes.count, 1)
        model.refresh(context: tabs[1], tabs: tabs)
        XCTAssertEqual(model.currentCount, 0)
        XCTAssertEqual(model.scopes.count, 1)
    }

    func testWhitespaceCannotSaveUpdateOrRunAndNumbersClamp() throws {
        let model = try makeModel()
        var notices: [String] = []
        model.onNotice = { text, _ in notices.append(text) }
        for text in ["", " \t\n\r\n", "　\n"] {
            model.draft.text = text
            XCTAssertFalse(model.hasContent)
            XCTAssertFalse(model.canSave)
            model.save(scopeIndex: 0)
            model.start(repeating: false)
            model.start(repeating: true)
            XCTAssertFalse(model.manualRunning || model.repeatRunning)
        }
        XCTAssertTrue(notices.isEmpty)
        XCTAssertTrue(model.core.snippets().isEmpty)
        let menu = SenderPanel(model: model).contextMenu(for: snippet("blank", text: " \n"))
        XCTAssertFalse(menu.autoenablesItems)
        for title in [String(localized: "Repeat send"), String(localized: "Line ending"), String(localized: "Scope")] {
            XCTAssertEqual(menu.items.first { $0.title == title }?.isEnabled, false)
        }
        let saved = snippet("saved")
        try model.core.saveSnippet(snippet: saved)
        model.reload()
        model.edit(saved)
        model.draft.text = " \n"
        XCTAssertFalse(model.canSave)
        model.save(scopeIndex: 0)
        XCTAssertEqual(model.core.snippets().first?.text, saved.text)
        model.interval = -1; model.count = -3; model.clampRepeatValues()
        XCTAssertEqual(model.interval, 1); XCTAssertEqual(model.count, 0)
        model.interval = 999999; model.count = 999999; model.clampRepeatValues()
        XCTAssertEqual(model.interval, 86400); XCTAssertEqual(model.count, 100000)
    }

    func testSavedSftpOffersProfileAndGroupWithoutTerminalKindOrTarget() throws {
        let model = try makeModel()
        let group = FfiGroup(id: UUID().uuidString.lowercased(), name: "Files")
        try model.core.saveGroup(group: group)
        var profile = SessionFixtures.sftp(UUID().uuidString.lowercased(), "Files")
        profile.group = group.id
        try model.core.saveProfile(profile: profile)
        model.core.notifyLibraryChanged()
        let active = TabItem(id: 99, kind: .sftp, name: "Files", profileID: profile.id, connection: .connected)
        model.refresh(context: active, tabs: [active])
        XCTAssertEqual(model.scopes.map(\.level), [0, 2, 3])
        XCTAssertEqual(model.connectedCount, 0)
        XCTAssertEqual(model.currentCount, 0)
    }

    func testHiddenSenderLoadsOnlyAfterFirstShowAndLibraryChanges() throws {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        let model = SenderModel(core: core)
        models.append(model)
        for _ in 0..<100 { model.refresh(context: nil, tabs: []) }
        XCTAssertEqual(model.libraryLoadCount, 0)
        model.isVisible = true
        XCTAssertEqual(model.libraryLoadCount, 1)
        model.isVisible = false
        let local = TabItem(id: 123, kind: .local, name: "Local", connection: .connected)
        for _ in 0..<100 { model.refresh(context: local, tabs: [local]) }
        XCTAssertEqual(model.libraryLoadCount, 1)
        let added = snippet("new command")
        try core.saveSnippet(snippet: added)
        core.notifyLibraryChanged()
        XCTAssertEqual(model.libraryLoadCount, 1)
        model.isVisible = true
        XCTAssertEqual(model.libraryLoadCount, 2)
        XCTAssertTrue(model.visible.contains { $0.id == added.id })
    }

    func testSavedSendDoesNotReplaceDraftAndCollapseKeepsTasks() throws {
        let model = try makeModel()
        let session = try model.core.openSession(target: .defaultLocalShell, size: FfiGridSize(cols: 80, rows: 24))
        let active = TabItem(id: session, kind: .local, name: "Local", connection: .connected)
        model.refresh(context: active, tabs: [active])
        model.draft = SenderDraft(text: "draft", name: "name", ending: .none)
        let draft = model.draft
        model.count = 0
        model.start(repeating: true, snippet: snippet("saved"))
        XCTAssertTrue(model.repeatRunning)
        XCTAssertEqual(model.runningTitle, "echo saved")
        model.isVisible = false
        XCTAssertEqual(model.draft, draft)
        XCTAssertTrue(model.repeatRunning)
        model.stop(repeating: true)
        XCTAssertFalse(model.repeatRunning)
    }
}
