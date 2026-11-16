import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

/// 记录型应答者：不连 core。
@MainActor
final class RecordingResponder: PromptResponder {
    private(set) var responses: [(prompt: UInt64, response: FfiPromptResponse)] = []
    func respond(prompt: UInt64, response: FfiPromptResponse) { responses.append((prompt, response)) }
}

/// 记录型显示层：不弹任何 sheet，由测试手动"点击"。
@MainActor
final class RecordingSheetHost: PromptSheetHost {
    var canPresent = true
    private(set) var presented: [PromptDialogModel] = []
    private(set) var dismissCount = 0
    private var completion: (@MainActor (FfiPromptResponse) -> Void)?

    func present(_ model: PromptDialogModel, completion: @escaping @MainActor (FfiPromptResponse) -> Void) {
        presented.append(model)
        self.completion = completion
    }

    func dismissCurrent() {
        dismissCount += 1
        completion = nil
    }

    /// 模拟用户作出选择。
    func complete(_ response: FfiPromptResponse) {
        let c = completion
        completion = nil
        c?(response)
    }
}

private let endpoint = FfiEndpoint(host: "example.com", port: 2222, via: ["jump1", "jump2"])

private func hostKeyRequest() -> FfiPromptRequest {
    .hostKeyChanged(endpoint: endpoint, keyType: "ssh-ed25519", fingerprintSha256: "SHA256:abc",
                    knownHostsPath: "/Users/u/.ssh/known_hosts", line: 7)
}

private func kbdRequest(name: String = "", instruction: String = "", preamble: [String] = [],
                        questions: [FfiKbdQuestion]) -> FfiPromptRequest {
    .keyboardInteractive(endpoint: endpoint, user: "root", name: name, instruction: instruction,
                         preamble: preamble, questions: questions)
}

@MainActor
final class PromptViewModelTests: XCTestCase {
    func testHostKeyFieldsAndDefaultButtonIsCancel() throws {
        guard case .hostKeyChanged(let m)? = PromptDialogModel(request: hostKeyRequest(), sessionTitle: "prod") else {
            return XCTFail("应为主机密钥对话框")
        }
        XCTAssertEqual(m.target.hostPort, "example.com:2222")
        XCTAssertEqual(m.target.via, ["jump1", "jump2"])
        XCTAssertNotNil(m.target.viaText)
        XCTAssertEqual(m.target.sessionTitle, "prod")
        XCTAssertEqual(m.keyType, "ssh-ed25519")
        XCTAssertEqual(m.fingerprint, "SHA256:abc")
        XCTAssertEqual(m.knownHostsLocation, "/Users/u/.ssh/known_hosts:7")
        XCTAssertEqual(m.defaultButton, .cancel, "回车不应接受变了的密钥")
        XCTAssertEqual(m.acceptResponse, .acceptHostKey)
        XCTAssertEqual(m.cancelResponse, .cancel)
    }

    func testDirectConnectionHasNoViaText() {
        let t = PromptTarget(endpoint: FfiEndpoint(host: "h", port: 22, via: []), sessionTitle: nil)
        XCTAssertNil(t.viaText)
        XCTAssertEqual(t.hostPort, "h:22")
    }

    func testKeyboardInteractiveFieldsAndAnswersInOrder() throws {
        let request = kbdRequest(
            name: "Duo", instruction: "Approve", preamble: ["Push sent", "Check phone"],
            questions: [FfiKbdQuestion(text: "Password: ", echo: false), FfiKbdQuestion(text: "Code: ", echo: true)]
        )
        guard case .keyboardInteractive(let m)? = PromptDialogModel(request: request, sessionTitle: nil) else {
            return XCTFail("应为 keyboard-interactive")
        }
        XCTAssertEqual(m.user, "root")
        XCTAssertEqual(m.name, "Duo")
        XCTAssertEqual(m.instruction, "Approve")
        XCTAssertEqual(m.preamble, ["Push sent", "Check phone"])
        XCTAssertEqual(m.fields, [
            .init(prompt: "Password: ", isSecure: true), .init(prompt: "Code: ", isSecure: false),
        ])
        XCTAssertEqual(m.defaultButton, .primary)
        m.setAnswer("123456", at: 1)
        m.setAnswer("pw", at: 0)
        m.setAnswer("ignored", at: 5)
        XCTAssertEqual(m.continueResponse, .answers(answers: ["pw", "123456"]))
        m.clear()
        XCTAssertEqual(m.continueResponse, .answers(answers: ["", ""]))
        XCTAssertEqual(m.cancelResponse, .cancel)
    }

    func testKeyboardInteractiveHidesEmptyNameAndInstruction() throws {
        guard case .keyboardInteractive(let m)? = PromptDialogModel(
            request: kbdRequest(questions: []), sessionTitle: nil
        ) else { return XCTFail() }
        XCTAssertNil(m.name)
        XCTAssertNil(m.instruction)
        XCTAssertTrue(m.preamble.isEmpty)
        XCTAssertEqual(m.continueResponse, .answers(answers: []))
    }

