import AppKit
import KansoBridge
@testable import Kanso
import XCTest

/// 编辑对话框视图层（重做后的布局与"连接"按钮）的测试。视图模型逻辑见 `SessionEditorTests`。
@MainActor
final class SessionEditorViewTests: XCTestCase {
    private func makeEditor(kind: SessionKindChoice = .ssh, name: String = "n", store: SessionStore = MemoryStore())
        -> (SessionEditorViewController, SessionEditorModel)
    {
        let model = SessionEditorModel(store: store, channel: "Direct")
        model.name = name
        model.kind = kind
        model.host = "example.test"
        let editor = SessionEditorViewController(model: model)
        _ = editor.view
        return (editor, model)
    }

    /// 控件在对话框根视图坐标系里的外框。
    private func frame(of control: NSView, in root: NSView) -> NSRect {
        control.convert(control.bounds, to: root)
    }

    // MARK: 连接按钮

    func testConnectSavesThenFinishesThenConnects() async {
        let store = MemoryStore()
        let (editor, model) = makeEditor(store: store)
        var events: [String] = []
        editor.onFinish = { events.append("finish") }
        editor.onConnect = { events.append("connect:\($0)") }
        await editor.performSave(thenConnect: true).value
        XCTAssertEqual(store.storedProfiles.map(\.id), [model.profileID])
        XCTAssertEqual(events, ["finish", "connect:\(model.profileID)"], "先收起对话框，再开会话")
    }

    func testCustomLocalShellOpensWithoutSavingAProfile() async {
        let store = MemoryStore()
        let model = SessionEditorModel(store: store)
        model.shellCommand = "/bin/zsh"
        let editor = SessionEditorViewController(model: model, customLocal: true)
        _ = editor.view
        var events: [String] = []
        editor.onFinish = { events.append("finish") }
        editor.onLocalShellAction = { profile, open in
            XCTAssertEqual(profile.shellCommand, "/bin/zsh")
            events.append(open ? "open" : "default")
        }
        await editor.performSave(thenConnect: false).value
        await editor.performSave(thenConnect: true).value
        XCTAssertTrue(store.storedProfiles.isEmpty)
        XCTAssertEqual(events, ["default", "open", "finish"])
    }

    func testConnectDoesNotOpenWhenSaveFails() async {
        let store = MemoryStore()
        let (editor, model) = makeEditor(name: "  ", store: store)
        var finished = false, connected = false
        editor.onFinish = { finished = true }
        editor.onConnect = { _ in connected = true }
        await editor.performSave(thenConnect: true).value
        XCTAssertFalse(finished)
        XCTAssertFalse(connected)
        XCTAssertTrue(store.storedProfiles.isEmpty)
        XCTAssertNotNil(model.errorText)
        XCTAssertFalse(editor.shell.messageLabel.isHidden, "错误就地显示在底部")
        XCTAssertEqual(editor.shell.messageLabel.stringValue, model.errorText)
    }

    func testSaveOnlyDoesNotConnect() async {
        let (editor, _) = makeEditor()
        var finished = false, connected = false
        editor.onFinish = { finished = true }
        editor.onConnect = { _ in connected = true }
        await editor.performSave(thenConnect: false).value
        XCTAssertTrue(finished)
        XCTAssertFalse(connected)
    }

    func testButtonsReachTheSameFlowAsPerformSave() async {
        let (editor, _) = makeEditor()
        let connected = expectation(description: "connect")
        editor.onConnect = { _ in connected.fulfill() }
        editor.connectButton.performClick(nil)
        await fulfillment(of: [connected], timeout: 5)
    }

    func testFooterButtonOrderAndDefaultKeys() {
        let (editor, _) = makeEditor()
        XCTAssertEqual(editor.cancelButton.keyEquivalent, "\u{1b}")
        XCTAssertEqual(editor.saveButton.keyEquivalent, "\r")
        XCTAssertEqual(editor.connectButton.style, .primary)
        XCTAssertEqual(editor.connectButton.keyEquivalent, "")
    }

    // MARK: 布局

