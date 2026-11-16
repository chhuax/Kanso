import AppKit
import KansoUI

/// 小结果对话框：一张"结果"卡片 + "完成"。用于 `.kanso` 数据导入结果（新增 / 更新 / 跳过及原因）与导出成功提示。
/// 与其他对话框同一外壳；没有取消语义，Esc / 回车 / × 都只是关闭。
@MainActor
final class DataResultViewController: NSViewController {
    var onFinish: (() -> Void)?

    static let width: CGFloat = 480

    let maxHeight: CGFloat
    let doneButton = KButton(title: String(localized: "Done"), style: .primary)
    private let dialogTitle: String
    private let symbol: String
    let content: ImportResultContent
    private(set) var shell: DialogShellView!

    init(title: String, symbol: String, content: ImportResultContent, maxHeight: CGFloat = 680) {
        dialogTitle = title
        self.symbol = symbol
        self.content = content
        self.maxHeight = maxHeight
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("DataResultViewController 只支持代码创建") }

    override func loadView() {
        let shell = DialogShellView(title: dialogTitle, symbol: symbol, width: Self.width)
        self.shell = shell
        view = shell
        let card = KCard.result(content, width: Self.width)
        card.translatesAutoresizingMaskIntoConstraints = false
        shell.bodyStack.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: shell.bodyStack.widthAnchor).isActive = true
        doneButton.keyEquivalent = "\r"
        doneButton.onPress = { [weak self] in self?.onFinish?() }
        shell.setFooterButtons([doneButton])
        shell.onClose = { [weak self] in self?.onFinish?() }
        preferredContentSize = NSSize(width: Self.width, height: shell.fittingHeight(maxHeight: maxHeight))
    }
}