    func testCredentialPasswordAndPassphrase() throws {
        guard case .credential(let pw)? = PromptDialogModel(
            request: .credential(endpoint: endpoint, user: "u", kind: .password), sessionTitle: nil
        ) else { return XCTFail() }
        XCTAssertFalse(pw.isPassphrase)
        XCTAssertNil(pw.keyPath)
        XCTAssertTrue(pw.remember, "默认勾选保存到钥匙串")
        pw.setSecret("s3cret")
        XCTAssertEqual(pw.connectResponse, .credential(secret: "s3cret", remember: true))
        pw.remember = false
        XCTAssertEqual(pw.connectResponse, .credential(secret: "s3cret", remember: false))
        pw.clear()
        XCTAssertEqual(pw.connectResponse, .credential(secret: "", remember: false))
        XCTAssertEqual(pw.cancelResponse, .cancel)

        guard case .credential(let pp)? = PromptDialogModel(
            request: .credential(endpoint: endpoint, user: "u", kind: .keyPassphrase(path: "/k/id_ed25519")),
            sessionTitle: nil
        ) else { return XCTFail() }
        XCTAssertTrue(pp.isPassphrase)
        XCTAssertEqual(pp.keyPath, "/k/id_ed25519")
    }

    func testZmodemPromptsUseFilePanels() {
        guard case .transfer(.zmodemPickFiles)? = PromptDialogModel(request: .zmodemPickFiles, sessionTitle: nil)
        else { return XCTFail() }
        guard case .transfer(.zmodemSaveFile(let name, let size))? = PromptDialogModel(
            request: .zmodemSaveFile(suggestedName: "data.bin", size: 42), sessionTitle: nil)
        else { return XCTFail() }
        XCTAssertEqual(name, "data.bin")
        XCTAssertEqual(size, 42)
    }

    func testXmodemPromptsUseFilePanelsAndCancelSelection() {
        for request: FfiPromptRequest in [.xmodemPickFile, .xmodemSaveFile] {
            guard case .transfer(let actual)? = PromptDialogModel(request: request, sessionTitle: nil)
            else { return XCTFail() }
            XCTAssertEqual(actual, request)
            XCTAssertEqual(PromptDialogModel(request: request, sessionTitle: nil)?.cancelResponse, .cancel)
        }
    }
}

@MainActor
final class PromptPresenterTests: XCTestCase {
    private func make() -> (PromptPresenter, RecordingResponder, RecordingSheetHost) {
        let responder = RecordingResponder(), host = RecordingSheetHost()
        let presenter = PromptPresenter(responder: responder, host: host) { $0 == 1 ? "tab-one" : nil }
        return (presenter, responder, host)
    }

    /// 文件面板等待真实用户回答，沿用撤销和队列语义。
    func testTransferPromptWaitsForFileSelection() {
        let (presenter, responder, host) = make()
        presenter.enqueue(prompt: 7, session: nil, request: .zmodemPickFiles)
        XCTAssertTrue(responder.responses.isEmpty)
        XCTAssertEqual(host.presented.count, 1)
        XCTAssertEqual(presenter.currentPromptID, 7)
        host.complete(.files(paths: ["/temporary/a", "/temporary/b"]))
        XCTAssertEqual(responder.responses.first?.response, .files(paths: ["/temporary/a", "/temporary/b"]))
    }

    func testTwoPromptsShowOneAfterAnother() {
        let (p, responder, host) = make()
        p.enqueue(prompt: 10, session: 1, request: hostKeyRequest())
        p.enqueue(prompt: 11, session: nil, request: kbdRequest(questions: []))
        XCTAssertEqual(host.presented.count, 1, "同一时间只显示一个")
        XCTAssertEqual(p.currentPromptID, 10)
        XCTAssertEqual(p.queuedPromptIDs, [11])
        if case .hostKeyChanged(let m) = host.presented[0] { XCTAssertEqual(m.target.sessionTitle, "tab-one") }

        host.complete(.cancel)
        XCTAssertEqual(responder.responses.map(\.prompt), [10])
        XCTAssertEqual(host.presented.count, 2)
        XCTAssertEqual(p.currentPromptID, 11)

        host.complete(.answers(answers: []))
        XCTAssertEqual(responder.responses.map(\.prompt), [10, 11])
        XCTAssertNil(p.currentPromptID)
    }

    func testDismissCurrentClosesWithoutAnsweringAndShowsNext() {
        let (p, responder, host) = make()
        p.enqueue(prompt: 1, session: nil, request: hostKeyRequest())
        p.enqueue(prompt: 2, session: nil, request: hostKeyRequest())
        p.dismiss(prompt: 1)
        XCTAssertEqual(host.dismissCount, 1)
        XCTAssertTrue(responder.responses.isEmpty, "core 已撤销，不回答")
        XCTAssertEqual(p.currentPromptID, 2)
        XCTAssertEqual(host.presented.count, 2)
    }

