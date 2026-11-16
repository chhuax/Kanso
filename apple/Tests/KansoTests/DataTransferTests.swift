import AppKit
import KansoBridge
@testable import Kanso
import XCTest

/// OpenSSH 配置导入对话框、`.kanso` 导出 / 导入流程、菜单入口与空状态页的测试。
/// 全部用 mock core 与注入的文件面板：不读真实 `~/.ssh`、不写真实文件、不弹模态框。
@MainActor
final class DataTransferTests: XCTestCase {
    private let home = "/tmp/kanso-test-home"

    private func makeModel(_ core: MockTransferCore = MockTransferCore()) -> SshConfigImportModel {
        SshConfigImportModel(core: core, homeDirectory: home, languages: ["en"])
    }

    private func scannedModel() -> (SshConfigImportModel, MockTransferCore) {
        let core = MockTransferCore()
        core.groupList = [FfiGroup(id: "g-prod", name: "Prod"), FfiGroup(id: "g-dev", name: "Dev")]
        let model = makeModel(core)
        model.apply(scan: TransferFixtures.scan)
        return (model, core)
    }

    // MARK: 路径

    func testDefaultPathDisplaysWithTilde() {
        let model = makeModel()
        XCTAssertEqual(model.configPath, "\(home)/.ssh/config")
        XCTAssertEqual(model.displayPath, "~/.ssh/config")
        XCTAssertEqual(model.sshDirectory.path, "\(home)/.ssh")
    }

    func testAbbreviateOnlyWholeHomeComponent() {
        XCTAssertEqual(SshConfigImportModel.abbreviate("\(home)/a", home: home), "~/a")
        XCTAssertEqual(SshConfigImportModel.abbreviate(home, home: home), "~")
        // 只是前缀相同的兄弟目录不能被当成主目录下的路径。
        XCTAssertEqual(SshConfigImportModel.abbreviate("\(home)-other/a", home: home), "\(home)-other/a")
        XCTAssertEqual(SshConfigImportModel.abbreviate("/etc/ssh/config", home: home), "/etc/ssh/config")
    }

    // MARK: 候选行

    func testRowTexts() {
        let (model, _) = scannedModel()
        XCTAssertEqual(model.rows.map(\.alias), ["web", "db", "legacy", "bastion"])
        XCTAssertEqual(model.rows[0].destination, "alice@web.example.com:22")
        XCTAssertEqual(model.rows[0].identityText, "~/.ssh/id_web")
        XCTAssertNil(model.rows[0].jumpText)
        XCTAssertEqual(model.rows[1].destination, "deploy@db.example.com:2222")
        XCTAssertEqual(model.rows[1].jumpText, String(localized: "via \("bastion")"), "跳板文字随界面语言")
        XCTAssertNil(model.rows[1].identityText)
    }

    func testExistingFlagAndWarnings() {
        let (model, _) = scannedModel()
        XCTAssertEqual(model.rows.map(\.isExisting), [false, true, false, false])
        XCTAssertEqual(model.rows[2].warnings.count, 1)
        XCTAssertTrue(model.rows[2].warnings[0].contains("abc"), "警告用 core 消息渲染")
        XCTAssertTrue(model.rows[0].warnings.isEmpty)
        XCTAssertEqual(model.fileWarnings.count, 1)
        XCTAssertTrue(model.fileWarnings[0].contains("Match"))
    }

    func testEmptyUserOmitsAtSign() {
        let model = makeModel()
        model.apply(scan: FfiSshConfigScan(candidates: [TransferFixtures.candidate("x", user: "")], warnings: []))
        XCTAssertEqual(model.rows[0].destination, "x.example.com:22")
    }

    // MARK: 勾选

    func testDefaultsAllSelectedIncludingExisting() {
        let (model, _) = scannedModel()
        XCTAssertEqual(model.rows.map(\.isSelected), [true, true, true, true])
        XCTAssertTrue(model.canImport)
    }

    func testSelectAllNoneAndImportDisabledWithoutSelection() {
        let (model, _) = scannedModel()
        model.selectNone()
        XCTAssertEqual(model.selectedCount, 0)
        XCTAssertFalse(model.canImport)
        model.toggle(at: 2)
        XCTAssertEqual(model.rows.map(\.isSelected), [false, false, true, false])
        XCTAssertTrue(model.canImport)
        model.selectAll()
        XCTAssertEqual(model.selectedCount, 4)
    }

