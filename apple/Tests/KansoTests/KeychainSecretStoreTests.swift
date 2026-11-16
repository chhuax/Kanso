import KansoBridge
@testable import PlatformMac
import Security
import XCTest

/// 测试用内存凭据存储，供需要构造真实 core 的测试使用，避免触碰用户真实的钥匙串。
final class InMemorySecretStore: FfiSecretStore, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String: String] = [:]

    func get(account: String) throws -> String? {
        lock.withLock { items[account] }
    }

    func set(account: String, secret: String) throws {
        lock.withLock { items[account] = secret }
    }

    func delete(account: String) throws {
        _ = lock.withLock { items.removeValue(forKey: account) }
    }
}

/// 记录调用并按脚本返回状态码的假钥匙串。
private final class FakeKeychainClient: KeychainClient, @unchecked Sendable {
    var copyResult: (OSStatus, Data?) = (errSecItemNotFound, nil)
    var updateStatuses: [OSStatus] = [errSecSuccess]
    var addStatus: OSStatus = errSecSuccess
    var deleteStatus: OSStatus = errSecSuccess

    var copyQueries: [[String: Any]] = []
    var updateCalls: [(query: [String: Any], attrs: [String: Any])] = []
    var addCalls: [[String: Any]] = []
    var deleteQueries: [[String: Any]] = []

    func copyMatching(_ query: [String: Any]) -> (OSStatus, Data?) {
        copyQueries.append(query)
        return copyResult
    }

    func add(_ attributes: [String: Any]) -> OSStatus {
        addCalls.append(attributes)
        return addStatus
    }

    func update(_ query: [String: Any], _ attributes: [String: Any]) -> OSStatus {
        updateCalls.append((query, attributes))
        return updateStatuses.count > 1 ? updateStatuses.removeFirst() : updateStatuses[0]
    }

    func delete(_ query: [String: Any]) -> OSStatus {
        deleteQueries.append(query)
        return deleteStatus
    }
}

final class KeychainSecretStoreTests: XCTestCase {
    private let account = "profile/0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b/password"

    private func make(_ fake: FakeKeychainClient) -> KeychainSecretStore {
        KeychainSecretStore(service: "test.service", client: fake)
    }

    private func assertBaseQuery(_ q: [String: Any], file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(q[kSecClass as String] as? String, kSecClassGenericPassword as String, file: file, line: line)
        XCTAssertEqual(q[kSecAttrService as String] as? String, "test.service", file: file, line: line)
        XCTAssertEqual(q[kSecAttrAccount as String] as? String, account, file: file, line: line)
        // 不使用数据保护钥匙串。
        XCTAssertNil(q[kSecUseDataProtectionKeychain as String], file: file, line: line)
    }

    func testGetNotFoundReturnsNil() throws {
        let fake = FakeKeychainClient()
        XCTAssertNil(try make(fake).get(account: account))
        assertBaseQuery(fake.copyQueries[0])
        XCTAssertEqual(fake.copyQueries[0][kSecReturnData as String] as? Bool, true)
    }

    func testGetReturnsUTF8String() throws {
        let fake = FakeKeychainClient()
        fake.copyResult = (errSecSuccess, Data("密码p@ss".utf8))
        XCTAssertEqual(try make(fake).get(account: account), "密码p@ss")
    }

    func testGetInvalidUTF8IsOther() {
        let fake = FakeKeychainClient()
        fake.copyResult = (errSecSuccess, Data([0xff, 0xfe]))
        XCTAssertThrowsError(try make(fake).get(account: account)) { error in
            guard case FfiSecretError.Other = error else { return XCTFail("\(error)") }
        }
    }

    func testSetUpdatesExistingWithoutAdd() throws {
        let fake = FakeKeychainClient()
        try make(fake).set(account: account, secret: "s3cret")
        XCTAssertEqual(fake.updateCalls.count, 1)
        XCTAssertTrue(fake.addCalls.isEmpty)
        assertBaseQuery(fake.updateCalls[0].query)
        XCTAssertEqual(fake.updateCalls[0].attrs[kSecValueData as String] as? Data, Data("s3cret".utf8))
    }

