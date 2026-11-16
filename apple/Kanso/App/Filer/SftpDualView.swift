import AppKit

@MainActor
final class SftpDualView: StyledBoxView {
    let model: SftpDualModel
    let remotePanel: FilerPanelView
    let localPanel: FilerPanelView
    private let upload = KButton(symbol: "arrow.left", style: .secondary)
    private let download = KButton(symbol: "arrow.right", style: .secondary)
    private let dividerLeft = StyledBoxView()
    private let dividerRight = StyledBoxView()

    init(model: SftpDualModel, prompter: SystemFilerPrompter? = nil) {
        self.model = model
        remotePanel = FilerPanelView(model: model.remote)
        localPanel = FilerPanelView(model: model.local)
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgWindow
        cornerRadius = 0
        upload.toolTip = String(localized: "Upload")
        download.toolTip = String(localized: "Download")
        upload.onPress = { [weak model] in model?.transfer(from: .local) }
        download.onPress = { [weak model] in model?.transfer(from: .remote) }
        for (panel, side) in [(remotePanel, SftpSide.remote), (localPanel, .local)] {
            panel.prompter = prompter
            panel.dualDragWriter = { [weak model] in model?.dragItem($0, from: side) }
            panel.dualDropVerdict = { [weak model] in
                model?.dropVerdict(SftpDualModel.drags(on: $0), onto: side) ?? .ignore
            }
            panel.dualDrop = { [weak model] in model?.performDrop(SftpDualModel.drags(on: $0), onto: side) ?? false }
        }
        for divider in [dividerLeft, dividerRight] {
            divider.fillToken = DesignTokens.Color.borderSubtle
            divider.cornerRadius = 0
        }
        for view in [remotePanel, localPanel, dividerLeft, dividerRight, upload, download] { addSubview(view) }
        model.onChange = { [weak self] in self?.reload() }
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SftpDualView 只支持代码创建") }

    private func reload() {
        remotePanel.connectionText = model.connectionText
        localPanel.connectionText = model.connectionText
        upload.isEnabled = model.canTransfer(from: .local)
        download.isEnabled = model.canTransfer(from: .remote)
    }

    override func layout() {
        super.layout()
        let rail: CGFloat = 48
        let width = max(0, (bounds.width - rail) / 2)
        remotePanel.frame = NSRect(x: 0, y: 0, width: width, height: bounds.height)
        localPanel.frame = NSRect(x: width + rail, y: 0, width: width, height: bounds.height)
        dividerLeft.frame = NSRect(x: width, y: 0, width: 1, height: bounds.height)
        dividerRight.frame = NSRect(x: width + rail - 1, y: 0, width: 1, height: bounds.height)
        upload.frame = NSRect(x: width + 8, y: bounds.midY - 40, width: 32, height: 32)
        download.frame = NSRect(x: width + 8, y: bounds.midY + 8, width: 32, height: 32)
    }
}