    func testEmptyScanCannotImport() {
        let model = makeModel()
        XCTAssertFalse(model.canImport)
    }

    // MARK: 扫描

    func testScanFailureShowsErrorAndClearsRows() async {
        let core = MockTransferCore()
        core.scanResult = .failure(TransferFixtures.readFailure("~/.ssh/config"))
        let model = makeModel(core)
        model.apply(scan: TransferFixtures.scan)
        await model.scan()
        XCTAssertEqual(model.scanError, "Can’t read ~/.ssh/config: No such file.")
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertTrue(model.fileWarnings.isEmpty)
        XCTAssertFalse(model.canImport)
        XCTAssertEqual(core.scanPaths, ["\(home)/.ssh/config"])
    }

    func testChoosingAnotherFileRescansAndClearsError() async {
        let core = MockTransferCore()
        core.scanResult = .failure(TransferFixtures.readFailure("a"))
        let model = makeModel(core)
        await model.scan()
        XCTAssertNotNil(model.scanError)
        core.scanResult = .success(TransferFixtures.scan)
        await model.choose(path: "\(home)/work/config")
        XCTAssertNil(model.scanError)
        XCTAssertEqual(model.displayPath, "~/work/config")
        XCTAssertEqual(model.rows.count, 4)
        XCTAssertEqual(core.scanPaths.last ?? nil, "\(home)/work/config")
    }

    // MARK: 导入

    func testImportSendsSelectedAliasesAndGroupThenShowsReport() async {
        let (model, core) = scannedModel()
        core.importSshResult = .success(FfiSshConfigImportReport(
            added: 2, updated: 1, jumpsAdded: 1, multiHopIgnored: 1,
            warnings: [TransferFixtures.message("sshconfig-multi-hop-ignored", ["alias": "db"])]
        ))
        model.setSelected(false, at: 2)
        model.groupID = "g-prod"
        await model.runImport()
        XCTAssertEqual(core.importSshCalls.count, 1)
        XCTAssertEqual(core.importSshCalls[0].aliases, ["web", "db", "bastion"])
        XCTAssertEqual(core.importSshCalls[0].group, "g-prod")
        XCTAssertEqual(core.importSshCalls[0].path, "\(home)/.ssh/config")
        let result = try? XCTUnwrap(model.result)
        XCTAssertEqual(result?.summary, "2 added, 1 updated, 1 jump hosts added, 1 multi-hop ProxyJump entries ignored.")
        XCTAssertEqual(result?.details.count, 1)
        XCTAssertTrue(result?.details[0].contains("db") ?? false)
        XCTAssertFalse(model.canImport, "已导入完成，不能重复导入")
    }

    func testImportFailureKeepsSelectionAndReportsError() async {
        let (model, core) = scannedModel()
        core.importSshResult = .failure(TransferFixtures.readFailure("cfg"))
        await model.runImport()
        XCTAssertNil(model.result)
        XCTAssertEqual(model.importError, "Can’t read cfg: No such file.")
        XCTAssertTrue(model.canImport, "失败后可重试")
    }

    func testImportDoesNothingWithoutSelection() async {
        let (model, core) = scannedModel()
        model.selectNone()
        await model.runImport()
        XCTAssertTrue(core.importSshCalls.isEmpty)
    }

    func testGroupsSortedByName() {
        let (model, _) = scannedModel()
        XCTAssertEqual(model.groups.map(\.name), ["Dev", "Prod"])
    }

    // MARK: 导出 / 导入数据

    func testExportDefaultNameFormat() {
        // 2026-03-05 12:00 UTC。
        let date = Date(timeIntervalSince1970: 1_772_712_000)
        XCTAssertEqual(DataTransferText.exportDefaultName(date: date, timeZone: TimeZone(identifier: "UTC")!), "Kanso-20260305.kanso")
        // 东八区此刻仍是同一天；格式不随时区变化。
        XCTAssertEqual(DataTransferText.exportDefaultName(date: date, timeZone: TimeZone(identifier: "Asia/Shanghai")!), "Kanso-20260305.kanso")
    }

