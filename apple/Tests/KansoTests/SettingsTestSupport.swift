import KansoBridge
@testable import Kanso

/// 内存设置后端：与 core 设置表同语义（键 → JSON 文本），并记录下发的终端选项。
/// 设置逻辑测试用它，不碰真实存储；需要真实 core 行为时用 `KansoCore.newMock`。
final class MemorySettingsBackend: SettingsBackend, @unchecked Sendable {
    var values: [String: String] = [:]
    private(set) var terminalOptionsLog: [FfiTerminalOptions] = []
    private(set) var resetCount = 0
    /// 置位后写入抛错，用来测试"写失败不影响内存状态"。
    var failWrites = false

    struct WriteError: Error {}

    func setting(key: String) -> String? { values[key] }

    func setSetting(key: String, json: String) throws {
        if failWrites { throw WriteError() }
        values[key] = json
    }

    func resetSettings() throws {
        resetCount += 1
        values.removeAll()
    }

    func setTerminalOptions(options: FfiTerminalOptions) { terminalOptionsLog.append(options) }
}

extension SettingsStore {
    /// 新建一个已连接到 `backend` 的存储（等价于"应用重启后重新载入"）。
    @MainActor
    static func attached(to backend: SettingsBackend) -> SettingsStore {
        let store = SettingsStore()
        store.attach(backend: backend)
        return store
    }
}
