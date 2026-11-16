import AppKit

/// 快捷键录制框：显示一个组合键，点击后进入录制，按下新组合即回报。
///
/// 录制规则（ui-shell.md §7）：Esc 取消录制；Delete / Backspace 清空（不绑定任何键）；其余键连同修饰键作为新组合。
/// 不带修饰键的普通字母等"无效组合"也会回报，由使用方决定提示什么（这里不判断绑定规则，只负责读键）。
///
/// 为什么重写 `performKeyEquivalent` 而不只靠 `keyDown`：⌘ 开头的组合会先被主菜单的快捷键匹配
/// （⌘N 会真的新建会话、⌘W 会关窗口）。录制期间必须在最前面截住所有按键，才能把它们当作"要绑定的组合"。
@MainActor
final class KeyChordField: StyledBoxView {
    enum Event: Equatable {
        case chord(KeyChord)
        case clear
        case cancel
    }

    private let label = PassthroughLabel(labelWithString: "")

    /// 当前显示的组合；nil 显示"未设置"。
    var chord: KeyChord? {
        didSet { refresh() }
    }

    /// 固定文字（⌘1–⌘9 这类不可改的行）：设置后直接显示它，忽略 `chord`。
    var fixedText: String? {
        didSet { refresh() }
    }

    /// 是否已禁用（固定不可改的行）。
    var isEnabled = true {
        didSet {
            alphaValue = isEnabled ? 1 : 0.6
            if !isEnabled { cancelRecording() }
        }
    }

    private(set) var isRecording = false {
        didSet { refresh() }
    }

    /// 录制到事件。使用方处理后通常再设置 `chord`。
    var onEvent: ((Event) -> Void)?
    /// 点击开始录制时回调（让使用方取消其他行的录制 / 清提示）。
    var onBeginRecording: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        cornerRadius = DesignTokens.Radius.md
        label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        label.alignment = .center
        label.lineBreakMode = .byTruncatingTail
        addSubview(label)
        setAccessibilityRole(.button)
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("KeyChordField 只支持代码创建") }

    override var intrinsicContentSize: NSSize {
        NSSize(width: 148, height: DesignTokens.Spacing.fieldHeight)
    }

    override var acceptsFirstResponder: Bool { isEnabled }

    private func refresh() {
        if let fixedText {
            label.stringValue = fixedText
            label.textColor = .token(DesignTokens.Color.textSecondary)
            fillToken = DesignTokens.Color.bgField
            borderToken = DesignTokens.Color.borderField
        } else if isRecording {
            label.stringValue = String(localized: "Press shortcut…")
            label.textColor = .token(DesignTokens.Color.textSecondary)
            fillToken = DesignTokens.Color.bgField
            borderToken = DesignTokens.Color.accent
        } else if let chord {
            label.stringValue = chord.display
            label.textColor = .token(DesignTokens.Color.textPrimary)
            fillToken = DesignTokens.Color.bgButtonSecondary
            borderToken = DesignTokens.Color.borderStrong
        } else {
            label.stringValue = String(localized: "Not Set")
            label.textColor = .token(DesignTokens.Color.textTertiary)
            fillToken = DesignTokens.Color.bgButtonSecondary
            borderToken = DesignTokens.Color.borderStrong
        }
        setAccessibilityValue(label.stringValue)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let h = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: 8, y: (bounds.height - h) / 2, width: max(0, bounds.width - 16), height: h)
    }

    // MARK: 录制

    /// 开始录制。点击与键盘（空格 / 回车）都走这里。
    func beginRecording() {
        guard isEnabled, !isRecording else { return }
        onBeginRecording?()
        isRecording = true
        window?.makeFirstResponder(self)
    }

    func cancelRecording() {
        isRecording = false
    }

    override func mouseDown(with event: NSEvent) {
        if isRecording {
            cancelRecording()
        } else {
            beginRecording()
        }
    }

    override func resignFirstResponder() -> Bool {
        // 焦点移走（点了别处、切换窗口）等同取消，避免界面停在"录制中"而按键已不再被截获。
        isRecording = false
        return true
    }

    /// 录制期间截住一切按键（含 ⌘ 组合，见类注释）。返回 true 表示已消费，菜单不再看到它。
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard isRecording, event.type == .keyDown else { return super.performKeyEquivalent(with: event) }
        handle(event)
        return true
    }

    override func keyDown(with event: NSEvent) {
        guard isRecording else {
            // 未录制时空格 / 回车开始录制，其余交给上层（Tab 切换焦点）。
            switch event.charactersIgnoringModifiers {
            case " ", "\r": beginRecording()
            default: super.keyDown(with: event)
            }
            return
        }
        handle(event)
    }

    /// 处理一次录制按键。internal 供测试直接驱动。
    func handle(_ event: NSEvent) {
        let mods = ChordModifiers(event.modifierFlags)
        // Esc / Delete 只有在不带修饰键时才是控制键：⌘⌫、⌃⌫ 等有人想绑定，它们按普通组合处理。
        if mods.isEmpty {
            switch event.keyCode {
            case 53:  // Esc
                isRecording = false
                onEvent?(.cancel)
                return
            case 51, 117:  // Delete（退格）、Forward Delete
                isRecording = false
                onEvent?(.clear)
                return
            default:
                break
            }
        }
        guard let chord = KeyChord(event: event) else { return }
        isRecording = false
        onEvent?(.chord(chord))
    }
}
