import AppKit
import KansoBridge
import KansoUI

/// 右侧服务器监控面板（docs/spec/host-monitor.md）。
@MainActor
final class MonitorPanelView: StyledBoxView {
    let model: MonitorPanelModel
    var onClose: (() -> Void)?

    private let titleLabel = NSTextField(labelWithString: String(localized: "Server Monitor"))
    private let closeButton = KButton(symbol: "xmark", style: .icon)
    private let errorLabel = NSTextField(wrappingLabelWithString: "")
    private let emptyLabel = NSTextField(wrappingLabelWithString: "")
    private let scroll = NSScrollView()
    private let document = FlippedStackHost()
    private let contentStack = NSStackView()

    init(model: MonitorPanelModel) {
        self.model = model
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgPanel
        cornerRadius = 0

        titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        closeButton.toolTip = String(localized: "Close Monitor")
        closeButton.setAccessibilityLabel(String(localized: "Close Monitor"))
        closeButton.onPress = { [weak self] in self?.onClose?() }

        errorLabel.font = .systemFont(ofSize: 11)
        errorLabel.textColor = .systemRed
        emptyLabel.font = .systemFont(ofSize: 12)
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center

        contentStack.orientation = .vertical
        contentStack.alignment = .leading
        contentStack.spacing = 14
        contentStack.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(contentStack)
        NSLayoutConstraint.activate([
            contentStack.topAnchor.constraint(equalTo: document.topAnchor, constant: 8),
            contentStack.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: 16),
            contentStack.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -16),
            contentStack.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -16),
        ])

        scroll.documentView = document
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false

        for view in [titleLabel, closeButton, errorLabel, emptyLabel, scroll] {
            addSubview(view)
        }

        let previous = model.onChange
        model.onChange = { [weak self] in
            previous?()
            self?.reload()
        }
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("MonitorPanelView 只支持代码创建") }

    private func reload() {
        for view in contentStack.arrangedSubviews {
            contentStack.removeArrangedSubview(view)
            view.removeFromSuperview()
        }

        errorLabel.stringValue = model.error ?? ""
        errorLabel.isHidden = model.error == nil

        if model.targetSession == nil {
            emptyLabel.stringValue = String(localized: "Connect an SSH session to see CPU, memory, disks, and network.")
            emptyLabel.isHidden = false
            scroll.isHidden = true
            needsLayout = true
            return
        }

        guard let snap = model.snapshot else {
            emptyLabel.stringValue = model.isSampling ? String(localized: "Sampling…") : ""
            emptyLabel.isHidden = model.error != nil || !model.isSampling
            scroll.isHidden = true
            needsLayout = true
            return
        }

        emptyLabel.isHidden = true
        scroll.isHidden = false

        addSection(title: String(localized: "Load"), views: [
            monoLabel(String(format: "%.2f   %.2f   %.2f", snap.load1, snap.load5, snap.load15), size: 13, primary: true),
        ])

        let cpuText = snap.cpuPercent.map { "\($0)%" } ?? "—"
        addSection(title: String(localized: "CPU"), views: [
            metricBarRow(text: cpuText, fraction: snap.cpuPercent.map { Double($0) / 100.0 } ?? 0),
        ])

        let memPct = snap.memTotal > 0 ? Int((Double(snap.memUsed) / Double(snap.memTotal) * 100).rounded()) : 0
        let memText = "\(MonitorPanelModel.formatBytes(snap.memUsed)) / \(MonitorPanelModel.formatBytes(snap.memTotal)) (\(memPct)%)"
        addSection(title: String(localized: "Memory"), views: [
            metricBarRow(text: memText, fraction: snap.memTotal > 0 ? Double(snap.memUsed) / Double(snap.memTotal) : 0),
        ])

        if snap.swapTotal > 0 {
            let swapPct = Int((Double(snap.swapUsed) / Double(snap.swapTotal) * 100).rounded())
            let swapText = "\(MonitorPanelModel.formatBytes(snap.swapUsed)) / \(MonitorPanelModel.formatBytes(snap.swapTotal)) (\(swapPct)%)"
            addSection(title: String(localized: "Swap"), views: [
                metricBarRow(text: swapText, fraction: Double(snap.swapUsed) / Double(snap.swapTotal)),
            ])
        }

        if !snap.disks.isEmpty {
            let diskViews = snap.disks.map { disk -> NSView in
                let header = rowHeader(
                    left: disk.mount,
                    right: "\(MonitorPanelModel.formatBytes(disk.used)) / \(MonitorPanelModel.formatBytes(disk.size)) (\(disk.percent)%)"
                )
                return stack([header, ProgressStrip(fraction: Double(disk.percent) / 100.0)], spacing: 4)
            }
            addSection(title: String(localized: "Disks"), views: diskViews)
        }

        if !snap.nets.isEmpty {
            let netViews = snap.nets.map { net -> NSView in
                var rateText = ""
                if let rx = net.rxPerSec, let tx = net.txPerSec {
                    rateText = "↓ \(MonitorPanelModel.formatRate(rx))  ↑ \(MonitorPanelModel.formatRate(tx))"
                }
                let header = rowHeader(left: net.iface, right: rateText)
                let total = monoLabel(
                    "↓ \(MonitorPanelModel.formatBytes(net.rx))   ↑ \(MonitorPanelModel.formatBytes(net.tx))",
                    size: 11,
                    primary: false
                )
                return stack([header, total], spacing: 2)
            }
            addSection(title: String(localized: "Network"), views: netViews)
        }

        needsLayout = true
    }

    private func addSection(title: String, views: [NSView]) {
        let heading = NSTextField(labelWithString: title)
        heading.font = .systemFont(ofSize: 11, weight: .semibold)
        heading.textColor = .secondaryLabelColor
        let group = stack([heading] + views, spacing: 6)
        contentStack.addArrangedSubview(group)
        group.widthAnchor.constraint(equalTo: contentStack.widthAnchor).isActive = true
    }

    private func stack(_ views: [NSView], spacing: CGFloat) -> NSStackView {
        let s = NSStackView(views: views)
        s.orientation = .vertical
        s.alignment = .leading
        s.spacing = spacing
        s.translatesAutoresizingMaskIntoConstraints = false
        for v in views {
            v.translatesAutoresizingMaskIntoConstraints = false
            v.widthAnchor.constraint(equalTo: s.widthAnchor).isActive = true
        }
        return s
    }

    private func metricBarRow(text: String, fraction: Double) -> NSView {
        stack([monoLabel(text, size: 12, primary: true), ProgressStrip(fraction: fraction)], spacing: 5)
    }

    private func rowHeader(left: String, right: String) -> NSView {
        let row = NSStackView()
        row.orientation = .horizontal
        row.alignment = .firstBaseline
        row.spacing = 8
        let lhs = monoLabel(left, size: 12, primary: true)
        lhs.lineBreakMode = .byTruncatingMiddle
        lhs.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let rhs = monoLabel(right, size: 11, primary: false)
        rhs.alignment = .right
        rhs.setContentHuggingPriority(.required, for: .horizontal)
        rhs.setContentCompressionResistancePriority(.required, for: .horizontal)
        row.addArrangedSubview(lhs)
        row.addArrangedSubview(rhs)
        return row
    }

    private func monoLabel(_ text: String, size: CGFloat, primary: Bool) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .monospacedDigitSystemFont(ofSize: size, weight: primary ? .medium : .regular)
        label.textColor = primary ? .labelColor : .secondaryLabelColor
        return label
    }

    override func layout() {
        super.layout()
        let width = bounds.width
        let height = bounds.height
        titleLabel.frame = NSRect(x: 16, y: 12, width: max(0, width - 64), height: 20)
        closeButton.frame = NSRect(x: width - 36, y: 10, width: 24, height: 24)
        var top: CGFloat = 44
        if !errorLabel.isHidden {
            let errSize = errorLabel.sizeThatFits(NSSize(width: max(40, width - 32), height: 200))
            errorLabel.frame = NSRect(x: 16, y: top, width: max(0, width - 32), height: errSize.height)
            top += errSize.height + 10
        }
        emptyLabel.frame = NSRect(x: 24, y: top + 36, width: max(0, width - 48), height: 60)
        scroll.frame = NSRect(x: 0, y: top, width: width, height: max(0, height - top))
        let docWidth = max(0, scroll.contentSize.width)
        document.frame.size.width = docWidth
        document.layoutSubtreeIfNeeded()
        let neededHeight = max(scroll.contentSize.height, contentStack.fittingSize.height + 24)
        document.frame = NSRect(x: 0, y: 0, width: docWidth, height: neededHeight)
    }
}

private final class FlippedStackHost: NSView {
    override var isFlipped: Bool { true }
}

private final class ProgressStrip: NSView {
    private let fraction: Double

    init(fraction: Double) {
        self.fraction = min(max(fraction, 0), 1)
        super.init(frame: .zero)
        wantsLayer = true
        heightAnchor.constraint(equalToConstant: 4).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("ProgressStrip 只支持代码创建") }

    override func draw(_ dirtyRect: NSRect) {
        let track = NSBezierPath(roundedRect: bounds, xRadius: 2, yRadius: 2)
        NSColor.token(DesignTokens.Color.borderSubtle).setFill()
        track.fill()
        let fillW = bounds.width * fraction
        if fillW > 0 {
            let bar = NSBezierPath(roundedRect: NSRect(x: 0, y: 0, width: fillW, height: bounds.height), xRadius: 2, yRadius: 2)
            NSColor.token(DesignTokens.Color.accent).setFill()
            bar.fill()
        }
    }
}
