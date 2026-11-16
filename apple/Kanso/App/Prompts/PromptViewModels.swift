import Foundation
import KansoBridge

/// 对话框标题区所需的"是谁在问"信息：目标主机、跳板链、关联的标签标题。
///
/// 每个提问对话框都必须带上它：同时连着多台主机时，用户必须知道这个问题来自哪里，
/// 否则容易把密码 / 验证码填给错误的主机。
struct PromptTarget: Equatable {
    /// `host:port`。
    var hostPort: String
    /// 从外到内的跳板机显示名；空表示直连。
    var via: [String]
    /// 关联会话的标签标题；提问不关联会话时为 nil。
    var sessionTitle: String?

    init(endpoint: FfiEndpoint, sessionTitle: String?) {
        hostPort = "\(endpoint.host):\(endpoint.port)"
        via = endpoint.via
        self.sessionTitle = sessionTitle
    }

    /// "经由 X"文字；直连为 nil。多跳用 " → " 连接，顺序与连接顺序一致（外到内）。
    var viaText: String? {
        guard !via.isEmpty else { return nil }
        return String(localized: "via \(via.joined(separator: " → "))")
    }
}

/// 按钮的语义：决定对话框里的默认按钮（回车触发）。
enum PromptButtonRole: Equatable {
    case primary
    case cancel
}

/// 主机密钥已变更。
///
/// 默认按钮是"取消"：回车不应接受一把变了的密钥（可能是中间人攻击），接受必须是显式点击。
struct HostKeyPromptModel: Equatable {
    var target: PromptTarget
    var keyType: String
    var fingerprint: String
    /// "文件:行号"，行号与编辑器看到的一致。
    var knownHostsLocation: String

    init(endpoint: FfiEndpoint, keyType: String, fingerprint: String, knownHostsPath: String, line: UInt32, sessionTitle: String?) {
        target = PromptTarget(endpoint: endpoint, sessionTitle: sessionTitle)
        self.keyType = keyType
        self.fingerprint = fingerprint
        knownHostsLocation = "\(knownHostsPath):\(line)"
    }

    var acceptTitle: String { String(localized: "Accept New Key and Connect") }
    var cancelTitle: String { String(localized: "Cancel") }
    /// 安全起见固定为取消。
    var defaultButton: PromptButtonRole { .cancel }

    var acceptResponse: FfiPromptResponse { .acceptHostKey }
    var cancelResponse: FfiPromptResponse { .cancel }
}

/// keyboard-interactive 一轮问答。
///
/// 服务器给的 name / instruction / 问题文字是原文，不本地化。输入内容放在本对象里，
/// 回答后由 `clear()` 清空，缩短明文在内存里的生命周期。
final class KeyboardInteractivePromptModel {
    struct Field: Equatable {
        /// 服务器原文问题。
        var prompt: String
        /// `echo == false` 时用密码框。
        var isSecure: Bool
    }

    let target: PromptTarget
    let user: String
    /// 空串按"没有"处理，不显示。
    let name: String?
    let instruction: String?
    /// 前面无问题的轮次带过来的文字（如"推送已发送"），逐行显示。
    let preamble: [String]
    let fields: [Field]
    private(set) var answers: [String]

    init(endpoint: FfiEndpoint, user: String, name: String, instruction: String, preamble: [String],
         questions: [FfiKbdQuestion], sessionTitle: String?) {
        target = PromptTarget(endpoint: endpoint, sessionTitle: sessionTitle)
        self.user = user
        self.name = name.isEmpty ? nil : name
        self.instruction = instruction.isEmpty ? nil : instruction
        self.preamble = preamble
        fields = questions.map { Field(prompt: $0.text, isSecure: !$0.echo) }
        answers = Array(repeating: "", count: questions.count)
    }

    var continueTitle: String { String(localized: "Continue") }
    var cancelTitle: String { String(localized: "Cancel") }
    var defaultButton: PromptButtonRole { .primary }

    func setAnswer(_ text: String, at index: Int) {
        guard answers.indices.contains(index) else { return }
        answers[index] = text
    }

