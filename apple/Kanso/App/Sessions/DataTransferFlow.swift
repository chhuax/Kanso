import AppKit
import KansoBridge
import KansoUI

/// 导入 / 导出结果页的内容：一句汇总 + 可选的明细列表（警告、被跳过的条目及原因）。
struct ImportResultContent: Equatable {
    var summary: String
    /// 明细列表的小标题；`details` 为空时不显示。
    var detailsTitle: String?
    var details: [String]
}

/// 导入 / 导出相关的文字拼装。消息一律交给 core 的 `kanso-l10n` 渲染（`CoreMessageText`），
/// 汇总句的 key 与参数名必须与 `locales/*.ftl` 一致。
enum DataTransferText {
    /// 数据文件扩展名（不含点）。
    static let fileExtension = "kanso"

    /// 导出对话框的默认文件名：`Kanso-YYYYMMDD.kanso`。
    /// 固定用公历与 `en_US_POSIX`：文件名格式不能随系统区域（如佛历、日历设置）变化。
    static func exportDefaultName(date: Date = Date(), timeZone: TimeZone = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = timeZone
        formatter.dateFormat = "yyyyMMdd"
        return "Kanso-\(formatter.string(from: date)).\(fileExtension)"
    }

    private static func count(_ name: String, _ value: UInt32) -> FfiMessageArg {
        FfiMessageArg(name: name, value: .int(value: Int64(value)))
    }

    /// `.kanso` 数据导入结果。
    static func content(for report: FfiImportReport, languages: [String] = Bundle.main.preferredLocalizations) -> ImportResultContent {
        let summary = FfiMessage(key: "import-report", args: [
            count("added", report.added), count("updated", report.updated), count("skipped", UInt32(clamping: report.skipped.count)),
        ])
        let convertedSuffix = report.converted.isEmpty ? "" : CoreMessageText.text(
            FfiMessage(key: "import-converted-suffix", args: [count("count", UInt32(clamping: report.converted.count))]),
            languages: languages
        )
        return ImportResultContent(
            summary: CoreMessageText.text(summary, languages: languages) + convertedSuffix,
            detailsTitle: report.converted.isEmpty
                ? (report.skipped.isEmpty ? nil : String(localized: "Skipped"))
                : CoreMessageText.text(FfiMessage(key: "import-converted-and-skipped", args: []), languages: languages),
            details: (report.converted + report.skipped).map { CoreMessageText.text($0, languages: languages) }
        )
    }

    /// OpenSSH 配置导入结果。
    static func content(for report: FfiSshConfigImportReport, languages: [String] = Bundle.main.preferredLocalizations) -> ImportResultContent {
        let summary = FfiMessage(key: "sshconfig-import-report", args: [
            count("added", report.added), count("updated", report.updated),
            count("jumps_added", report.jumpsAdded), count("multi_hop_ignored", report.multiHopIgnored),
        ])
        return ImportResultContent(
            summary: CoreMessageText.text(summary, languages: languages),
            detailsTitle: report.warnings.isEmpty ? nil : String(localized: "Warnings"),
            details: report.warnings.map { CoreMessageText.text($0, languages: languages) }
        )
    }
}

/// 「导出数据… / 导入数据…」的流程：选文件 -> 调 core -> 产出可展示的结果。
/// 不碰任何窗口与对话框（呈现由 `SessionPresenter` 负责），文件面板注入，所以可以直接驱动测试。
@MainActor
final class DataTransferFlow {
    enum ExportOutcome: Equatable {
        case cancelled
        case exported(path: String)
        /// 已渲染好的错误文字。
        case failed(String)
    }

    enum ImportOutcome: Equatable {
        case cancelled
        case imported(ImportResultContent)
        case failed(String)
    }

    private let core: DataTransferCore
    private let panels: DataFilePanels
    private let languages: [String]

    init(core: DataTransferCore, panels: DataFilePanels, languages: [String] = Bundle.main.preferredLocalizations) {
        self.core = core
        self.panels = panels
        self.languages = languages
    }

    func export(window: NSWindow?, now: Date = Date()) async -> ExportOutcome {
        guard var url = await panels.chooseExportDestination(defaultName: DataTransferText.exportDefaultName(date: now), window: window)
        else { return .cancelled }
        // 导出时自动补扩展名（spec §6.1）：用户在面板里改掉扩展名时面板的类型过滤不一定拦得住。
        if url.pathExtension.lowercased() != DataTransferText.fileExtension {
            url.appendPathExtension(DataTransferText.fileExtension)
        }
        do {
            try await core.exportData(path: url.path)
            return .exported(path: url.path)
        } catch {
            return .failed(CoreMessageText.text(for: error, languages: languages))
        }
    }

    func importData(window: NSWindow?) async -> ImportOutcome {
        guard let url = await panels.chooseImportSource(window: window) else { return .cancelled }
        do {
            let report = try await core.importData(path: url.path)
            NotificationCenter.default.post(name: .sessionLibraryDidChange, object: core)
            return .imported(DataTransferText.content(for: report, languages: languages))
        } catch {
            return .failed(CoreMessageText.text(for: error, languages: languages))
        }
    }
}
