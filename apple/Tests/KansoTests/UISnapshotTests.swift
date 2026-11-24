import AppKit
import KansoBridge
import KansoUI
@testable import TerminalKit
@testable import Kanso
import XCTest

/// 自绘界面（编辑对话框、会话启动器）的截图测试：离屏渲染到 2x 位图，深色 / 浅色各一张，
/// 与 `apple/Tests/Snapshots/` 下的参考图逐像素比较（比较与录制逻辑复用 `SnapshotTests`）。
///
/// 录制：`KANSO_RECORD_SNAPSHOTS=1 cargo xtask apple-test --only-testing KansoTests/UISnapshotTests`。
/// 文字用系统字体渲染，系统大版本升级后出现整体性小差异时重新录制即可。
@MainActor
final class UISnapshotTests: XCTestCase {
    private static let scale: CGFloat = 2

    func testSemanticColorDark() throws { try verify(semanticCanvas(.dark), .darkAqua, "semantic-color-dark") }
    func testSemanticColorLight() throws { try verify(semanticCanvas(.light), .aqua, "semantic-color-light") }

    private func semanticCanvas(_ appearance: TerminalAppearance) throws -> NSView {
        let size = NSSize(width: 900, height: 540)
        let renderer = try XCTUnwrap(TerminalRenderer())
        renderer.timeZone = TimeZone(secondsFromGMT: 0)!
        let grid = renderer.gridSize(for: size, scale: Self.scale)
        let frame = try XCTUnwrap(FrameSnapshot(coreFrame: FrameHandle.mockSemanticColors(cols: UInt16(grid.cols), rows: UInt16(grid.rows))))
        let image = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: Self.scale, appearance: appearance))
        let root = NSImageView(frame: NSRect(origin: .zero, size: size))
        root.image = NSImage(cgImage: image, size: size)
        return root
    }

    func testCommandBlocksDark() throws { try verify(commandBlocksCanvas(.dark), .darkAqua, "command-blocks-dark") }
    func testCommandBlocksLight() throws { try verify(commandBlocksCanvas(.light), .aqua, "command-blocks-light") }

    private func commandBlocksCanvas(_ appearance: TerminalAppearance) throws -> NSView {
        let size = NSSize(width: 740, height: 240)
        let renderer = try XCTUnwrap(TerminalRenderer())
        renderer.timeZone = TimeZone(secondsFromGMT: 0)!
        renderer.selectedCommandBlock = 2
        let grid = renderer.gridSize(for: size, scale: Self.scale)
        let frame = try XCTUnwrap(FrameSnapshot(coreFrame: FrameHandle.mockCommandBlocks(cols: UInt16(grid.cols), rows: UInt16(grid.rows))))
        let image = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: Self.scale, appearance: appearance))
        let root = NSImageView(frame: NSRect(origin: .zero, size: size))
        root.image = NSImage(cgImage: image, size: size)
        return root
    }

    func testEditorSSHDark() throws { try verify(editorCanvas(), .darkAqua, "editor-ssh-dark") }
    func testEditorSSHLight() throws { try verify(editorCanvas(), .aqua, "editor-ssh-light") }
    func testLauncherDark() throws { try verify(launcherCanvas(), .darkAqua, "launcher-dark") }
    func testLauncherLight() throws { try verify(launcherCanvas(), .aqua, "launcher-light") }

    func testPaletteDark() throws { try verify(paletteCanvas(), .darkAqua, "palette-dark") }
    func testPaletteLight() throws { try verify(paletteCanvas(), .aqua, "palette-light") }
    func testCompletionDark() throws { try verify(completionCanvas(), .darkAqua, "completion-popup-dark") }
    func testCompletionLight() throws { try verify(completionCanvas(), .aqua, "completion-popup-light") }

    func testDirectoryPickerDark() throws { try verify(directoryPickerCanvas(), .darkAqua, "directory-picker-dark") }
    func testDirectoryPickerLight() throws { try verify(directoryPickerCanvas(), .aqua, "directory-picker-light") }

    private func directoryPickerCanvas() -> NSView {
        let controller = DirectoryPickerViewController(directory: "/tmp/project")
        let root = controller.view
        controller.model.candidates = CommandPaletteFuzzy.prepare(["Sources", "Tests", "apple", "crates", "docs"].map {
            DirectoryPickerModel.Entry(name: $0, path: "/tmp/project/" + $0)
        }, text: { $0.name })
        controller.reload()
        let size = DirectoryPickerViewController.size
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80),
                      content: root, contentSize: size, retaining: controller)
    }
    func testSenderDark() throws { try verify(senderCanvas(), .darkAqua, "sender-dark") }
    func testSenderLight() throws { try verify(senderCanvas(), .aqua, "sender-light") }

    func testSshImportDark() throws { try verify(sshImportCanvas(), .darkAqua, "ssh-import-dark") }
    func testSshImportLight() throws { try verify(sshImportCanvas(), .aqua, "ssh-import-light") }

    func testSettingsGeneralDark() throws { try verify(settingsGeneralCanvas(), .darkAqua, "settings-general-dark") }
    func testSettingsGeneralLight() throws { try verify(settingsGeneralCanvas(), .aqua, "settings-general-light") }
    func testSettingsDisplayDark() throws { try verify(settingsDisplayCanvas(), .darkAqua, "settings-display-dark") }
    func testSettingsDisplayLight() throws { try verify(settingsDisplayCanvas(), .aqua, "settings-display-light") }
    func testSettingsKeyboardDark() throws { try verify(settingsKeyboardCanvas(), .darkAqua, "settings-keyboard-dark") }
    func testSettingsKeyboardLight() throws { try verify(settingsKeyboardCanvas(), .aqua, "settings-keyboard-light") }

    func testTopTabStripDark() throws { try verify(topTabStripCanvas(), .darkAqua, "top-tab-strip-dark") }
    func testTopTabStripLight() throws { try verify(topTabStripCanvas(), .aqua, "top-tab-strip-light") }

    func testFilerPanelDark() async throws { try verify(await filerCanvas(transfer: false), .darkAqua, "filer-panel-dark") }
    func testFilerPanelLight() async throws { try verify(await filerCanvas(transfer: false), .aqua, "filer-panel-light") }
    func testGitPanelDark() async throws { try verify(await gitCanvas(), .darkAqua, "git-panel-dark") }
    func testGitPanelLight() async throws { try verify(await gitCanvas(), .aqua, "git-panel-light") }
    func testSftpDualDark() async throws { try verify(await sftpDualCanvas(), .darkAqua, "sftp-dual-dark") }
    func testSftpDualLight() async throws { try verify(await sftpDualCanvas(), .aqua, "sftp-dual-light") }
    func testFilerPanelTransferDark() async throws { try verify(await filerCanvas(transfer: true), .darkAqua, "filer-panel-transfer-dark") }
    func testFilerPanelTransferLight() async throws { try verify(await filerCanvas(transfer: true), .aqua, "filer-panel-transfer-light") }
    func testFilerConflictDark() throws { try verify(filerConflictCanvas(), .darkAqua, "filer-conflict-dark") }
    func testFilerConflictLight() throws { try verify(filerConflictCanvas(), .aqua, "filer-conflict-light") }

    func testTabRowAgentsDark() throws { try verify(tabRowAgentsCanvas(), .darkAqua, "tab-row-agents-dark") }
    func testTabRowStatesDark() throws { try verify(tabRowStatesCanvas(), .darkAqua, "tab-row-states-dark") }

    // MARK: 画布

    private func completionCanvas() -> NSView {
        let size = CompletionPopupView.size(count: 4)
        let popup = CompletionPopupView(frame: NSRect(origin: .zero, size: size))
        popup.candidates = [
            FfiCompletionCandidate(replacement: "git status", category: .history, deleteCodepoints: 3),
            FfiCompletionCandidate(replacement: "git log --oneline", category: .git, deleteCodepoints: 3),
            FfiCompletionCandidate(replacement: "git diff", category: .git, deleteCodepoints: 3),
            FfiCompletionCandidate(replacement: "git add ", category: .git, deleteCodepoints: 3)
        ]
        popup.selected = 1
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: popup, contentSize: size, retaining: popup)
    }

    private func senderCanvas() throws -> NSView {
        let core = KansoCore.newMock(listener: CoreEventDispatcher())
        for snippet in core.snippets() { try core.deleteSnippet(id: snippet.id) }
        for (name, text) in [("磁盘用量", "df -h"), ("服务状态", "systemctl status nginx"), ("近期日志", "journalctl -n 100"), ("内存状态", "free -h"), ("当前目录", "pwd"), ("网络连接", "ss -tln")] {
            try core.saveSnippet(snippet: FfiSnippet(id: UUID().uuidString, name: name, text: text, lineEnding: .lf, scope: .global))
        }
        let model = SenderModel(core: core)
        model.isVisible = true
        let active = TabItem(id: 1, kind: .ssh, name: "Production", connection: .connected)
        model.refresh(context: active, tabs: [active, TabItem(id: 2, kind: .local, name: "Local", connection: .connected)])
        model.allTargets = true
        model.draft = SenderDraft(text: "  cd /srv/app\nsystemctl status nginx", name: "检查服务", ending: .lf)
        let panel = SenderPanel(model: model)
        let size = NSSize(width: 900, height: 250)
        return canvas(size: NSSize(width: 980, height: 330), content: panel, contentSize: size, retaining: model)
    }

    /// 标签行 AI 图标：Claude（有素材）、Codex（有素材）、Aider（无素材 → sparkles）。
    private func tabRowAgentsCanvas() -> NSView {
        func row(_ id: SessionID, name: String, activity: TabActivity, selection: TabRowSelection) -> TabRowView.Model {
            TabRowView.Model(
                item: TabItem(
                    id: id, kind: .local, name: name, cwd: "/Users/demo/project",
                    connection: .connected, activity: activity
                ),
                sidebarIndex: Int(id) - 1, selection: selection
            )
        }
        let listHeight = SidebarListView.verticalInset * 2
            + 3 * (DesignTokens.Spacing.tabRowHeight + SidebarListView.rowSpacing)
        let list = SidebarListView(frame: NSRect(x: 0, y: 0, width: 240, height: listHeight))
        list.update([
            SidebarListView.Section(
                pane: PaneID(raw: 1), titleIndex: 1, showsHeader: false, isActivePane: true,
                rows: [
                    row(1, name: "claude", activity: .agentWorking(name: "Claude Code", iconAsset: "claude"), selection: .windowActive),
                    row(2, name: "codex", activity: .agentIdle(name: "Codex", iconAsset: "codex"), selection: .none),
                    row(3, name: "aider", activity: .agentWorking(name: "Aider", iconAsset: nil), selection: .none),
                ]
            ),
        ])
        list.layoutSubtreeIfNeeded()
        let size = NSSize(width: 240, height: listHeight)
        list.frame = NSRect(origin: .zero, size: size)
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: list, contentSize: size, retaining: list)
    }

    /// 标签行状态：选中本地（含分支）、SSH 运行中、SSH 空闲、SSH 已断开（暗灰条 + 副标题"已断开"）。
    private func tabRowStatesCanvas() -> NSView {
        let rows: [TabRowView.Model] = [
            TabRowView.Model(
                item: TabItem(
                    id: 1, kind: .local, name: "local", cwd: "/data/workspace/iuap-yms-manage",
                    gitBranch: "onpremise-V3R6_20240712", connection: .connected
                ),
                sidebarIndex: 0, selection: .windowActive
            ),
            TabRowView.Model(
                item: TabItem(id: 2, kind: .ssh, name: "jumper-dev", connection: .connected, activity: .commandRunning),
                sidebarIndex: 1, selection: .none
            ),
            TabRowView.Model(
                item: TabItem(id: 3, kind: .ssh, name: "beijing", connection: .connected),
                sidebarIndex: 2, selection: .none
            ),
            TabRowView.Model(
                item: TabItem(id: 4, kind: .ssh, name: "生产堡垒机", connection: .closed(reason: nil)),
                sidebarIndex: 3, selection: .none
            ),
        ]
        let listHeight = SidebarListView.verticalInset * 2
            + CGFloat(rows.count) * (DesignTokens.Spacing.tabRowHeight + SidebarListView.rowSpacing)
        let list = SidebarListView(frame: NSRect(x: 0, y: 0, width: 240, height: listHeight))
        list.update([
            SidebarListView.Section(pane: PaneID(raw: 1), titleIndex: 1, showsHeader: false, isActivePane: true, rows: rows),
        ])
        list.layoutSubtreeIfNeeded()
        let size = NSSize(width: 240, height: listHeight)
        list.frame = NSRect(origin: .zero, size: size)
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: list, contentSize: size, retaining: list)
    }

    /// 设置窗口"通用"页：外观模式 + 语言（默认值）。
    private func settingsGeneralCanvas() -> NSView {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let settings = SettingsStore()
        settings.attach(backend: MemorySettingsBackend(), themesDirectory: directory)
        addTeardownBlock { try FileManager.default.removeItem(at: directory) }
        let appearance = AppearanceController(settings: settings, apply: { _ in })
        let page = GeneralSettingsViewController(appearance: appearance, openURL: { _ in })
        return pageCanvas(page, retaining: appearance)
    }

    /// 设置窗口"显示"页：非默认值（Menlo、下划线光标、闪烁、回滚 50 000、对比度 7），用本机必有的字体，截图不依赖安装的字体。
    private func settingsDisplayCanvas() -> NSView {
        let settings = SettingsStore.attached(to: MemorySettingsBackend())
        settings.updateDisplay {
            $0.fontFamily = "Menlo"
            $0.fontSize = 14
            $0.cursorShape = .underline
            $0.cursorBlink = true
            $0.scrollbackLines = 50_000
            $0.minimumContrastRatio = 7
        }
        let page = DisplaySettingsViewController(settings: settings, fontFamilies: { [] })
        return pageCanvas(page)
    }

    /// 设置窗口"键盘快捷键"页：复制改绑为 ⌃⇧C、粘贴清空，其余默认。
    private func settingsKeyboardCanvas() -> NSView {
        let settings = SettingsStore.attached(to: MemorySettingsBackend())
        let controller = KeybindingController(settings: settings, menuProvider: { nil })
        _ = controller.assign(KeyChord(key: "c", modifiers: [.control, .shift]), to: .copy)
        controller.clear(.paste)
        let page = KeyboardSettingsViewController(keybindings: controller, prompter: AlertKeybindingConflictPrompter())
        return pageCanvas(page, retaining: controller)
    }

    /// 设置页本身就是窗口内容区（自带窗口底色），所以不加边距，按页面尺寸整体截取。
    private func pageCanvas(_ page: NSViewController, retaining extra: AnyObject? = nil) -> NSView {
        let view = page.view
        view.layoutSubtreeIfNeeded()
        let size = view.fittingSize
        let root = RetainingBackdrop(frame: NSRect(origin: .zero, size: size), owner: [page, extra as Any] as NSArray)
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0
        view.frame = NSRect(origin: .zero, size: size)
        root.addSubview(view)
        return root
    }

    /// 辅助 Filer：本地目录（mock 数据，固定中午 UTC 的日期避免时区边界），选中一行；`transfer` 时底部是上传进度。
    private func filerCanvas(transfer: Bool) async -> NSView {
        let env = FakeEnv(), prompter = FakePrompter()
        let model = FilerModel(env: env, prompter: prompter, clock: { 0 })
        let ds = FakeDataSource(remote: transfer)
        let day: (Int) -> Date = { Date(timeIntervalSince1970: 1_775_000_000 + TimeInterval($0) * 86_400) }
        func entry(_ n: String, dir: Bool = false, link: Bool = false, size: UInt64 = 2048, day d: Int) -> FilerEntry {
            FilerEntry(name: n, path: "/Users/demo/\(n)", isDirectory: dir, isSymlink: link, size: dir ? nil : size, modified: day(d), permissions: 0o644)
        }
        ds.home = "/Users/demo"
        ds.dirs["/Users/demo"] = [
            entry(".config", dir: true, day: 3), entry(".cache", dir: true, day: 40), entry("Documents", dir: true, day: 10),
            entry("projects", dir: true, day: 22), entry("bin", link: true, day: 5), entry("notes.md", day: 31),
            entry("build.sh", day: 12), entry("main.swift", day: 18), entry("photo.png", day: 20), entry("backup.tar.gz", day: 25),
            entry("data.json", day: 26), entry("song.mp3", day: 27), entry("README", day: 28),
        ]
        env.sources[1] = ds
        let panel = FilerPanelView(model: model)
        panel.frame = NSRect(x: 0, y: 0, width: 320, height: 560)
        model.follow(transfer ? FilerFixtures.tab(1, kind: .ssh, name: "prod-web") : FilerFixtures.tab(1))
        await model.waitUntilIdle()
        model.setSelection(paths: ["/Users/demo/projects"])
        if transfer {
            env.engine.extraEvents = [.progress(done: 4_200_000, total: 12_000_000)]
            // 进度停在运行中：用永不结束的批次不方便，直接展示一个运行中的状态。
            var status = FilerTransferStatus(kind: .upload, name: "backup.tar.gz", index: 2, count: 3, doneBytes: 4_200_000,
                                             totalBytes: 12_000_000, bytesPerSecond: 3_200_000)
            status.origin = "prod-web · /Users/demo"
            model.showExternalStatus(status)
        }
        panel.layoutSubtreeIfNeeded()
        return canvas(size: NSSize(width: 400, height: 640), content: panel, contentSize: panel.frame.size, retaining: model)
    }

    /// 两条顶部标签条（ui-shell.md §2.5）：上面是活动分屏（宽松、等宽取最大宽度），下面是非活动分屏（拥挤、缩到最小宽度）。
    private func topTabStripCanvas() -> NSView {
        func item(_ id: SessionID, _ kind: TabKind, _ name: String, cwd: String? = nil, branch: String? = nil) -> TabItem {
            var item = TabItem(id: id, kind: kind, name: name, connection: .connected)
            item.cwd = cwd
            item.gitBranch = branch
            return item
        }
        var running = item(3, .ssh, "Production")
        running.activity = .commandRunning
        var unread = item(4, .local, "zsh", cwd: "/Users/demo/Downloads")
        unread.unreadDone = true
        let activeRows: [TabRowView.Model] = [
            .init(item: item(1, .local, "zsh", cwd: "/Users/demo/workspace/kanso", branch: "main"), sidebarIndex: 0, selection: .windowActive),
            .init(item: item(2, .sftp, "Backup Server"), sidebarIndex: 1, selection: .none),
            .init(item: running, sidebarIndex: 2, selection: .none),
            .init(item: unread, sidebarIndex: 3, selection: .none),
        ]
        let crowdedRows: [TabRowView.Model] = (5...12).map { id in
            .init(item: item(SessionID(id), .local, "zsh", cwd: "/srv/app/service-\(id)"), sidebarIndex: id - 1, selection: id == 6 ? .paneActive : .none)
        }
        let width: CGFloat = 900
        let container = NSView(frame: NSRect(x: 0, y: 0, width: width, height: TabStripView.height * 2 + 12))
        let top = TabStripView(pane: PaneID(raw: 1))
        let bottom = TabStripView(pane: PaneID(raw: 2))
        // 容器不是 flipped：先放的在上方。
        top.frame = NSRect(x: 0, y: TabStripView.height + 12, width: width, height: TabStripView.height)
        bottom.frame = NSRect(x: 0, y: 0, width: width, height: TabStripView.height)
        for strip in [top, bottom] { container.addSubview(strip) }
        top.update(activeRows, activeTab: 1)
        bottom.update(crowdedRows, activeTab: 6)
        container.layoutSubtreeIfNeeded()
        return canvas(size: NSSize(width: width + 80, height: container.frame.height + 80), content: container, contentSize: container.frame.size, retaining: top)
    }

    private func gitCanvas() async -> NSView {
        let model = GitPanelModel(engine: FakeGitEngine())
        let panel = GitPanelView(model: model)
        panel.frame = NSRect(x: 0, y: 0, width: 420, height: 620)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        model.toggle(GitFixtures.files[0].path)
        await model.waitUntilIdle()
        model.message = "完善本地仓库改动面板"
        panel.layoutSubtreeIfNeeded()
        return canvas(size: NSSize(width: 500, height: 700), content: panel, contentSize: panel.frame.size, retaining: model)
    }

    private func sftpDualCanvas() async -> NSView {
        let env = FakeEnv(), prompter = FakePrompter()
        let remote = FakeDataSource(remote: true), local = FakeDataSource(remote: false)
        remote.home = "/home/deploy"
        local.home = "/Users/demo"
        for source in [remote, local] {
            let entries: [FilerEntry] = [
                .dir(".config", in: source.home), .dir("Documents", in: source.home), .dir("projects", in: source.home),
                .file("README.md", in: source.home, size: 2048), .file("backup.tar.gz", in: source.home, size: 12_000_000),
                .file("report.pdf", in: source.home, size: 485_000), .link("current", in: source.home),
            ]
            source.dirs[source.home] = entries.enumerated().map { index, entry in
                var entry = entry
                entry.modified = Date(timeIntervalSince1970: 1_775_000_000 + TimeInterval(index) * 86_400)
                return entry
            }
        }
        env.sources[1] = remote
        env.localSource = local
        var tab = FilerFixtures.tab(1, kind: .sftp, name: "production")
        tab.userHost = "deploy@files.example.com"
        let model = SftpDualModel(tab: tab, env: env, prompter: prompter)
        let view = SftpDualView(model: model)
        await model.remote.waitUntilIdle()
        await model.local.waitUntilIdle()
        model.remote.setSelection(paths: ["/home/deploy/report.pdf"])
        model.local.setSelection(paths: ["/Users/demo/backup.tar.gz"])
        return canvas(size: NSSize(width: 1100, height: 660), content: view,
                      contentSize: NSSize(width: 1040, height: 600), retaining: model)
    }

    private func filerConflictCanvas() -> NSView {
        let request = FilerConflictRequest(itemName: "report.pdf", isDirectory: false, kind: .upload,
                                           destinationDescription: "prod-web · root@prod.example.com · /srv/reports")
        let content = FilerDialogContent.conflict(request) { _ in }
        return canvas(size: NSSize(width: content.contentSize.width + 80, height: content.contentSize.height + 80),
                      content: content.shell, contentSize: content.contentSize, retaining: content)
    }

    /// OpenSSH 配置导入对话框：4 个候选（1 个已保存、1 个带警告、1 个未勾选）+ 一条文件级警告。
    private func sshImportCanvas() -> NSView {
        let core = MockTransferCore()
        core.groupList = [FfiGroup(id: "g", name: "Prod")]
        let model = SshConfigImportModel(core: core, homeDirectory: "/Users/demo", languages: ["en"])
        model.apply(scan: TransferFixtures.scan)
        model.setSelected(false, at: 3)
        let dialog = SshConfigImportViewController(model: model, panels: MockPanels())
        dialog.maxHeight = 2000
        _ = dialog.view
        let size = dialog.preferredContentSize
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: dialog.view,
                      contentSize: size, retaining: dialog)
    }

    /// 编辑对话框（SSH 类型、新建）放在窗口底色上。
    private func editorCanvas() -> NSView {
        let model = SessionEditorModel(store: MemoryStore(groups: [FfiGroup(id: "g", name: "Prod")]), channel: "Direct")
        model.kind = .ssh
        model.name = "ssh"
        model.color = SessionColorPresets.all[13]
        let editor = SessionEditorViewController(model: model)
        editor.maxHeight = 2000
        _ = editor.view
        let size = editor.preferredContentSize
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: editor.view,
                      contentSize: size, retaining: editor)
    }

    /// 命令面板：四类结果各若干条，空查询全显示。
    private func paletteCanvas() -> NSView {
        let model = CommandPaletteModel()
        model.replaceSnapshot(CommandPaletteSnapshot(
            tabs: [
                .init(id: 1, title: "~/demo", detail: "⌘1"),
                .init(id: 2, title: "prod-web", detail: "⌘2"),
            ],
            sessions: [
                .init(id: "s1", title: "db-primary", detail: "deploy@db1.internal:22", color: SessionColorPresets.all[3]),
                .init(id: "s2", title: "build-box", detail: "builder@ci.internal:60022", color: SessionColorPresets.all[1]),
            ],
            commands: [
                .init(id: "c1", title: "Find…", shortcut: "⌘F"),
                .init(id: "c2", title: "Toggle File Browser", shortcut: "⌥⌘→"),
                .init(id: "c3", title: "Split Right", shortcut: "⌘\\"),
            ],
            files: [
                .init(name: "main.swift", path: "/Users/demo/main.swift"),
                .init(name: "README.md", path: "/Users/demo/README.md"),
                .init(name: "build.sh", path: "/Users/demo/build.sh"),
            ]
        ))
        let palette = CommandPaletteViewController(model: model)
        _ = palette.view
        let size = palette.preferredContentSize
        let chrome = StyledBoxView()
        chrome.fillToken = DesignTokens.Color.bgPopover
        chrome.borderToken = DesignTokens.Color.borderStrong
        chrome.cornerRadius = DesignTokens.Radius.xl
        palette.view.frame = NSRect(origin: .zero, size: size)
        chrome.addSubview(palette.view)
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: chrome,
                      contentSize: size, retaining: palette)
    }

    /// 启动器：3 个会话、2 个分组（Prod / Dev），其中一个未分组。
    private func launcherCanvas() -> NSView {
        func profile(_ id: String, _ name: String, host: String, user: String, port: UInt16, color: FfiRgb?, group: String?) -> FfiProfile {
            let target = FfiSshTarget(host: host, port: port, user: user, auth: .password, privateKeyPath: nil, jump: nil, proxyCommand: nil, keepalive: .defaultKeepalive)
            return FfiProfile(id: id, name: name, color: color, group: group,
                              kind: .ssh(profile: FfiSshProfile(target: target, encoding: "", locale: nil)))
        }
        let store = MemoryStore(
            profiles: [
                profile("1", "jumper.example.com", host: "jumper.example.com", user: "alice", port: 2222,
                        color: SessionColorPresets.all[7], group: nil),
                profile("2", "db-primary", host: "db1.internal", user: "deploy", port: 22,
                        color: SessionColorPresets.all[3], group: "prod"),
                profile("3", "build-box", host: "ci.internal", user: "builder", port: 60022,
                        color: SessionColorPresets.all[1], group: "dev"),
            ],
            groups: [FfiGroup(id: "prod", name: "Prod"), FfiGroup(id: "dev", name: "Dev")]
        )
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { _ in }, edit: { _ in }, dismiss: {}, openSettings: {},
            withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(model: SessionLauncherModel(store: store), dialogs: AlertSessionDialogs(), actions: actions)
        _ = launcher.view
        let size = launcher.preferredContentSize
        // 面板的底板（与 FloatingPanel 内部一致）。
        let chrome = StyledBoxView()
        chrome.fillToken = DesignTokens.Color.bgPopover
        chrome.borderToken = DesignTokens.Color.borderStrong
        chrome.cornerRadius = DesignTokens.Radius.xl
        launcher.view.frame = NSRect(origin: .zero, size: size)
        chrome.addSubview(launcher.view)
        return canvas(size: NSSize(width: size.width + 80, height: size.height + 80), content: chrome,
                      contentSize: size, retaining: launcher)
    }

    private func canvas(size: NSSize, content: NSView, contentSize: NSSize, retaining owner: AnyObject) -> NSView {
        let root = RetainingBackdrop(frame: NSRect(origin: .zero, size: size), owner: owner)
        root.fillToken = DesignTokens.Color.bgWindow
        root.cornerRadius = 0
        content.frame = NSRect(x: 40, y: 40, width: contentSize.width, height: contentSize.height)
        root.addSubview(content)
        return root
    }

    // MARK: 渲染与比较

    private func verify(_ canvas: NSView, _ appearance: NSAppearance.Name, _ name: String) throws {
        let image = try render(canvas, appearance: appearance)
        let url = SnapshotTests.snapshotDirectory.appendingPathComponent("\(name).png")
        if ProcessInfo.processInfo.environment["KANSO_RECORD_SNAPSHOTS"] == "1" {
            try FileManager.default.createDirectory(at: SnapshotTests.snapshotDirectory, withIntermediateDirectories: true)
            try SnapshotTests.writePNG(image, to: url)
            print("已录制参考截图：\(url.path)")
            return
        }
        guard FileManager.default.fileExists(atPath: url.path) else {
            XCTFail("缺少参考截图 \(url.path)；用 KANSO_RECORD_SNAPSHOTS=1 录制")
            return
        }
        let reference = try XCTUnwrap(SnapshotTests.loadPNG(url), "无法读取参考截图 \(url.path)")
        let result = try SnapshotTests.compare(image, reference)
        if !result.passed {
            let actual = FileManager.default.temporaryDirectory.appendingPathComponent("\(name)-actual.png")
            try? SnapshotTests.writePNG(image, to: actual)
            XCTFail("\(name) 与参考截图不一致：\(result.summary)；实际输出见 \(actual.path)")
        }
    }

    /// 把视图放进指定外观的离屏窗口，渲染成固定 2x 的 CGImage（输出尺寸不依赖运行机器的屏幕）。
    private func render(_ view: NSView, appearance: NSAppearance.Name) throws -> CGImage {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: view.frame.size), styleMask: [.borderless],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: appearance)
        window.contentView = view
        defer { window.close() }
        view.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        view.layoutSubtreeIfNeeded()

        let pixels = NSSize(width: view.bounds.width * Self.scale, height: view.bounds.height * Self.scale)
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(pixels.width), pixelsHigh: Int(pixels.height), bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0
        ))
        // size 是点尺寸，像素是它的 2 倍：cacheDisplay 按这个比例渲染。
        rep.size = view.bounds.size
        view.cacheDisplay(in: view.bounds, to: rep)
        return try XCTUnwrap(rep.cgImage)
    }
}

/// 截图用背景：顺带持有内容的控制器（视图本身不会强引用它，渲染期间不能被释放）。
@MainActor
private final class RetainingBackdrop: StyledBoxView {
    private let owner: AnyObject

    init(frame: NSRect, owner: AnyObject) {
        self.owner = owner
        super.init(frame: frame)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("RetainingBackdrop 只支持代码创建") }
}
