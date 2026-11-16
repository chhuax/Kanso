import AppKit
import KansoBridge
import KansoUI

/// 「导入 OpenSSH 配置」对话框的内容（visual-style §3.3，结构与会话编辑对话框一致）。
///
/// 结构：对话框外壳（头部 / 可滚动主体 / 底部按钮）> "配置文件"卡片（路径 + 选择…）、提示条（文件级警告 / 扫描失败）、
/// "主机"卡片（全选 / 全不选、可滚动候选列表、分组下拉）。底部：取消、导入（主按钮）。
/// 导入完成后在原对话框里把主体换成"结果"卡片，底部只剩"完成"：不再叠第二个 sheet
/// （上一个 sheet 还在收起时立刻弹新 sheet，时序上不可靠）。逻辑都在 `SshConfigImportModel`，这里只管控件与布局。
@MainActor
final class SshConfigImportViewController: NSViewController {
    /// 对话框关闭（取消 / 完成）时调用；由呈现方负责收起 sheet。
    var onFinish: (() -> Void)?
    /// 导入成功（core 已写入）后调用一次：呈现方据此刷新依赖会话数据的界面。晚于结果页显示。
    var onImported: (() -> Void)?

    static let width: CGFloat = 560

    /// 对话框允许的最大高度（呈现方按主窗口高度设置）；主体内容更高时主体滚动。
    var maxHeight: CGFloat = 680 {
        didSet { if isViewLoaded { resizeSheet() } }
    }

    let model: SshConfigImportModel
    private let panels: DataFilePanels

    let pathField = KTextField()
    let chooseButton = KButton(title: String(localized: "Choose…"), style: .secondary)
    let selectAllButton = KButton(title: String(localized: "Select All"), style: .icon)
    let selectNoneButton = KButton(title: String(localized: "Select None"), style: .icon)
    let groupPopup = KPopUpField()
    let cancelButton = KButton(title: String(localized: "Cancel"), style: .secondary)
    let importButton = KButton(title: String(localized: "Import"), symbol: "square.and.arrow.down", style: .primary)
    let doneButton = KButton(title: String(localized: "Done"), style: .primary)
    let errorBanner = NoticeBannerView(symbol: "xmark.octagon", tintToken: DesignTokens.Color.statusError)
    let warningBanner = NoticeBannerView(symbol: "exclamationmark.triangle", tintToken: DesignTokens.Color.statusWarning)
    let list = CandidateListView()

    private(set) var shell: DialogShellView!
    private let pathCard = KCard(title: String(localized: "Config File"), symbol: "doc.text")
    private let hostsCard = KCard(title: String(localized: "Hosts"), symbol: "server.rack")
    private let countLabel = NSTextField(labelWithString: "")
    private let emptyLabel = NSTextField(wrappingLabelWithString: String(localized: "No importable hosts found in this file."))
    private var resultCard: KCard?
    /// 最近一次扫描任务；只为让测试能等待结果，界面代码不需要关心。
    private(set) var scanTask: Task<Void, Never>?

    init(model: SshConfigImportModel, panels: DataFilePanels) {
        self.model = model
        self.panels = panels
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SshConfigImportViewController 只支持代码创建") }

    override func loadView() {
        let shell = DialogShellView(title: String(localized: "Import OpenSSH Config"), symbol: "arrow.down.doc", width: Self.width)
        self.shell = shell
        view = shell
        buildControls()
        for v in [pathCard, errorBanner, warningBanner, hostsCard] as [NSView] {
            shell.bodyStack.addArrangedSubview(v)
            v.translatesAutoresizingMaskIntoConstraints = false
            v.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        }
        shell.setFooterButtons([cancelButton, importButton])
        shell.onClose = { [weak self] in self?.finish() }
        refresh()
    }

    /// 开始首次扫描。由呈现方在对话框显示后调用：加载视图本身不触发扫描，
    /// 这样截图 / 视图测试可以先给模型塞确定的数据。
    @discardableResult
    func startScan() -> Task<Void, Never> {
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            await model.scan()
            refresh()
        }
        scanTask = task
        return task
    }

    // MARK: - 构建

    private func buildControls() {
        pathField.textField.isEditable = false
        pathField.textField.isSelectable = true
        chooseButton.setContentHuggingPriority(.required, for: .horizontal)
        chooseButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        chooseButton.onPress = { [weak self] in self?.chooseConfig() }
        let pathRow = NSStackView(views: [pathField, chooseButton])
        pathRow.orientation = .horizontal
        pathRow.alignment = .centerY
        pathRow.spacing = DesignTokens.Spacing.sm
        pathCard.addRow(pathRow)

        selectAllButton.onPress = { [weak self] in self?.selectAll() }
        selectNoneButton.onPress = { [weak self] in self?.selectNone() }
        countLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        countLabel.textColor = .token(DesignTokens.Color.textSecondary)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(249), for: .horizontal)
        let toolbar = NSStackView(views: [selectAllButton, selectNoneButton, spacer, countLabel])
        toolbar.orientation = .horizontal
        toolbar.alignment = .centerY
        toolbar.spacing = DesignTokens.Spacing.xs
        hostsCard.addRow(toolbar)
        hostsCard.addRow(list)
        emptyLabel.font = .systemFont(ofSize: DesignTokens.Font.uiCaption)
        emptyLabel.textColor = .token(DesignTokens.Color.textTertiary)
        hostsCard.addRow(emptyLabel)

