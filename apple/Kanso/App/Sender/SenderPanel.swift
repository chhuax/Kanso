import AppKit
import KansoBridge

@MainActor
final class SenderPanel: StyledBoxView, NSTextViewDelegate, NSTextFieldDelegate {
    let model: SenderModel
    let editor = SenderTextView()
    private let title = NSTextField(labelWithString: String(localized: "Sender"))
    private let ending = KPopUpField()
    private let targets = KPopUpField()
    private let name = KTextField()
    private let interval = KTextField()
    private let count = KTextField()
    private let intervalLabel = NSTextField(labelWithString: String(localized: "Interval / sec"))
    private let countLabel = NSTextField(labelWithString: String(localized: "Count · 0 unlimited"))
    private let save = KButton(title: String(localized: "Save…"))
    private let cancel = KButton(symbol: "xmark", style: .icon)
    private let clear = KButton(symbol: "trash", style: .icon)
    private let run = KButton(symbol: "play.fill", style: .primary)
    private let repeatButton = KButton(symbol: "repeat", style: .secondary)
    private let stopRepeat = KButton(title: String(localized: "Stop"), style: .secondary)
    private let runLabel = NSTextField(labelWithString: "")
    private let pageLabel = NSTextField(labelWithString: "")
    private let previous = KButton(symbol: "chevron.left", style: .icon)
    private let next = KButton(symbol: "chevron.right", style: .icon)
    private let empty = NSTextField(labelWithString: String(localized: "Loading commands…"))
    private let editorBox = StyledBoxView()
    private let editorScroll = NSScrollView()
    private let scroll = NSScrollView()
    private let content = StyledBoxView()
    private var rows: [KButton] = []
    private var displayedItems: [FfiSnippet] = []
    var onStateChange: (() -> Void)?

    init(model: SenderModel) {
        self.model = model
        super.init(frame: .zero)
        fillToken = DesignTokens.Color.bgSidebar
        content.fillToken = DesignTokens.Color.bgSidebar
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.documentView = content
        addSubview(scroll)
        for view in [title, ending, targets, editorBox, name, interval, count, intervalLabel, countLabel,
                     save, cancel, clear, run, repeatButton, stopRepeat, runLabel, pageLabel, previous, next, empty] {
            content.addSubview(view)
        }
        title.font = .systemFont(ofSize: 12, weight: .semibold)
        for label in [intervalLabel, countLabel, runLabel, pageLabel, empty] {
            label.font = .systemFont(ofSize: 11)
            label.textColor = .token(DesignTokens.Color.textSecondary)
            label.lineBreakMode = .byTruncatingTail
        }
        editorBox.fillToken = DesignTokens.Color.bgField
        editorBox.borderToken = DesignTokens.Color.borderField
        editorBox.cornerRadius = DesignTokens.Radius.md
        editorScroll.drawsBackground = false
        editorScroll.hasVerticalScroller = true
        editor.isRichText = false
        editor.isAutomaticQuoteSubstitutionEnabled = false
        editor.isAutomaticDashSubstitutionEnabled = false
        editor.isAutomaticTextReplacementEnabled = false
        editor.drawsBackground = false
        editor.isVerticallyResizable = true
        editor.textContainerInset = NSSize(width: 7, height: 4)
        editor.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        editor.delegate = self
        editor.onSend = { [weak model] in model?.start(repeating: false) }
        editor.onCancel = { [weak model] in model?.cancelEditing() }
        editorScroll.documentView = editor
        editorBox.addSubview(editorScroll)
        name.placeholder = String(localized: "Name (optional)")
        name.textField.delegate = self
        interval.textField.delegate = self
        count.textField.delegate = self
        for field in [interval, count] {
            let formatter = NumberFormatter()
            formatter.allowsFloats = false
            formatter.usesGroupingSeparator = false
            field.textField.formatter = formatter
        }
        interval.toolTip = String(localized: "Repeat interval: 1–86400 seconds, clamped when leaving the field")
        count.toolTip = String(localized: "Planned rounds: 0–100000, 0 means unlimited")
        ending.setItems([String(localized: "No line ending"), "LF", "CRLF"])
        ending.onChange = { [weak self] index in
            self?.model.draft.ending = [.none, .lf, .crLf][index]
        }
        targets.onChange = { [weak model] in model?.allTargets = $0 == 1 }
        clear.toolTip = String(localized: "Clear text")
        clear.onPress = { [weak self] in self?.model.draft.text = ""; self?.render() }
        cancel.toolTip = String(localized: "Cancel editing and restore draft (Esc)")
        cancel.onPress = { [weak model] in model?.cancelEditing() }
        run.onPress = { [weak model] in
            guard let model else { return }
            if model.manualRunning { model.stop(repeating: false) } else { model.start(repeating: false) }
        }
        repeatButton.toolTip = String(localized: "Send now and repeat")
        repeatButton.onPress = { [weak self] in self?.commitNumbers(); self?.model.start(repeating: true) }
        stopRepeat.onPress = { [weak model] in model?.stop(repeating: true) }
        save.onPress = { [weak self] in self?.chooseScope() }
        previous.onPress = { [weak self] in self?.model.page -= 1; self?.render() }
        next.onPress = { [weak self] in self?.model.page += 1; self?.render() }
        model.onChange = { [weak self] in self?.render(); self?.onStateChange?() }
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("SenderPanel 只支持代码创建") }

