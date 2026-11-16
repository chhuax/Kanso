import Foundation
import KansoBridge

extension Notification.Name {
    static let sessionLibraryDidChange = Notification.Name("Kanso.sessionLibraryDidChange")
}

extension SessionStore {
    func notifyLibraryChanged() {
        NotificationCenter.default.post(name: .sessionLibraryDidChange, object: self)
    }
}

/// 会话启动器与编辑对话框用到的 core 能力。
///
/// 抽成协议只为两件事：测试可以用记录调用的包装（例如断言"留空密码不调用 `setCredential`"）
/// 而不必碰真实钥匙串；视图模型不依赖整个 `KansoCore`。生产环境直接传 `KansoCore`。
/// 要求 `Sendable`：模型在主线程调用其 async 方法，值要能跨隔离域传递（`KansoCore` 本身就是 Sendable）。
protocol SessionStore: AnyObject, Sendable {
    func profiles() -> [FfiProfile]
    func groups() -> [FfiGroup]
    func saveProfile(profile: FfiProfile) throws
    func deleteProfile(id: String) async throws
    func saveGroup(group: FfiGroup) throws
    func groupDeleteImpact(id: String) throws -> FfiDeleteImpact
    func deleteGroup(id: String) async throws
    func setCredential(profile: String, kind: FfiSecretKind, secret: String) async throws
}

extension KansoCore: SessionStore {}

/// 会话类型（编辑对话框里的"类型"）。
enum SessionKindChoice: CaseIterable, Equatable {
    case local, ssh, sftp

    init(_ kind: FfiProfileKind) {
        switch kind {
        case .local: self = .local
        case .ssh: self = .ssh
        case .sftp: self = .sftp
        }
    }

    var title: String {
        switch self {
        case .local: String(localized: "Local")
        case .ssh: String(localized: "SSH")
        case .sftp: String(localized: "SFTP")
        }
    }

    /// 启动器行与菜单里的 SF Symbol 名。
    var symbol: String {
        switch self {
        case .local: "terminal"
        case .ssh: "server.rack"
        case .sftp: "folder"
        }
    }
}

extension FfiProfile {
    var kindChoice: SessionKindChoice { SessionKindChoice(kind) }

    /// SSH / SFTP 的连接目标；本地会话为 nil。
    var sshTarget: FfiSshTarget? {
        switch kind {
        case .local: nil
        case .ssh(let profile): profile.target
        case .sftp(let target): target
        }
    }
}