    /// 答案与问题一一对应、顺序一致。
    var continueResponse: FfiPromptResponse { .answers(answers: answers) }
    var cancelResponse: FfiPromptResponse { .cancel }

    func clear() { answers = Array(repeating: "", count: answers.count) }
}

/// 缺少凭据（密码 / 私钥口令）。
final class CredentialPromptModel {
    let target: PromptTarget
    let user: String
    /// 口令时为私钥路径；密码为 nil。
    let keyPath: String?
    let isPassphrase: Bool
    var remember = true
    private(set) var secret = ""

    init(endpoint: FfiEndpoint, user: String, kind: FfiCredentialKind, sessionTitle: String?) {
        target = PromptTarget(endpoint: endpoint, sessionTitle: sessionTitle)
        self.user = user
        switch kind {
        case .password:
            keyPath = nil
            isPassphrase = false
        case .keyPassphrase(let path):
            keyPath = path
            isPassphrase = true
        }
    }

    var fieldLabel: String { isPassphrase ? String(localized: "Key passphrase") : String(localized: "Password") }
    var rememberTitle: String { String(localized: "Save to Keychain") }
    var connectTitle: String { String(localized: "Connect") }
    var cancelTitle: String { String(localized: "Cancel") }
    var defaultButton: PromptButtonRole { .primary }

    func setSecret(_ text: String) { secret = text }

    var connectResponse: FfiPromptResponse { .credential(secret: secret, remember: remember) }
    var cancelResponse: FfiPromptResponse { .cancel }

    func clear() { secret = "" }
}

/// 一个待显示的提问对话框。
enum PromptDialogModel {
    case hostKeyChanged(HostKeyPromptModel)
    case keyboardInteractive(KeyboardInteractivePromptModel)
    case credential(CredentialPromptModel)
    case transfer(FfiPromptRequest)

    /// 由 core 的提问构造，传输提问交给系统文件面板。
    init?(request: FfiPromptRequest, sessionTitle: String?) {
        switch request {
        case let .hostKeyChanged(endpoint, keyType, fingerprint, knownHostsPath, line):
            self = .hostKeyChanged(HostKeyPromptModel(
                endpoint: endpoint, keyType: keyType, fingerprint: fingerprint,
                knownHostsPath: knownHostsPath, line: line, sessionTitle: sessionTitle))
        case let .keyboardInteractive(endpoint, user, name, instruction, preamble, questions):
            self = .keyboardInteractive(KeyboardInteractivePromptModel(
                endpoint: endpoint, user: user, name: name, instruction: instruction,
                preamble: preamble, questions: questions, sessionTitle: sessionTitle))
        case let .credential(endpoint, user, kind):
            self = .credential(CredentialPromptModel(endpoint: endpoint, user: user, kind: kind, sessionTitle: sessionTitle))
        case .zmodemSaveFile, .zmodemPickFiles, .xmodemPickFile, .xmodemSaveFile:
            self = .transfer(request)
        }
    }

    /// 取消时的应答（窗口不存在等无法展示的场景也用它）。
    var cancelResponse: FfiPromptResponse { .cancel }

    /// 清空输入的明文。
    func clear() {
        switch self {
        case .hostKeyChanged, .transfer: break
        case .keyboardInteractive(let model): model.clear()
        case .credential(let model): model.clear()
        }
    }
}

/// 状态栏 Legacy SSH 标记的悬停文字（ssh.md §2.2）。纯函数，便于单测。
enum LegacySshSummary {
    /// 每台服务器（含跳板机）一行：`host:port：算法1, 算法2`。
    /// 没有任何旧算法的跳被略过；全部略过返回 nil，表示不显示标记。
    static func toolTip(hops: [FfiLegacyHop]) -> String? {
        let lines = hops.filter { !$0.algorithms.isEmpty }.map { hop in
            let server = "\(hop.endpoint.host):\(hop.endpoint.port)"
            return String(localized: "\(server): \(hop.algorithms.joined(separator: ", "))")
        }
        return lines.isEmpty ? nil : lines.joined(separator: "\n")
    }
}
