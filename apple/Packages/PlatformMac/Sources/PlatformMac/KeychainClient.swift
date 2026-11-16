import Foundation
import Security

/// 对 Security 框架 `SecItem*` 四个函数的最小封装，让 `KeychainSecretStore` 可以注入假实现测试。
///
/// 实现必须线程安全：`KeychainSecretStore` 会在 Rust 工作线程上调用它，可能并发，也可能阻塞
/// （钥匙串授权框）。所有方法只返回 `OSStatus`，不抛错，错误的解释交给调用方。
public protocol KeychainClient: Sendable {
    /// `SecItemCopyMatching`。`query` 需自带 `kSecReturnData` 等返回选项；成功且有数据时返回 `Data`。
    func copyMatching(_ query: [String: Any]) -> (OSStatus, Data?)
    /// `SecItemAdd`（不要结果）。
    func add(_ attributes: [String: Any]) -> OSStatus
    /// `SecItemUpdate`。
    func update(_ query: [String: Any], _ attributes: [String: Any]) -> OSStatus
    /// `SecItemDelete`。
    func delete(_ query: [String: Any]) -> OSStatus
}

/// 生产实现：直接调用系统钥匙串。无状态，天然线程安全。
public struct SystemKeychainClient: KeychainClient {
    /// 创建实例。
    public init() {}

    public func copyMatching(_ query: [String: Any]) -> (OSStatus, Data?) {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        return (status, result as? Data)
    }

    public func add(_ attributes: [String: Any]) -> OSStatus {
        SecItemAdd(attributes as CFDictionary, nil)
    }

    public func update(_ query: [String: Any], _ attributes: [String: Any]) -> OSStatus {
        SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
    }

    public func delete(_ query: [String: Any]) -> OSStatus {
        SecItemDelete(query as CFDictionary)
    }
}
