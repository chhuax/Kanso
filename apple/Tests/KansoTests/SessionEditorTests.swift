import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

@MainActor
final class SessionEditorTests: XCTestCase {
    private var cores: [KansoCore] = []

    private func makeCore() -> KansoCore {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        cores.append(core)
        return core
    }

    override func tearDown() async throws {
        for c in cores { c.shutdown() }
        cores = []
    }

    private func newSSHModel(_ store: SessionStore, channel: String = "Direct") -> SessionEditorModel {
        let model = SessionEditorModel(store: store, channel: channel)
        model.name = "n"
        model.kind = .ssh
        model.host = " host.example "
        model.user = "me"
        return model
    }

    // MARK: 可见性

    func testFieldVisibilityByKindAndAuth() {
        let model = SessionEditorModel(store: MemoryStore(), channel: "Direct")
        XCTAssertTrue(model.isVisible(.shellCommand))
        XCTAssertTrue(model.isVisible(.encoding))
        XCTAssertFalse(model.isVisible(.host))

        model.kind = .ssh
        XCTAssertFalse(model.isVisible(.shellCommand))
        XCTAssertTrue(model.isVisible(.host) && model.isVisible(.jump) && model.isVisible(.proxyCommand))
        XCTAssertTrue(model.isVisible(.password))
        XCTAssertFalse(model.isVisible(.keyPath))
        model.auth = .privateKey
        XCTAssertTrue(model.isVisible(.keyPath) && model.isVisible(.keyPassphrase))
        XCTAssertFalse(model.isVisible(.password))
        model.auth = .agent
        XCTAssertFalse(model.isVisible(.password) || model.isVisible(.keyPath) || model.isVisible(.keyPassphrase))

        model.kind = .sftp
        XCTAssertFalse(model.isVisible(.encoding) || model.isVisible(.locale))
        XCTAssertTrue(model.isVisible(.host))
    }

    func testProxyCommandOnlyOnDirectChannel() {
        let mas = SessionEditorModel(store: MemoryStore(), channel: "MAS")
        mas.kind = .ssh
        XCTAssertFalse(mas.isVisible(.proxyCommand))
        mas.host = "h"; mas.user = "u"; mas.name = "n"
        mas.proxyCommand = "nc %h %p"
        XCTAssertNil(mas.assemble()?.sshTarget?.proxyCommand)
    }

    func testJumpAndProxyCommandAreMutuallyExclusive() {
        let model = newSSHModel(MemoryStore())
        XCTAssertTrue(model.isJumpEnabled && model.isProxyCommandEnabled)
        model.jumpID = "x"
        XCTAssertFalse(model.isProxyCommandEnabled)
        model.jumpID = nil
        model.proxyCommand = "  ssh -W %h:%p gw "
        XCTAssertFalse(model.isJumpEnabled)
        model.proxyCommand = "   "
        XCTAssertTrue(model.isJumpEnabled)
    }

    func testJumpCandidatesExcludeSelfAndNonSSH() throws {
        let core = makeCore()
        let web = try XCTUnwrap(core.profiles().first { $0.name == "web-01" })
        let model = SessionEditorModel(store: core, editing: web)
        let names = Set(model.jumpCandidates.map(\.name))
        XCTAssertEqual(names, ["legacy-switch", "mfa-bastion"])
    }

    // MARK: 端口与组装

    func testPortValidation() {
        let model = newSSHModel(MemoryStore())
        XCTAssertEqual(model.port, 22)
        for bad in ["", "0", "65536", "abc", "-1"] {
            model.portText = bad
            XCTAssertNil(model.port, bad)
            XCTAssertNil(model.assemble(), bad)
        }
        model.portText = "65535"
        XCTAssertEqual(model.port, 65535)
        XCTAssertEqual(SessionEditorModel.sanitizedPortText("2a2-2999999"), "22299")
    }

    func testAssembleSSHFields() throws {
        let model = newSSHModel(MemoryStore(), channel: "Direct")
        model.name = "  My Host "
        model.portText = "2222"
        model.auth = .privateKey
        model.keyPath = " ~/.ssh/id "
        model.jumpID = "jump-id"
        model.encoding = "gbk"
        model.locale = " en_US.UTF-8 "
        model.groupID = "g"
        model.color = SessionColorPresets.all[5]
        let profile = try XCTUnwrap(model.assemble())
        XCTAssertEqual(profile.name, "My Host")
        XCTAssertEqual(profile.group, "g")
        XCTAssertEqual(profile.color, SessionColorPresets.all[5])
        guard case .ssh(let ssh) = profile.kind else { return XCTFail("应为 SSH") }
        XCTAssertEqual(ssh.target.host, "host.example")
        XCTAssertEqual(ssh.target.port, 2222)
        XCTAssertEqual(ssh.target.user, "me")
        XCTAssertEqual(ssh.target.auth, .privateKey)
        XCTAssertEqual(ssh.target.privateKeyPath, "~/.ssh/id")
        XCTAssertEqual(ssh.target.jump, "jump-id")
        XCTAssertNil(ssh.target.proxyCommand)
        XCTAssertEqual(ssh.encoding, "gbk")
        XCTAssertEqual(ssh.locale, "en_US.UTF-8")
    }

    func testKeyPathDroppedWhenNotPrivateKeyAuth() throws {
        let model = newSSHModel(MemoryStore())
        model.keyPath = "/k"
        model.auth = .agent
        XCTAssertNil(try XCTUnwrap(model.assemble()).sshTarget?.privateKeyPath)
    }