    func testExportAddsExtensionAndCallsCore() async {
        let core = MockTransferCore()
        let panels = MockPanels()
        panels.exportURL = URL(fileURLWithPath: "/tmp/kanso-test/backup")
        let flow = DataTransferFlow(core: core, panels: panels, languages: ["en"])
        let outcome = await flow.export(window: nil, now: Date(timeIntervalSince1970: 1_772_712_000))
        XCTAssertEqual(outcome, .exported(path: "/tmp/kanso-test/backup.kanso"))
        XCTAssertEqual(core.exportPaths, ["/tmp/kanso-test/backup.kanso"])
        XCTAssertEqual(panels.exportDefaultNames.count, 1)
        XCTAssertTrue(panels.exportDefaultNames[0].hasPrefix("Kanso-"))
        XCTAssertTrue(panels.exportDefaultNames[0].hasSuffix(".kanso"))
    }

    func testExportKeepsExistingExtensionCaseInsensitive() async {
        let core = MockTransferCore()
        let panels = MockPanels()
        panels.exportURL = URL(fileURLWithPath: "/tmp/kanso-test/a.KANSO")
        let outcome = await DataTransferFlow(core: core, panels: panels, languages: ["en"]).export(window: nil)
        XCTAssertEqual(outcome, .exported(path: "/tmp/kanso-test/a.KANSO"))
    }

    func testExportCancelledAndFailed() async {
        let core = MockTransferCore()
        let panels = MockPanels()
        let flow = DataTransferFlow(core: core, panels: panels, languages: ["en"])
        let cancelled = await flow.export(window: nil)
        XCTAssertEqual(cancelled, .cancelled)
        XCTAssertTrue(core.exportPaths.isEmpty)

        panels.exportURL = URL(fileURLWithPath: "/tmp/kanso-test/a.kanso")
        core.exportError = FfiCoreError.User(message: TransferFixtures.message("export-write-failed", ["path": "p", "reason": "disk full"]))
        let failed = await flow.export(window: nil)
        XCTAssertEqual(failed, .failed("Can’t save p: disk full."))
    }

    func testImportDataResultListsSkippedWithReasons() async {
        let core = MockTransferCore()
        core.importDataResult = .success(FfiImportReport(added: 2, updated: 1, skipped: [
            TransferFixtures.message("import-skipped-unsupported-kind", ["name": "ftp1", "kind": "ftp"]),
            TransferFixtures.message("import-skipped-invalid", ["name": "bad", "detail": "empty host"]),
        ], converted: []))
        let panels = MockPanels()
        panels.importURL = URL(fileURLWithPath: "/tmp/kanso-test/in.kanso")
        let outcome = await DataTransferFlow(core: core, panels: panels, languages: ["en"]).importData(window: nil)
        guard case .imported(let content) = outcome else { return XCTFail("应为 imported：\(outcome)") }
        XCTAssertEqual(content.summary, "2 added, 1 updated, 2 skipped.")
        XCTAssertEqual(content.details, ["Skipped “ftp1”: the type “ftp” isn’t supported.", "Skipped “bad”: empty host"])
        XCTAssertEqual(core.importDataPaths, ["/tmp/kanso-test/in.kanso"])
    }

    func testImportDataReportsConvertedSftpScopeSeparately() {
        let content = DataTransferText.content(for: FfiImportReport(added: 1, updated: 0, skipped: [], converted: [
            TransferFixtures.message("import-snippet-converted", ["name": "files"]),
        ]), languages: ["en"])
        XCTAssertTrue(content.summary.contains("1 converted"))
        XCTAssertEqual(content.details, ["Converted “files”: SFTP-only scope changed to global."])
    }

    func testImportDataCancelledAndFailed() async {
        let core = MockTransferCore()
        let panels = MockPanels()
        let flow = DataTransferFlow(core: core, panels: panels, languages: ["en"])
        let cancelled = await flow.importData(window: nil)
        XCTAssertEqual(cancelled, .cancelled)
        XCTAssertTrue(core.importDataPaths.isEmpty)
        panels.importURL = URL(fileURLWithPath: "/tmp/kanso-test/in.kanso")
        core.importDataResult = .failure(FfiCoreError.User(message: TransferFixtures.message("import-not-kanso-file")))
        let failed = await flow.importData(window: nil)
        XCTAssertEqual(failed, .failed("This isn’t a Kanso export file."))
    }