        groupPopup.setItems([String(localized: "No Group")] + model.groups.map(\.name))
        groupPopup.onChange = { [weak self] index in
            guard let self else { return }
            model.groupID = index == 0 ? nil : model.groups[index - 1].id
        }
        hostsCard.addRow(LabeledField(title: String(localized: "Group"), control: groupPopup))

        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.onPress = { [weak self] in self?.finish() }
        importButton.keyEquivalent = "\r"
        importButton.onPress = { [weak self] in self?.performImport() }
        doneButton.keyEquivalent = "\r"
        doneButton.onPress = { [weak self] in self?.finish() }
    }

    // MARK: - 刷新

    /// 按模型整体刷新（扫描完成、换文件、导入完成后）。勾选变化只走 `refreshSelection`，避免重建列表丢掉滚动位置。
    func refresh() {
        guard isViewLoaded else { return }
        if let result = model.result {
            showResult(result)
            return
        }
        pathField.stringValue = model.displayPath
        pathField.toolTip = model.configPath
        errorBanner.setText(model.scanError ?? "")
        warningBanner.setText(model.fileWarnings.joined(separator: "\n"))
        // 扫描失败时没有候选可选，整张主机卡片隐藏；路径卡片留着，用户可以换文件。
        hostsCard.isHidden = model.scanError != nil
        let views = model.rows.enumerated().map { index, row in
            let rowView = CandidateRowView(row: row)
            rowView.onToggle = { [weak self] in self?.toggle(index) }
            return rowView
        }
        list.setRows(views)
        list.isHidden = views.isEmpty
        emptyLabel.isHidden = !views.isEmpty
        groupPopup.selectItem(at: model.groupID.flatMap { id in model.groups.firstIndex { $0.id == id } }.map { $0 + 1 } ?? 0)
        refreshSelection()
    }

    private func refreshSelection() {
        for (view, row) in zip(list.rowViews, model.rows) { view.setChecked(row.isSelected) }
        countLabel.stringValue = String(localized: "\(model.selectedCount) of \(model.rows.count) selected")
        let hasRows = !model.rows.isEmpty
        selectAllButton.isEnabled = hasRows
        selectNoneButton.isEnabled = hasRows
        importButton.isEnabled = model.canImport
        shell.setMessage(model.importError)
        resizeSheet()
    }

    private func showResult(_ result: ImportResultContent) {
        for v in [pathCard, errorBanner, warningBanner, hostsCard] as [NSView] { v.isHidden = true }
        if resultCard == nil {
            let card = KCard.result(result, width: Self.width)
            card.translatesAutoresizingMaskIntoConstraints = false
            shell.bodyStack.addArrangedSubview(card)
            card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
            resultCard = card
        }
        shell.setFooterButtons([doneButton])
        shell.setMessage(nil)
        resizeSheet()
    }

    /// 显隐内容后按内容重新计算对话框高度；宽度固定。
    private func resizeSheet() {
        let size = NSSize(width: Self.width, height: shell.fittingHeight(maxHeight: maxHeight))
        preferredContentSize = size
        if let window = view.window as? DialogSheetWindow {
            window.resize(toContentSize: size)
        } else {
            view.window?.setContentSize(size)
        }
    }

    // MARK: - 动作

    private func finish() { onFinish?() }

    func toggle(_ index: Int) {
        model.toggle(at: index)
        refreshSelection()
    }

    func selectAll() {
        model.selectAll()
        refreshSelection()
    }

    func selectNone() {
        model.selectNone()
        refreshSelection()
    }

    /// 选择其他配置文件并重新扫描。返回的 Task 只为让测试能等待结果。
    @discardableResult
    func chooseConfig() -> Task<Void, Never> {
        let task = Task { @MainActor [weak self] in
            guard let self, let url = await panels.chooseSshConfig(startingAt: model.sshDirectory, window: view.window) else { return }
            await model.choose(path: url.path)
            refresh()
        }
        scanTask = task
        return task
    }

    /// 导入已勾选的主机。返回的 Task 只为让测试能等待结果。
    @discardableResult
    func performImport() -> Task<Void, Never> {
        Task { @MainActor [weak self] in
            guard let self, model.canImport else { return }
            // 导入是异步的（core 写盘）：先禁用按钮，避免重复点击产生重复导入。
            importButton.isEnabled = false
            await model.runImport()
            refresh()
            if model.result != nil { onImported?() }
        }
    }
}