    func testAssembleLocalAndSFTP() throws {
        let model = SessionEditorModel(store: MemoryStore())
        model.name = "L"
        model.shellCommand = " /bin/zsh "
        model.startDir = ""
        model.locale = ""
        guard case .local(let local) = try XCTUnwrap(model.assemble()).kind else { return XCTFail("应为本地") }
        XCTAssertEqual(local.shellCommand, "/bin/zsh")
        XCTAssertNil(local.startDir)
        XCTAssertNil(local.locale)

        model.kind = .sftp
        model.host = "h"; model.user = "u"
        guard case .sftp(let target) = try XCTUnwrap(model.assemble()).kind else { return XCTFail("应为 SFTP") }
        XCTAssertEqual(target.host, "h")
    }

    func testNewProfileIDIsLowercaseUUIDAndStable() throws {
        let model = SessionEditorModel(store: MemoryStore())
        XCTAssertEqual(model.profileID, model.profileID.lowercased())
        XCTAssertNotNil(UUID(uuidString: model.profileID))
        XCTAssertEqual(model.assemble()?.id, model.profileID)
    }

    func testEditingRoundTripKeepsProfile() throws {
        let core = makeCore()
        let web = try XCTUnwrap(core.profiles().first { $0.name == "web-01" })
        let model = SessionEditorModel(store: core, editing: web, channel: "Direct")
        XCTAssertEqual(model.assemble(), web)
        XCTAssertEqual(model.profileID, web.id)
    }

    func testUnknownEncodingAndCustomColorAreKept() {
        let model = SessionEditorModel(store: MemoryStore())
        model.encoding = "x-weird"
        XCTAssertTrue(model.encodingOptions.contains { $0.label == "x-weird" })
        XCTAssertEqual(SessionEditorModel(store: MemoryStore()).encodingOptions.first?.label, "")
        XCTAssertEqual(SessionEditorModel(store: MemoryStore()).colorOptions.count, 17)
        model.color = FfiRgb(r: 1, g: 2, b: 3)
        XCTAssertEqual(model.colorOptions.count, 18)
    }

    // MARK: 保存与凭据

    func testBlankPasswordDoesNotCallSetCredential() async {
        let store = RecordingStore(MemoryStore())
        let model = newSSHModel(store)
        let outcome = await model.save()
        XCTAssertEqual(outcome, .saved)
        XCTAssertTrue(store.credentialCalls.isEmpty)
        XCTAssertEqual(store.profiles().count, 1)
    }

    func testFilledPasswordCallsSetCredentialAfterSave() async {
        let store = RecordingStore(MemoryStore())
        let model = newSSHModel(store)
        model.password = "secret"
        let outcome = await model.save()
        XCTAssertEqual(outcome, .saved)
        XCTAssertEqual(store.credentialCalls, [CredentialCall(profile: model.profileID, kind: .password, secret: "secret")])
    }

    func testPassphraseOnlyForPrivateKeyAuth() async {
        let store = RecordingStore(MemoryStore())
        let model = newSSHModel(store)
        model.password = "p"
        model.keyPassphrase = "kp"
        model.auth = .privateKey
        model.keyPath = "/k"
        _ = await model.save()
        XCTAssertEqual(store.credentialCalls.map(\.kind), [.keyPassphrase])
        model.auth = .agent
        _ = await model.save()
        XCTAssertEqual(store.credentialCalls.count, 1)
    }

    func testSaveFailureShowsCoreMessageAndSkipsCredential() async throws {
        let store = RecordingStore(makeCore())
        let model = newSSHModel(store)
        model.host = ""
        model.password = "x"
        let expected: String
        do {
            try store.saveProfile(profile: try XCTUnwrap(model.assemble()))
            return XCTFail("core 应拒绝空主机")
        } catch {
            expected = CoreMessageText.text(for: error)
        }
        let outcome = await model.save()
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(model.errorText, expected)
        XCTAssertFalse(expected.isEmpty)
        XCTAssertTrue(store.credentialCalls.isEmpty)
    }

    func testJumpProxyConflictErrorComesFromCore() async {
        let store = RecordingStore(makeCore())
        let jump = store.profiles().first { $0.name == "mfa-bastion" }
        let model = newSSHModel(store)
        model.jumpID = jump?.id
        model.proxyCommand = "nc %h %p"
        let outcome = await model.save()
        XCTAssertEqual(outcome, .failed)
        XCTAssertNotNil(model.errorText)
        XCTAssertFalse(store.profiles().contains { $0.id == model.profileID })
    }

    func testInvalidPortAndEmptyNameAreRejectedLocally() async {
        let model = newSSHModel(MemoryStore())
        model.portText = "0"
        var outcome = await model.save()
        XCTAssertEqual(outcome, .failed)
        XCTAssertNotNil(model.errorText)
        model.portText = "22"
        model.name = "  "
        outcome = await model.save()
        XCTAssertEqual(outcome, .failed)
    }

    func testCredentialFailureKeepsSavedProfileAndRetryDoesNotDuplicate() async {
        struct Boom: Error {}
        let store = RecordingStore(MemoryStore())
        store.credentialError = Boom()
        let model = newSSHModel(store)
        model.password = "pw"
        var outcome = await model.save()
        XCTAssertEqual(outcome, .savedButCredentialFailed)
        XCTAssertEqual(store.profiles().count, 1)
        XCTAssertNotNil(model.errorText)
        store.credentialError = nil
        outcome = await model.save()
        XCTAssertEqual(outcome, .saved)
        XCTAssertEqual(store.profiles().count, 1)
        XCTAssertEqual(store.credentialCalls.count, 1)
    }
}