    // MARK: 对话框视图

    private func makeDialog(_ core: MockTransferCore = MockTransferCore(), panels: MockPanels = MockPanels())
        -> (SshConfigImportViewController, MockTransferCore)
    {
        let model = makeModel(core)
        let dialog = SshConfigImportViewController(model: model, panels: panels)
        _ = dialog.view
        return (dialog, core)
    }

    func testDialogListsRowsWithTagOnlyForExisting() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        let (dialog, _) = makeDialog(core)
        await dialog.startScan().value
        XCTAssertEqual(dialog.list.rowViews.count, 4)
        XCTAssertEqual(dialog.list.rowViews.map { $0.existingTag != nil }, [false, true, false, false])
        XCTAssertEqual(dialog.list.rowViews.map { $0.warningIcon != nil }, [false, false, true, false])
        XCTAssertEqual(dialog.list.rowViews[1].titleLabel.stringValue, "db")
        XCTAssertEqual(dialog.list.rowViews[1].destinationLabel.stringValue, "deploy@db.example.com:2222")
        XCTAssertEqual(dialog.list.rowViews[1].extraLabel.stringValue, String(localized: "via \("bastion")"))
        XCTAssertFalse(dialog.warningBanner.isHidden)
        XCTAssertTrue(dialog.errorBanner.isHidden)
        XCTAssertEqual(dialog.pathField.stringValue, "~/.ssh/config")
        XCTAssertTrue(dialog.importButton.isEnabled)
    }

    func testDialogImportButtonFollowsSelection() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        let (dialog, _) = makeDialog(core)
        await dialog.startScan().value
        dialog.selectNone()
        XCTAssertFalse(dialog.importButton.isEnabled)
        XCTAssertTrue(dialog.list.rowViews.allSatisfy { !$0.checkbox.isChecked })
        dialog.list.rowViews[0].onToggle?()
        XCTAssertTrue(dialog.importButton.isEnabled)
        XCTAssertTrue(dialog.list.rowViews[0].checkbox.isChecked)
        dialog.selectAll()
        XCTAssertTrue(dialog.list.rowViews.allSatisfy(\.checkbox.isChecked))
    }

    func testDialogShowsScanErrorAndKeepsOpen() async {
        let core = MockTransferCore()
        core.scanResult = .failure(TransferFixtures.readFailure("x"))
        let (dialog, _) = makeDialog(core)
        var finished = false
        dialog.onFinish = { finished = true }
        await dialog.startScan().value
        XCTAssertFalse(dialog.errorBanner.isHidden)
        XCTAssertTrue(dialog.errorBanner.label.stringValue.contains("Can’t read"))
        XCTAssertFalse(dialog.importButton.isEnabled)
        XCTAssertFalse(finished, "扫描失败不关闭对话框")
    }

    func testDialogChooseConfigRescansWithPickedFile() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        let panels = MockPanels()
        panels.sshConfigURL = URL(fileURLWithPath: "\(home)/other/config")
        let (dialog, _) = makeDialog(core, panels: panels)
        await dialog.chooseConfig().value
        XCTAssertEqual(core.scanPaths.last ?? nil, "\(home)/other/config")
        XCTAssertEqual(dialog.pathField.stringValue, "~/other/config")
        XCTAssertEqual(panels.sshStartDirectories.map(\.path), ["\(home)/.ssh"])
    }

    func testDialogChooseCancelledKeepsState() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        let (dialog, _) = makeDialog(core)
        await dialog.startScan().value
        await dialog.chooseConfig().value
        XCTAssertEqual(core.scanPaths.count, 1, "取消选择不重新扫描")
    }

    func testDialogImportSwitchesToResultAndNotifies() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        core.importSshResult = .success(FfiSshConfigImportReport(added: 4, updated: 0, jumpsAdded: 0, multiHopIgnored: 0, warnings: []))
        let (dialog, _) = makeDialog(core)
        var imported = 0
        dialog.onImported = { imported += 1 }
        await dialog.startScan().value
        await dialog.performImport().value
        XCTAssertEqual(imported, 1)
        XCTAssertNotNil(dialog.model.result)
        XCTAssertTrue(dialog.view.subviewsRecursive.contains { ($0 as? NSTextField)?.stringValue.hasPrefix("4 added") == true })
        XCTAssertEqual(core.importSshCalls.count, 1)
    }

    func testDialogImportFailureShowsMessageInFooter() async {
        let core = MockTransferCore()
        core.scanResult = .success(TransferFixtures.scan)
        core.importSshResult = .failure(TransferFixtures.readFailure("cfg"))
        let (dialog, _) = makeDialog(core)
        var imported = 0
        dialog.onImported = { imported += 1 }
        await dialog.startScan().value
        await dialog.performImport().value
        XCTAssertEqual(imported, 0)
        XCTAssertEqual(dialog.shell.messageLabel.stringValue, "Can’t read cfg: No such file.")
        XCTAssertTrue(dialog.importButton.isEnabled)
    }

    func testDataResultDialogShowsSummaryAndDetails() {
        let content = ImportResultContent(summary: "1 added, 0 updated, 1 skipped.", detailsTitle: "Skipped", details: ["Skipped “a”: x"])
        let vc = DataResultViewController(title: "Import Complete", symbol: "square.and.arrow.down", content: content)
        _ = vc.view
        let texts = vc.view.subviewsRecursive.compactMap { ($0 as? NSTextField)?.stringValue }
        XCTAssertTrue(texts.contains("1 added, 0 updated, 1 skipped."))
        XCTAssertTrue(texts.contains("Skipped “a”: x"))
        var finished = false
        vc.onFinish = { finished = true }
        vc.doneButton.performClick(nil)
        XCTAssertTrue(finished)
    }

    // MARK: 菜单

    private func sessionMenu() throws -> NSMenu {
        let settings = SettingsStore()
        let main = AppDelegate.makeMainMenu(appearance: AppearanceController(settings: settings, apply: { _ in }),
                                            gutter: GutterModeController(settings: settings))
        return try XCTUnwrap(main.items.first { $0.submenu?.items.contains { $0.action == #selector(MainWindowController.kansoNewSession(_:)) } == true }?.submenu)
    }

    func testSessionMenuHasImportExportGroupBetweenSeparators() throws {
        let menu = try sessionMenu()
        let actions: [Selector] = [
            #selector(MainWindowController.kansoImportSshConfig(_:)),
            #selector(MainWindowController.kansoExportData(_:)),
            #selector(MainWindowController.kansoImportData(_:)),
        ]
        let indexes = try actions.map { action in try XCTUnwrap(menu.items.firstIndex { $0.action == action }, "缺少菜单项 \(action)") }
        XCTAssertEqual(indexes, Array(indexes[0]...indexes[0] + 2), "三项相邻")
        XCTAssertTrue(menu.items[indexes[0] - 1].isSeparatorItem)
        XCTAssertTrue(menu.items[indexes[2] + 1].isSeparatorItem)
        for index in indexes {
            XCTAssertTrue(menu.items[index].isEnabled)
            XCTAssertTrue(MainWindowController.instancesRespond(to: menu.items[index].action), "窗口控制器必须实现该动作")
        }
    }

    // MARK: 空状态页

    func testEmptyStateHasThreeEntries() {
        let view = EmptyStateView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        var calls: [String] = []
        view.onNewLocalShell = { calls.append("shell") }
        view.onNewSession = { calls.append("session") }
        view.onImportSshConfig = { calls.append("import") }
        view.layoutSubtreeIfNeeded()
        XCTAssertEqual(view.newShellButton.style, .primary)
        XCTAssertEqual(view.newSessionButton.style, .secondary)
        XCTAssertEqual(view.importButton.style, .secondary)
        let frames = [view.newShellButton, view.newSessionButton, view.importButton].map(\.frame)
        XCTAssertTrue(frames.allSatisfy { view.bounds.contains($0) }, "三个按钮都在视图内")
        XCTAssertTrue(frames[0].maxY <= frames[1].minY && frames[1].maxY <= frames[2].minY, "自上而下排列且不重叠")
        view.newShellButton.performClick(nil)
        view.newSessionButton.performClick(nil)
        view.importButton.performClick(nil)
        XCTAssertEqual(calls, ["shell", "session", "import"])
    }
}

private extension NSView {
    var subviewsRecursive: [NSView] { subviews + subviews.flatMap(\.subviewsRecursive) }
}
