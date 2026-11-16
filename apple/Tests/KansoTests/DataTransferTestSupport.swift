import AppKit
import KansoBridge
@testable import Kanso

/// 记录调用、返回预置结果的 `DataTransferCore` 替身：测试不读真实 `~/.ssh`、不写真实文件。
/// 测试只在主线程使用，所以可以安全地声明 `@unchecked Sendable`。
final class MockTransferCore: DataTransferCore, @unchecked Sendable {
    var groupList: [FfiGroup] = []
    var scanResult: Result<FfiSshConfigScan, Error> = .success(FfiSshConfigScan(candidates: [], warnings: []))
    var importSshResult: Result<FfiSshConfigImportReport, Error> = .success(
        FfiSshConfigImportReport(added: 0, updated: 0, jumpsAdded: 0, multiHopIgnored: 0, warnings: [])
    )
    var exportError: Error?
    var importDataResult: Result<FfiImportReport, Error> = .success(FfiImportReport(added: 0, updated: 0, skipped: [], converted: []))

    private(set) var scanPaths: [String?] = []
    private(set) var importSshCalls: [(path: String?, aliases: [String], group: String?)] = []
    private(set) var exportPaths: [String] = []
    private(set) var importDataPaths: [String] = []

    func groups() -> [FfiGroup] { groupList }

    func scanSshConfig(path: String?) async throws -> FfiSshConfigScan {
        scanPaths.append(path)
        return try scanResult.get()
    }

    func importSshConfig(path: String?, aliases: [String], group: String?) async throws -> FfiSshConfigImportReport {
        importSshCalls.append((path, aliases, group))
        return try importSshResult.get()
    }

    func exportData(path: String) async throws {
        exportPaths.append(path)
        if let exportError { throw exportError }
    }

    func importData(path: String) async throws -> FfiImportReport {
        importDataPaths.append(path)
        return try importDataResult.get()
    }
}

/// 不弹窗的文件面板替身：返回预置 URL（nil 表示用户取消）。
@MainActor
final class MockPanels: DataFilePanels {
    var sshConfigURL: URL?
    var exportURL: URL?
    var importURL: URL?
    private(set) var exportDefaultNames: [String] = []
    private(set) var sshStartDirectories: [URL] = []

    func chooseSshConfig(startingAt directory: URL, window: NSWindow?) async -> URL? {
        sshStartDirectories.append(directory)
        return sshConfigURL
    }

    func chooseExportDestination(defaultName: String, window: NSWindow?) async -> URL? {
        exportDefaultNames.append(defaultName)
        return exportURL
    }

    func chooseImportSource(window: NSWindow?) async -> URL? { importURL }
}

enum TransferFixtures {
    static func message(_ key: String, _ args: [String: String] = [:]) -> FfiMessage {
        FfiMessage(key: key, args: args.sorted { $0.key < $1.key }.map { FfiMessageArg(name: $0.key, value: .str(value: $0.value)) })
    }

    static func candidate(
        _ alias: String, host: String? = nil, port: UInt16 = 22, user: String = "alice", identity: String? = nil,
        jump: String? = nil, existing: String? = nil, warnings: [FfiMessage] = []
    ) -> FfiSshConfigCandidate {
        FfiSshConfigCandidate(alias: alias, host: host ?? "\(alias).example.com", port: port, user: user,
                              identityFile: identity, proxyJump: jump, existing: existing, warnings: warnings)
    }

    /// 4 个候选：web（带私钥）、db（已保存、有跳板）、legacy（带警告）、bastion（最简）。
    static var scan: FfiSshConfigScan {
        FfiSshConfigScan(
            candidates: [
                candidate("web", identity: "~/.ssh/id_web"),
                candidate("db", port: 2222, user: "deploy", jump: "bastion", existing: "p-db"),
                candidate("legacy", warnings: [message("sshconfig-invalid-port", ["alias": "legacy", "value": "abc"])]),
                candidate("bastion", user: "ops"),
            ],
            warnings: [message("sshconfig-match-unsupported", ["file": "~/.ssh/config"])]
        )
    }

    static func readFailure(_ path: String) -> Error {
        FfiCoreError.User(message: message("import-read-failed", ["path": path, "reason": "No such file"]))
    }
}