    func testPairedFieldsShareARow() {
        let (editor, _) = makeEditor(kind: .ssh)
        let root = editor.view
        root.layoutSubtreeIfNeeded()
        let pairs: [(NSView, NSView)] = [
            (editor.nameField, editor.colorPalette),
            (editor.hostField, editor.portField),
            (editor.userField, editor.authPopup),
            (editor.encodingPopup, editor.localeField),
        ]
        for (left, right) in pairs {
            let l = frame(of: left, in: root), r = frame(of: right, in: root)
            XCTAssertEqual(l.minY, r.minY, accuracy: 0.5, "并排字段顶端应对齐")
            XCTAssertLessThan(l.maxX, r.minX, "左字段在左、右字段在右")
        }
    }

    func testFullWidthFieldsSpanBothColumns() {
        let (editor, _) = makeEditor(kind: .ssh)
        let root = editor.view
        root.layoutSubtreeIfNeeded()
        let host = frame(of: editor.hostField, in: root), port = frame(of: editor.portField, in: root)
        let password = frame(of: editor.passwordField, in: root)
        let group = frame(of: editor.groupPopup, in: root)
        XCTAssertEqual(password.minX, host.minX, accuracy: 0.5)
        XCTAssertEqual(password.maxX, port.maxX, accuracy: 0.5)
        XCTAssertEqual(group.minX, host.minX, accuracy: 0.5)
        XCTAssertGreaterThan(password.minY, host.maxY, "整行字段在成对字段下面")
    }

    func testCardsFollowKind() {
        let (editor, model) = makeEditor(kind: .local)
        XCTAssertTrue(editor.hostField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.shellField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.encodingPopup.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.saveButton.isHidden)
        XCTAssertEqual(editor.connectButton.keyEquivalent, "\r")

        editor.kindCards.select(1)  // SSH
        XCTAssertEqual(model.kind, .ssh)
        XCTAssertFalse(editor.hostField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.shellField.isHiddenOrHasHiddenAncestor)
        XCTAssertFalse(editor.encodingPopup.isHiddenOrHasHiddenAncestor)

        editor.kindCards.select(2)  // SFTP：没有编码 / Locale
        XCTAssertEqual(model.kind, .sftp)
        XCTAssertFalse(editor.hostField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.encodingPopup.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.localeField.isHiddenOrHasHiddenAncestor)
    }

    func testAuthMethodSwitchesCredentialRows() {
        let (editor, _) = makeEditor(kind: .ssh)
        XCTAssertFalse(editor.passwordField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.keyPathField.isHiddenOrHasHiddenAncestor)
        editor.authPopup.userSelect(at: 1)  // 私钥
        XCTAssertTrue(editor.passwordField.isHiddenOrHasHiddenAncestor)
        XCTAssertFalse(editor.keyPathField.isHiddenOrHasHiddenAncestor)
        XCTAssertFalse(editor.keyPassphraseField.isHiddenOrHasHiddenAncestor)
        editor.authPopup.userSelect(at: 2)  // ssh-agent
        XCTAssertTrue(editor.keyPathField.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(editor.passwordField.isHiddenOrHasHiddenAncestor)
    }

    func testPasswordFieldsAreSecure() {
        let (editor, _) = makeEditor(kind: .ssh)
        XCTAssertTrue(editor.passwordField.textField is NSSecureTextField)
        XCTAssertTrue(editor.keyPassphraseField.textField is NSSecureTextField)
        XCTAssertFalse(editor.hostField.textField is NSSecureTextField)
    }

    func testColorPaletteWritesBackToModel() {
        let (editor, model) = makeEditor()
        XCTAssertEqual(editor.colorPalette.colors.count, 16)
        editor.colorPalette.choose(index: 5)
        XCTAssertEqual(model.color, SessionColorPresets.all[5])
        editor.colorPalette.choose(index: 5)
        XCTAssertNil(model.color)
    }

    func testDialogHeightIsCappedAndBodyScrolls() {
        let (editor, _) = makeEditor(kind: .ssh)
        editor.maxHeight = 400
        XCTAssertLessThanOrEqual(editor.shell.fittingHeight(maxHeight: 400), 400.5)
        editor.maxHeight = 2000
        XCTAssertGreaterThan(editor.shell.fittingHeight(maxHeight: 2000), 400)
    }
}