    func testSetFallsBackToAddWhenNotFound() throws {
        let fake = FakeKeychainClient()
        fake.updateStatuses = [errSecItemNotFound]
        try make(fake).set(account: account, secret: "s3cret")
        XCTAssertEqual(fake.addCalls.count, 1)
        let add = fake.addCalls[0]
        assertBaseQuery(add)
        XCTAssertEqual(add[kSecValueData as String] as? Data, Data("s3cret".utf8))
        XCTAssertEqual(
            add[kSecAttrAccessible as String] as? String,
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String
        )
        // 不能出现“先删再加”。
        XCTAssertTrue(fake.deleteQueries.isEmpty)
    }

    func testSetRetriesUpdateOnDuplicateRace() throws {
        let fake = FakeKeychainClient()
        fake.updateStatuses = [errSecItemNotFound, errSecSuccess]
        fake.addStatus = errSecDuplicateItem
        try make(fake).set(account: account, secret: "x")
        XCTAssertEqual(fake.updateCalls.count, 2)
    }

    func testSetErrorIsMapped() {
        let fake = FakeKeychainClient()
        fake.updateStatuses = [errSecAuthFailed]
        XCTAssertThrowsError(try make(fake).set(account: account, secret: "x")) { error in
            XCTAssertEqual(error as? FfiSecretError, .AccessDenied)
        }
        XCTAssertTrue(fake.addCalls.isEmpty)
    }

    func testDeleteNotFoundIsSuccess() throws {
        let fake = FakeKeychainClient()
        fake.deleteStatus = errSecItemNotFound
        try make(fake).delete(account: account)
        assertBaseQuery(fake.deleteQueries[0])
    }

    func testDeleteErrorIsMapped() {
        let fake = FakeKeychainClient()
        fake.deleteStatus = errSecInteractionNotAllowed
        XCTAssertThrowsError(try make(fake).delete(account: account)) { error in
            guard case FfiSecretError.Unavailable = error else { return XCTFail("\(error)") }
        }
    }

    func testStatusMapping() {
        let store = make(FakeKeychainClient())
        XCTAssertEqual(store.map("get", errSecUserCanceled), .AccessDenied)
        XCTAssertEqual(store.map("get", errSecAuthFailed), .AccessDenied)
        for status in [errSecInteractionNotAllowed, errSecNotAvailable] {
            guard case .Unavailable(let detail) = store.map("get", status) else { return XCTFail() }
            XCTAssertTrue(detail.contains("OSStatus \(status)"))
        }
        guard case .Other(let detail) = store.map("set", errSecParam) else { return XCTFail() }
        XCTAssertTrue(detail.contains("OSStatus \(errSecParam)"))
        XCTAssertTrue(detail.contains("set"))
    }

    /// 真实钥匙串集成测试：默认跳过。用随机 service，结束时清理。
    func testRealKeychainRoundTrip() throws {
        try XCTSkipUnless(
            ProcessInfo.processInfo.environment["KANSO_KEYCHAIN_TESTS"] == "1",
            "设置 KANSO_KEYCHAIN_TESTS=1 才会访问真实钥匙串"
        )
        let store = KeychainSecretStore(service: "kanso.test.\(UUID().uuidString)")
        let account = self.account
        // 闭包不捕获 self（Swift 6 下 addTeardownBlock 要求 Sendable）。
        addTeardownBlock { try? store.delete(account: account) }

        XCTAssertNil(try store.get(account: account))
        try store.set(account: account, secret: "one")
        XCTAssertEqual(try store.get(account: account), "one")
        try store.set(account: account, secret: "two")
        XCTAssertEqual(try store.get(account: account), "two")
        try store.delete(account: account)
        XCTAssertNil(try store.get(account: account))
        try store.delete(account: account)
    }
}
