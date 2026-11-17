import Foundation
import Security

/// MCP 客户端令牌（spec/mcp.md §1.1）。只进钥匙串，不进数据库、UserDefaults 或日志。
///
/// `account` 固定为 `mcp/client-token`，与会话凭据的 `profile/<uuid>/…` 分开，避免撞名。
public enum McpClientToken {
    public static let account = "mcp/client-token"

    /// 22 个均匀分布的字母数字。每个字符 log2(62) ≈ 5.95 位，合计约 131 位，满足「至少 128 位」。
    public static let length = 22

    /// 生成新令牌。用拒绝采样去掉取模偏差：62 × 4 = 248，大于等于 248 的字节丢弃。
    public static func generate() -> String {
        let alphabet = Array("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
        precondition(alphabet.count == 62)
        var chars: [Character] = []
        chars.reserveCapacity(length)
        while chars.count < length {
            var byte: UInt8 = 0
            let status = SecRandomCopyBytes(kSecRandomDefault, 1, &byte)
            precondition(status == errSecSuccess, "SecRandomCopyBytes 失败")
            if byte < 248 {
                chars.append(alphabet[Int(byte) % alphabet.count])
            }
        }
        return String(chars)
    }

    /// 读取已保存的令牌。条目不存在时返回 nil，其他钥匙串错误抛出。
    /// 钥匙串访问可能阻塞在系统授权框上，不要在主线程调用。
    public static func load(from store: KeychainSecretStore) throws -> String? {
        try store.get(account: account)
    }

    /// 保存令牌（已存在则覆盖），钥匙串出错时抛出。可能阻塞在系统授权框上，不要在主线程调用。
    public static func save(_ token: String, to store: KeychainSecretStore) throws {
        try store.set(account: account, secret: token)
    }
}