    func render() {
        if editor.string != model.draft.text { editor.string = model.draft.text }
        if name.stringValue != model.draft.name { name.stringValue = model.draft.name }
        if interval.textField.currentEditor() == nil { interval.stringValue = String(model.interval) }
        if count.textField.currentEditor() == nil { count.stringValue = String(model.count) }
        ending.selectItem(at: model.draft.ending == .none ? 0 : model.draft.ending == .lf ? 1 : 2)
        targets.setItems([String(localized: "Current session (\(model.currentCount))"), String(localized: "All sessions (\(model.connectedCount))")])
        targets.selectItem(at: model.allTargets ? 1 : 0)
        title.textColor = .token(DesignTokens.Color.textPrimary)
        editor.textColor = .token(DesignTokens.Color.textPrimary)
        editor.insertionPointColor = .token(DesignTokens.Color.accent)
        save.title = model.editing == nil ? String(localized: "Save…") : String(localized: "Update…")
        save.isEnabled = model.canSave
        save.toolTip = model.library.count >= 1000 && model.editing == nil ? String(localized: "The command library has reached its limit of 1000 commands") : String(localized: "Choose save location")
        cancel.isHidden = model.editing == nil
        run.symbol = model.manualRunning ? "stop.fill" : "play.fill"
        run.isEnabled = model.manualRunning || model.hasContent
        repeatButton.isEnabled = model.hasContent
        runLabel.isHidden = !model.repeatRunning
        stopRepeat.isHidden = !model.repeatRunning
        runLabel.stringValue = String(localized: "↻ \(model.runningTitle) · \(model.runningTarget) · every \(model.runningInterval) seconds · \(model.progress)")
        runLabel.toolTip = runLabel.stringValue
        pageLabel.stringValue = String(localized: "\(model.visible.count) commands · \(model.page + 1) / \(model.pageCount)")
        previous.isEnabled = model.page > 0
        next.isEnabled = model.page + 1 < model.pageCount
        empty.stringValue = model.isLoading ? String(localized: "Loading commands…") : model.loadError.map { String(localized: "Failed to load: \($0)") } ?? (model.library.isEmpty ? String(localized: "No commands yet · Save frequently used commands to send them here") : String(localized: "No commands available for the current session"))
        empty.isHidden = !model.pageItems.isEmpty && model.loadError == nil
        if displayedItems != model.pageItems {
            rows.forEach { $0.removeFromSuperview() }
            displayedItems = model.pageItems
            rows = displayedItems.map { snippet in
                let button = KButton(title: snippet.name)
                button.toolTip = "\(snippet.text)\n\(model.scopeTitle(snippet.scope))"
                button.onPress = { [weak model] in model?.start(repeating: false, snippet: snippet) }
                button.contextMenuProvider = { [weak self] in self?.contextMenu(for: snippet) ?? NSMenu() }
                content.addSubview(button)
                return button
            }
        }
        for (button, snippet) in zip(rows, displayedItems) {
            button.isEnabled = !model.manualRunning && model.loadError == nil && !snippet.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        needsLayout = true
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        render()
    }

    override func layout() {
        super.layout()
        scroll.frame = bounds
        let width = max(240, scroll.contentSize.width)
        let compact = width < 650
        let header: CGFloat = width < 440 ? 64 : 32
        title.frame = NSRect(x: 12, y: 5, width: 65, height: 22)
        ending.frame = NSRect(x: width < 440 ? 12 : width - 304, y: width < 440 ? 32 : 3, width: 112, height: 26)
        targets.frame = NSRect(x: width < 440 ? 132 : width - 184, y: width < 440 ? 32 : 3, width: 172, height: 26)
        let lines = model.draft.text.components(separatedBy: "\n").count
        let editorHeight = min(88, max(30, CGFloat(lines) * 16 + 10))
        editorBox.frame = NSRect(x: 12, y: header, width: width - 136, height: editorHeight)
        editorScroll.frame = editorBox.bounds.insetBy(dx: 1, dy: 1)
        editor.frame.size.width = editorScroll.contentSize.width
        editor.textContainer?.containerSize = NSSize(width: editorScroll.contentSize.width, height: .greatestFiniteMagnitude)
        clear.frame = NSRect(x: width - 118, y: header + 2, width: 26, height: 26)
        run.frame = NSRect(x: width - 88, y: header, width: 34, height: 30)
        repeatButton.frame = NSRect(x: width - 48, y: header, width: 36, height: 30)
        var offset = header + editorHeight + 6
        let nameWidth = compact ? width - 138 : width - 486
        name.frame = NSRect(x: 12, y: offset, width: max(60, nameWidth), height: 28)
        save.frame = NSRect(x: width - 112, y: offset, width: 72, height: 28)
        cancel.frame = NSRect(x: width - 36, y: offset + 1, width: 24, height: 26)
        if compact { offset += 34 }
        let settingsX: CGFloat = compact ? 12 : width - 466
        intervalLabel.frame = NSRect(x: settingsX, y: offset + 6, width: 56, height: 18)
        interval.frame = NSRect(x: settingsX + 59, y: offset, width: 64, height: 28)
        countLabel.frame = NSRect(x: settingsX + 133, y: offset + 6, width: 82, height: 18)
        count.frame = NSRect(x: settingsX + 218, y: offset, width: 76, height: 28)
        offset += 34
        if model.repeatRunning {
            runLabel.frame = NSRect(x: 12, y: offset + 5, width: width - 90, height: 20)
            stopRepeat.frame = NSRect(x: width - 72, y: offset, width: 60, height: 28)
            offset += 34
        }
        pageLabel.frame = NSRect(x: 12, y: offset + 3, width: width - 85, height: 18)
        previous.frame = NSRect(x: width - 68, y: offset - 2, width: 24, height: 24)
        next.frame = NSRect(x: width - 38, y: offset - 2, width: 24, height: 24)
        offset += 26
        empty.frame = NSRect(x: 12, y: offset, width: width - 24, height: 24)
        let columns = max(1, Int((width - 16) / 150))
        let itemWidth = (width - 24 - CGFloat(columns - 1) * 6) / CGFloat(columns)
        for (index, row) in rows.enumerated() {
            row.frame = NSRect(x: 12 + CGFloat(index % columns) * (itemWidth + 6), y: offset + CGFloat(index / columns) * 32, width: itemWidth, height: 26)
        }
        content.frame = NSRect(x: 0, y: 0, width: width, height: max(bounds.height, offset + CGFloat(max(1, (rows.count + columns - 1) / columns)) * 32 + 8))
    }

    func textDidChange(_ notification: Notification) {
        model.draft.text = editor.string
        render()
    }

    func controlTextDidChange(_ notification: Notification) {
        if notification.object as? NSTextField === name.textField {
            model.draft.name = String(name.stringValue.prefix(80))
            if name.stringValue != model.draft.name { name.stringValue = model.draft.name }
        }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        if notification.object as? NSTextField === interval.textField || notification.object as? NSTextField === count.textField { commitNumbers() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.cancelOperation(_:)), model.editing != nil { model.cancelEditing(); return true }
        if control === name.textField, selector == #selector(NSResponder.insertNewline(_:)), !textView.hasMarkedText() { chooseScope(); return true }
        return false
    }

    private func commitNumbers() {
        model.interval = Int(interval.stringValue) ?? 60
        model.count = Int(count.stringValue) ?? 0
        model.clampRepeatValues()
        interval.stringValue = String(model.interval)
        count.stringValue = String(model.count)
    }

    private func chooseScope() {
        guard model.canSave else { return }
        let menu = NSMenu(title: String(localized: "Save location"))
        for (index, scope) in model.scopes.enumerated() {
            menu.addItem(ActionMenuItem(scope.title, checked: index == model.defaultScopeIndex) { [weak model] in model?.save(scopeIndex: index) })
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: save.bounds.maxY), in: save)
    }

