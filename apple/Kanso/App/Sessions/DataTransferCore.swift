import KansoBridge

/// OpenSSH 配置导入与 `.kanso` 数据导出 / 导入用到的 core 能力。
///
/// 抽成协议是为了测试注入替身：测试不读真实 `~/.ssh`、不写真实文件。生产环境直接传 `KansoCore`。
/// 要求 `Sendable`：模型在主线程调用其 async 方法，值要能跨隔离域传递（`KansoCore` 本身就是 Sendable）。
protocol DataTransferCore: AnyObject, Sendable {
    func groups() -> [FfiGroup]
    func scanSshConfig(path: String?) async throws -> FfiSshConfigScan
    func importSshConfig(path: String?, aliases: [String], group: String?) async throws -> FfiSshConfigImportReport
    func exportData(path: String) async throws
    func importData(path: String) async throws -> FfiImportReport
}

extension KansoCore: DataTransferCore {}
