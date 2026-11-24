import Foundation
import KansoBridge
@testable import Kanso

struct CredentialCall: Equatable {
    let profile: String
    let kind: FfiSecretKind
    let secret: String
}

/// 包装另一个 `SessionStore`，记录 `setCredential` 调用且不真正写入（测试不碰钥匙串）。
/// 测试只在主线程使用，所以可以安全地声明 `@unchecked Sendable`。
final class RecordingStore: SessionStore, @unchecked Sendable {
    let inner: SessionStore
    private(set) var credentialCalls: [CredentialCall] = []
    var credentialError: Error?

    init(_ inner: SessionStore) { self.inner = inner }

    func profiles() -> [FfiProfile] { inner.profiles() }
    func groups() -> [FfiGroup] { inner.groups() }
    func saveProfile(profile: FfiProfile) throws { try inner.saveProfile(profile: profile) }
    func deleteProfile(id: String) async throws { try await inner.deleteProfile(id: id) }
    func saveGroup(group: FfiGroup) throws { try inner.saveGroup(group: group) }
    func groupDeleteImpact(id: String) throws -> FfiDeleteImpact { try inner.groupDeleteImpact(id: id) }
    func deleteGroup(id: String) async throws { try await inner.deleteGroup(id: id) }

    func setCredential(profile: String, kind: FfiSecretKind, secret: String) async throws {
        if let credentialError { throw credentialError }
        credentialCalls.append(CredentialCall(profile: profile, kind: kind, secret: secret))
    }
}

/// 纯内存的 `SessionStore`：数据完全由测试决定（mock core 的种子数据顺序与名称不便于断言排序）。
final class MemoryStore: SessionStore, @unchecked Sendable {
    var storedProfiles: [FfiProfile]
    var storedGroups: [FfiGroup]
    private(set) var profileReads = 0
    private(set) var groupReads = 0
    var impact = FfiDeleteImpact(profiles: 0, snippets: 0)

    init(profiles: [FfiProfile] = [], groups: [FfiGroup] = []) {
        storedProfiles = profiles
        storedGroups = groups
    }

    func profiles() -> [FfiProfile] { profileReads += 1; return storedProfiles }
    func groups() -> [FfiGroup] { groupReads += 1; return storedGroups }

    func saveProfile(profile: FfiProfile) throws {
        if let i = storedProfiles.firstIndex(where: { $0.id == profile.id }) {
            storedProfiles[i] = profile
        } else {
            storedProfiles.append(profile)
        }
    }

    func deleteProfile(id: String) async throws { storedProfiles.removeAll { $0.id == id } }

    func saveGroup(group: FfiGroup) throws {
        if let i = storedGroups.firstIndex(where: { $0.id == group.id }) {
            storedGroups[i] = group
        } else {
            storedGroups.append(group)
        }
    }

    func groupDeleteImpact(id: String) throws -> FfiDeleteImpact { impact }
    func deleteGroup(id: String) async throws {
        storedProfiles.removeAll { $0.group == id }
        storedGroups.removeAll { $0.id == id }
    }

    func setCredential(profile: String, kind: FfiSecretKind, secret: String) async throws {}
}

extension FfiKeepaliveConfig {
    /// 测试夹具用的默认值，与 core 的 `KeepaliveConfig::default` 一致。
    static let defaultKeepalive = FfiKeepaliveConfig(
        enabled: true, intervalSecs: 30, maxMisses: 3,
        idleEnabled: false, idleIntervalSecs: 3600, idleString: "\\r"
    )
}

enum SessionFixtures {
    static func target(host: String = "h", user: String = "u", port: UInt16 = 22) -> FfiSshTarget {
        FfiSshTarget(host: host, port: port, user: user, auth: .password, privateKeyPath: nil, jump: nil, proxyCommand: nil,
                     keepalive: .defaultKeepalive)
    }

    static func ssh(_ id: String, _ name: String, host: String = "h", user: String = "u", group: String? = nil) -> FfiProfile {
        FfiProfile(id: id, name: name, color: nil, group: group,
                   kind: .ssh(profile: FfiSshProfile(target: target(host: host, user: user), encoding: "", locale: nil)))
    }

    static func local(_ id: String, _ name: String, shell: String = "", group: String? = nil) -> FfiProfile {
        FfiProfile(id: id, name: name, color: nil, group: group,
                   kind: .local(profile: FfiLocalProfile(shellCommand: shell, startDir: nil, encoding: "", locale: nil)))
    }

    static func sftp(_ id: String, _ name: String) -> FfiProfile {
        FfiProfile(id: id, name: name, color: nil, group: nil, kind: .sftp(target: target()))
    }
}