    func testDismissQueuedRemovesIt() {
        let (p, responder, host) = make()
        p.enqueue(prompt: 1, session: nil, request: hostKeyRequest())
        p.enqueue(prompt: 2, session: nil, request: hostKeyRequest())
        p.dismiss(prompt: 2)
        XCTAssertEqual(p.queuedPromptIDs, [])
        XCTAssertEqual(host.dismissCount, 0)
        host.complete(.cancel)
        XCTAssertEqual(responder.responses.map(\.prompt), [1])
        XCTAssertEqual(host.presented.count, 1)
    }

    func testNoWindowAnswersCancel() {
        let (p, responder, host) = make()
        host.canPresent = false
        p.enqueue(prompt: 5, session: nil, request: hostKeyRequest())
        XCTAssertEqual(responder.responses.count, 1)
        XCTAssertEqual(responder.responses[0].prompt, 5)
        XCTAssertEqual(responder.responses[0].response, .cancel)
        XCTAssertTrue(host.presented.isEmpty)
        XCTAssertNil(p.currentPromptID)
    }

    func testControllerRoutesPromptEventsToPresenter() async throws {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let c = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true))
        defer { c.shutdown() }
        let responder = RecordingResponder(), host = RecordingSheetHost()
        c.promptPresenter = PromptPresenter(responder: responder, host: host) { _ in nil }

        dispatcher.onEvent(event: .prompt(prompt: 3, session: nil, request: hostKeyRequest()))
        dispatcher.onEvent(event: .promptDismissed(prompt: 3))
        let done = expectation(description: "事件处理完")
        DispatchQueue.main.async { done.fulfill() }
        await fulfillment(of: [done], timeout: 5)
        XCTAssertEqual(host.presented.count, 1)
        XCTAssertEqual(host.dismissCount, 1)
        XCTAssertNil(c.promptPresenter?.currentPromptID)
    }
}

@MainActor
final class LegacySshBadgeTests: XCTestCase {
    private func hop(_ host: String, _ port: UInt16, _ algos: [String]) -> FfiLegacyHop {
        FfiLegacyHop(endpoint: FfiEndpoint(host: host, port: port, via: []), algorithms: algos)
    }

    private func flush(_ dispatcher: CoreEventDispatcher) async {
        let done = XCTestExpectation(description: "事件处理完")
        DispatchQueue.main.async { done.fulfill() }
        await XCTWaiter().fulfillment(of: [done], timeout: 5)
    }

    func testToolTipHasOneLinePerServer() {
        let tip = LegacySshSummary.toolTip(hops: [
            hop("jump", 22, ["ssh-rsa", "hmac-sha1"]), hop("dev", 2200, ["diffie-hellman-group1-sha1"]), hop("clean", 22, []),
        ])
        let lines = tip?.components(separatedBy: "\n")
        XCTAssertEqual(lines?.count, 2)
        XCTAssertTrue(lines?[0].contains("jump:22") == true)
        XCTAssertTrue(lines?[0].contains("ssh-rsa, hmac-sha1") == true)
        XCTAssertTrue(lines?[1].contains("dev:2200") == true)
        XCTAssertNil(LegacySshSummary.toolTip(hops: []))
        XCTAssertNil(LegacySshSummary.toolTip(hops: [hop("a", 22, [])]))
    }

    func testBadgeFollowsActiveSessionAndClearsOnClose() async throws {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let c = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true))
        defer { c.shutdown() }
        let a = try c.openLocalShell()
        let b = try c.openLocalShell()
        XCTAssertEqual(c.workspace.activeSession, b)
        XCTAssertNil(c.activeLegacyToolTip)

        dispatcher.onEvent(event: .legacyAlgorithms(session: a, hops: [hop("old", 22, ["ssh-rsa"])]))
        await flush(dispatcher)
        XCTAssertNil(c.activeLegacyToolTip, "b 是活动会话，a 的旧算法不应显示")

        c.workspace.activate(a)
        XCTAssertTrue(c.activeLegacyToolTip?.contains("old:22") == true)
        c.workspace.activate(b)
        XCTAssertNil(c.activeLegacyToolTip)

        c.workspace.activate(a)
        _ = await c.requestClose([a])
        XCTAssertNil(c.legacyHops[a], "会话关闭后清除")
    }

    func testStatusBarShowsAndHidesBadge() {
        let bar = StatusBarView(frame: NSRect(x: 0, y: 0, width: 600, height: 24))
        XCTAssertFalse(bar.isLegacySshVisible)
        bar.setLegacySshToolTip("old:22: ssh-rsa")
        XCTAssertTrue(bar.isLegacySshVisible)
        XCTAssertEqual(bar.legacySshToolTip, "old:22: ssh-rsa")
        bar.setLegacySshToolTip(nil)
        XCTAssertFalse(bar.isLegacySshVisible)
    }
}