    func contextMenu(for snippet: FfiSnippet) -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.addItem(ActionMenuItem(String(localized: "Edit")) { [weak model] in model?.edit(snippet) })
        let repeatItem = ActionMenuItem(String(localized: "Repeat send")) { [weak self] in self?.commitNumbers(); self?.model.start(repeating: true, snippet: snippet) }
        repeatItem.isEnabled = !snippet.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        menu.addItem(repeatItem)
        let endingMenu = NSMenu()
        for (label, ending) in [(String(localized: "No line ending"), FfiLineEnding.none), ("LF", .lf), ("CRLF", .crLf)] {
            endingMenu.addItem(ActionMenuItem(label, checked: snippet.lineEnding == ending) { [weak model] in
                var updated = snippet; updated.lineEnding = ending; model?.update(updated)
            })
        }
        let endingItem = NSMenuItem(title: String(localized: "Line ending"), action: nil, keyEquivalent: "")
        endingItem.isEnabled = !model.isSaving && !snippet.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        endingItem.submenu = endingMenu
        menu.addItem(endingItem)
        let scopeMenu = NSMenu()
        for scope in model.scopes {
            scopeMenu.addItem(ActionMenuItem(scope.title, checked: snippet.scope == scope.scope) { [weak model] in
                var updated = snippet; updated.scope = scope.scope; model?.update(updated)
            })
        }
        let scopeItem = NSMenuItem(title: String(localized: "Scope"), action: nil, keyEquivalent: "")
        scopeItem.isEnabled = endingItem.isEnabled
        scopeItem.submenu = scopeMenu
        menu.addItem(scopeItem)
        menu.addItem(.separator())
        menu.addItem(ActionMenuItem(String(localized: "Delete")) { [weak model] in model?.delete(snippet) })
        return menu
    }
}

@MainActor
final class SenderTextView: NSTextView {
    var onSend: (() -> Void)?
    var onCancel: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 {
            switch SenderModel.enterAction(shift: event.modifierFlags.contains(.shift), marked: hasMarkedText()) {
            case .send: onSend?()
            case .newline: insertNewline(nil)
            case .inputMethod: super.keyDown(with: event)
            }
        } else { super.keyDown(with: event) }
    }

    override func cancelOperation(_ sender: Any?) { onCancel?() }
}

