import Foundation
import KansoBridge
import KansoUI

/// 「导入 OpenSSH 配置」对话框的视图模型：扫描、候选行文字、勾选、导入。不依赖 AppKit 控件，可直接驱动测试。
///
/// 解析与导入规则全部在 core（spec store-import §7.1 / §7.3）；这里只负责把 core 的结果整理成界面要显示的内容。
@MainActor
final class SshConfigImportModel {
    /// 一个候选主机行。
    struct Row: Equatable {
        let candidate: FfiSshConfigCandidate
        var isSelected: Bool
        /// 已有会话匹配（别名相同，或主机 / 端口 / 用户都相同）：导入时原地更新，界面上要标出来。
        var isExisting: Bool { candidate.existing != nil }
        var alias: String { candidate.alias }
        /// `user@host:port`；用户名为空时省略 `user@`。
        let destination: String
        /// 私钥路径（原样），没有则为 nil。
        let identityText: String?
        /// `via <ProxyJump 原文>`，没有则为 nil。
        let jumpText: String?
        /// 该条目自带的警告（已渲染）。
        let warnings: [String]
    }

    static let existingTagText = String(localized: "Saved, will update")

    private let core: DataTransferCore
    private let homeDirectory: String
    private let languages: [String]

    /// 配置文件的绝对路径。
    private(set) var configPath: String
    private(set) var rows: [Row] = []
    /// 文件级警告（已渲染）。
    private(set) var fileWarnings: [String] = []
    /// 扫描失败的原因（已渲染）；非 nil 时 `rows` 为空。
    private(set) var scanError: String?
    private(set) var isScanning = false
    /// 导入失败的原因（已渲染）；对话框保持打开。
    private(set) var importError: String?
    private(set) var isImporting = false
    /// 导入完成后的结果；非 nil 表示对话框应切换到结果页。
    private(set) var result: ImportResultContent?

    let groups: [FfiGroup]
    /// 目标分组 id；nil 表示顶层。
    var groupID: String?

    /// 防止过期结果覆盖：连续换文件时，较早发出的扫描可能后返回。
    private var scanGeneration = 0

    /// - Parameters:
    ///   - homeDirectory: 主目录；默认配置路径与 `~` 显示都以它为基准。测试注入临时目录。
    ///   - languages: 渲染 core 消息的语言；测试显式传入以保证断言稳定。
    init(core: DataTransferCore, homeDirectory: String = NSHomeDirectory(),
         languages: [String] = Bundle.main.preferredLocalizations) {
        self.core = core
        self.homeDirectory = homeDirectory
        self.languages = languages
        configPath = (homeDirectory as NSString).appendingPathComponent(".ssh/config")
        groups = core.groups().sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    // MARK: - 路径

    /// 路径的显示形式：主目录下的路径以 `~` 开头。
    var displayPath: String { Self.abbreviate(configPath, home: homeDirectory) }

    /// 文件面板的起始目录：`~/.ssh`。
    var sshDirectory: URL { URL(fileURLWithPath: homeDirectory).appendingPathComponent(".ssh", isDirectory: true) }

    static func abbreviate(_ path: String, home: String) -> String {
        if path == home { return "~" }
        let prefix = home.hasSuffix("/") ? home : home + "/"
        return path.hasPrefix(prefix) ? "~/" + path.dropFirst(prefix.count) : path
    }

    // MARK: - 扫描

    /// 扫描当前配置文件。失败时清空候选并记录错误，不抛出（对话框不关闭）。
    func scan() async {
        scanGeneration += 1
        let generation = scanGeneration
        isScanning = true
        defer { if generation == scanGeneration { isScanning = false } }
        do {
            let scan = try await core.scanSshConfig(path: configPath)
            guard generation == scanGeneration else { return }
            apply(scan: scan)
        } catch {
            guard generation == scanGeneration else { return }
            rows = []
            fileWarnings = []
            scanError = CoreMessageText.text(for: error, languages: languages)
        }
    }

    /// 换配置文件并重新扫描。
    func choose(path: String) async {
        configPath = path
        await scan()
    }

    /// 采用一次扫描结果。默认全部勾选：已保存的主机也勾选，更新是安全的（core 只改连接目标，见 §7.3）。
    func apply(scan: FfiSshConfigScan) {
        scanError = nil
        importError = nil
        fileWarnings = scan.warnings.map { CoreMessageText.text($0, languages: languages) }
        rows = scan.candidates.map { candidate in
            let userPart = candidate.user.isEmpty ? "" : candidate.user + "@"
            return Row(
                candidate: candidate,
                isSelected: true,
                destination: "\(userPart)\(candidate.host):\(candidate.port)",
                identityText: candidate.identityFile.flatMap { $0.isEmpty ? nil : $0 },
                jumpText: candidate.proxyJump.flatMap { $0.isEmpty ? nil : String(localized: "via \($0)") },
                warnings: candidate.warnings.map { CoreMessageText.text($0, languages: languages) }
            )
        }
    }

    // MARK: - 勾选

    var selectedCount: Int { rows.filter(\.isSelected).count }

    func setSelected(_ selected: Bool, at index: Int) {
        guard rows.indices.contains(index) else { return }
        rows[index].isSelected = selected
    }

    func toggle(at index: Int) {
        guard rows.indices.contains(index) else { return }
        rows[index].isSelected.toggle()
    }

    func selectAll() { for i in rows.indices { rows[i].isSelected = true } }
    func selectNone() { for i in rows.indices { rows[i].isSelected = false } }

    /// 未勾选任何项、正在扫描或正在导入时不能导入。
    var canImport: Bool { !isScanning && !isImporting && result == nil && selectedCount > 0 }

    // MARK: - 导入

    /// 导入已勾选的主机。成功后 `result` 有值；失败记录 `importError`，对话框保持打开让用户重试。
    func runImport() async {
        guard canImport else { return }
        isImporting = true
        importError = nil
        defer { isImporting = false }
        do {
            let report = try await core.importSshConfig(
                path: configPath, aliases: rows.filter(\.isSelected).map(\.alias), group: groupID
            )
            result = DataTransferText.content(for: report, languages: languages)
            NotificationCenter.default.post(name: .sessionLibraryDidChange, object: core)
        } catch {
            importError = CoreMessageText.text(for: error, languages: languages)
        }
    }
}
