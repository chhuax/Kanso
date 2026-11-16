import Foundation
import KansoBridge
import Security

/// 基于 macOS 钥匙串的凭据存储，实现 Rust core 的 `FfiSecretStore` 回调接口。
///
/// - 线程：core 在 `spawn_blocking` 工作线程上调用，**不是主线程**，所以不能标 `@MainActor`；
///   类本身不可变（只有 `let`），`Sendable` 由成员的 `Sendable` 保证，无需锁。
/// - 阻塞：钥匙串访问可能弹授权框并等待用户，这在 core 的契约里是允许的。
/// - `account` 是 Rust 生成的不透明键（`profile/<uuid>/password` 等），这里不解析；
///   `service` 由 App 传入（bundle id），用于隔离不同 App 的条目。
/// - 明文不进日志：本类不打印任何东西，`detail` 里只有状态码与系统错误描述。
///
/// 钥匙串类型的选择（重要，不要随手改）：
/// - **不使用** `kSecUseDataProtectionKeychain`。数据保护钥匙串要求 App 带 keychain-access-groups
///   权限与 Team ID，而目前本地构建是 ad-hoc 签名，用不了。
/// - 代价：旧版（文件）钥匙串的访问控制绑定到代码签名；ad-hoc 签名每次重新构建都会变，
///   所以每次更新后系统会弹一次“允许访问”提示（store-import.md §2 提到的问题）。
///   等 T12 有了固定签名身份后再评估切换到数据保护钥匙串。
public final class KeychainSecretStore: FfiSecretStore {
    private let service: String
    private let client: any KeychainClient

    /// - Parameters:
    ///   - service: `kSecAttrService`，App 传 bundle id。
    ///   - client: 钥匙串访问；生产用默认的 `SystemKeychainClient`，测试注入假实现。
    public init(service: String, client: any KeychainClient = SystemKeychainClient()) {
        self.service = service
        self.client = client
    }

    // MARK: - FfiSecretStore

    public func get(account: String) throws -> String? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        let (status, data) = client.copyMatching(query)
        switch status {
        case errSecSuccess:
            guard let data else {
                throw FfiSecretError.Other(detail: describe("get", errSecInternalError))
            }
            // 非法 UTF-8 说明条目不是我们写的（或已损坏）；不能静默丢字节。
            guard let text = String(data: data, encoding: .utf8) else {
                throw FfiSecretError.Other(detail: "keychain item is not valid UTF-8")
            }
            return text
        case errSecItemNotFound:
            return nil
        default:
            throw map("get", status)
        }
    }

    public func set(account: String, secret: String) throws {
        let data = Data(secret.utf8)
        let query = baseQuery(account)
        // 先 update、找不到再 add，而不是“先删再加”：删除之后 add 失败（授权被拒等）会丢掉旧凭据。
        // update 同时刷新 kSecAttrAccessible，使旧条目也收敛到同一策略。
        let attrs: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: Self.accessible,
        ]
        var status = client.update(query, attrs)
        if status == errSecItemNotFound {
            status = client.add(query.merging(attrs) { _, new in new })
            if status == errSecDuplicateItem {
                // 与另一线程的 set 竞争：对方刚好在我们 update 与 add 之间创建了条目，再 update 一次。
                status = client.update(query, attrs)
            }
        }
        guard status == errSecSuccess else { throw map("set", status) }
    }

    public func delete(account: String) throws {
        let status = client.delete(baseQuery(account))
        // 契约：条目不存在视为成功。
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw map("delete", status)
        }
    }

    // MARK: - 内部

    /// 不随 iCloud 钥匙串同步（`ThisDeviceOnly`）：凭据与本机绑定，避免把服务器密码同步到其他设备；
    /// 首次解锁后即可读，这样登录后用户锁屏期间后台重连不会因为钥匙串锁定而失败。
    // 用计算属性而非 static let：CFString 不是 Sendable，Swift 6 下全局存储属性会报错。
    static var accessible: CFString { kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly }

    /// 定位条目的查询字典。**不要**加 `kSecAttrSynchronizable`：缺省值只匹配本地条目，正是我们要的。
    func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    /// 状态码到 `FfiSecretError` 的映射：
    /// - 用户取消 / 认证失败 → `AccessDenied`：是用户或系统明确拒绝，重试没有意义，core 据此提示用户；
    /// - 不允许交互（锁屏、无 GUI 会话）/ 钥匙串服务不可用 → `Unavailable`：是环境暂时不可用，
    ///   稍后可能恢复，和“被拒绝”区分开；
    /// - 其余 → `Other`，带英文描述与数字码，便于排查。
    func map(_ op: String, _ status: OSStatus) -> FfiSecretError {
        switch status {
        case errSecUserCanceled, errSecAuthFailed:
            return .AccessDenied
        case errSecInteractionNotAllowed, errSecNotAvailable:
            return .Unavailable(detail: describe(op, status))
        default:
            return .Other(detail: describe(op, status))
        }
    }

    /// `SecCopyErrorMessageString` 的描述随系统语言本地化，这里原样附上并始终带数字码，
    /// 以数字码为准排查。只包含状态信息，绝不含条目内容。
    private func describe(_ op: String, _ status: OSStatus) -> String {
        let message = SecCopyErrorMessageString(status, nil) as String? ?? "unknown error"
        return "keychain \(op) failed: \(message) (OSStatus \(status))"
    }
}
