import AppKit
import UniformTypeIdentifiers

/// 导入 / 导出流程用到的文件面板。抽成协议是为了测试注入替身，测试不弹真的模态面板。
/// 取消一律返回 nil。
@MainActor
protocol DataFilePanels: AnyObject {
    /// 选择 OpenSSH 配置文件；面板从 `directory`（通常是 `~/.ssh`）开始，显示隐藏文件。
    func chooseSshConfig(startingAt directory: URL, window: NSWindow?) async -> URL?
    /// 选择导出位置；`defaultName` 是建议文件名（`Kanso-YYYYMMDD.kanso`）。
    func chooseExportDestination(defaultName: String, window: NSWindow?) async -> URL?
    /// 选择要导入的 `.kanso` 文件。
    func chooseImportSource(window: NSWindow?) async -> URL?
}

/// 基于 `NSOpenPanel` / `NSSavePanel` 的实现：有窗口时以 sheet 呈现，没有窗口时退化为应用级模态。
@MainActor
final class SystemDataFilePanels: DataFilePanels {
    /// `.kanso` 没有系统登记的类型，按扩展名生成动态类型即可用于面板过滤。
    private static var kansoType: UTType? { UTType(filenameExtension: DataTransferText.fileExtension) }

    func chooseSshConfig(startingAt directory: URL, window: NSWindow?) async -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        // `~/.ssh` 与 `.config` 之类的路径以点开头，必须显示隐藏文件。
        panel.showsHiddenFiles = true
        panel.directoryURL = directory
        return await run(panel, window: window) ? panel.url : nil
    }

    func chooseExportDestination(defaultName: String, window: NSWindow?) async -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = defaultName
        if let type = Self.kansoType { panel.allowedContentTypes = [type] }
        panel.canCreateDirectories = true
        return await run(panel, window: window) ? panel.url : nil
    }

    func chooseImportSource(window: NSWindow?) async -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        if let type = Self.kansoType { panel.allowedContentTypes = [type] }
        return await run(panel, window: window) ? panel.url : nil
    }

    private func run(_ panel: NSSavePanel, window: NSWindow?) async -> Bool {
        guard let window else { return panel.runModal() == .OK }
        return await withCheckedContinuation { continuation in
            panel.beginSheetModal(for: window) { continuation.resume(returning: $0 == .OK) }
        }
    }
}
