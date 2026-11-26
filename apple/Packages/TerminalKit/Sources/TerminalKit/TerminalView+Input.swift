import AppKit

/// 一次按键的处理上下文：把输入法的回调与触发它的原始 `NSEvent` 关联起来。
///
/// 每次 `keyDown` 新建一个，不跨按键共享任何状态。这是 terminal.md §16 场景 1（中文输入法英文模式下
/// 快速连按，第二个键不能丢）的核心：旧实现的问题是依赖"上一次按键已经处理完"这类全局状态，
/// 事件重叠时后一个键的回调被前一个键的状态吞掉。这里回调只看自己所属的事件。
@MainActor
final class KeyDispatch {
    let event: NSEvent
    /// 输入法是否已经针对这个事件回调过（insertText / setMarkedText / doCommand）。
    /// 用来防止同一个事件被输出两次：例如空格确认词组时 insertText 已经带上了结果，
    /// 空格本身绝不能再作为按键发出（§16 场景 3）。
    var callbackFired = false

    init(event: NSEvent) { self.event = event }
}

// MARK: - 键盘

extension TerminalView {
    public override func keyDown(with event: NSEvent) {
        noteUserActivityForBlink()
        // 宿主认领的组合键（用户绑定的命令）先于一切处理，包括输入法。
        if keyInterceptor?(event) == true { return }
        if markedString.isEmpty, completionKeyInterceptor?(event) == true { return }
        onInputActivity?()
        let mods = KeyModifiers(event.modifierFlags)
        // ⌘ 组合键归菜单（terminal.md §9）：能走到这里说明菜单没有认领它，也不发给 shell。
        // 故意不调用 super：默认实现会沿响应链上传并发出系统提示音。
        if mods.contains(.meta) { return }

        let hadMarkedText = !markedString.isEmpty

        // 普通 Ctrl+字母永远不拦截（terminal.md §9）。NSTextInputContext 的键位绑定会把 Ctrl+A / Ctrl+E / Ctrl+K
        // 等翻成 moveToBeginningOfLine: 之类的 Emacs 编辑命令，输入法也可能吞掉它们；
        // 没有组字时直接按键事件发出，保证 shell 收到控制字符。组字期间仍交给输入法（场景 6 的同类约束）。
        if mods.contains(.ctrl) && !hadMarkedText {
            _ = emitKey(event)
            return
        }

        let dispatch = KeyDispatch(event: event)
        // 保存并恢复而不是置 nil：输入法回调里理论上可能再触发事件，嵌套时外层上下文不能丢。
        let outer = currentDispatch
        currentDispatch = dispatch
        defer { currentDispatch = outer }

        let handled = routeToInputMethod(event)
        if dispatch.callbackFired || handled { return }
        // 输入法没有处理、也没有任何回调。组字期间的按键无论如何不能漏给 shell：
        // 回车、退格、Esc 此时属于输入法（场景 6）。
        if hadMarkedText { return }
        // 输入法没有认领的键（功能键、没有输入源参与的按键等）按普通按键发出。
        _ = emitKey(event)
    }

    public override func keyUp(with event: NSEvent) {
        // 只为"已经作为按键事件发出过 press"的键发 release：被输入法消耗的按键（组字、上屏）对 core 来说
        // 从没按下过，单独的 release 会让 kitty 协议的状态错位。
        guard pressedKeyCodes.remove(event.keyCode) != nil else { return }
        if let key = TerminalKeyEvent(event: event, kind: .release) {
            inputSink?.key(key)
        }
    }

    /// 修饰键变化。转给输入法（有的输入法靠单击 Shift 切换中英文，吞掉它会让切换失效），
    /// 但**不清除任何状态**：按住 Shift 输入的第一个全角标点不能丢（§16 场景 2），
    /// 旧实现的问题之一就是把 Shift 的 flagsChanged 当成了"重置"信号。
    public override func flagsChanged(with event: NSEvent) {
        _ = routeToInputMethod(event)
    }

    /// 把按键转成 `TerminalKeyEvent` 发给 sink；返回是否发出。
    @discardableResult
    func emitKey(_ event: NSEvent) -> Bool {
        let kind: KeyEventKind = event.isARepeat ? .repeat : .press
        guard let key = TerminalKeyEvent(event: event, kind: kind) else { return false }
        pressedKeyCodes.insert(event.keyCode)
        inputSink?.key(key)
        return true
    }

    func routeToInputMethod(_ event: NSEvent) -> Bool {
        if let inputMethod { return inputMethod.handleEvent(event) }
        return inputContext?.handleEvent(event) ?? false
    }

    func discardInputMethodMarkedText() {
        if let inputMethod {
            inputMethod.discardMarkedText()
        } else {
            inputContext?.discardMarkedText()
        }
    }
}

// MARK: - 焦点

extension TerminalView {
    public override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        // 此时 window.firstResponder 还没有切到自己，所以只看窗口是否是 key。
        if accepted {
            setFocus(window?.isKeyWindow ?? true)
            onBecomeFirstResponder?()
        }
        return accepted
    }

    public override func resignFirstResponder() -> Bool {
        let accepted = super.resignFirstResponder()
        if accepted {
            // 组字中失焦：丢弃，而不是让它在别的视图里"上屏"。丢弃不发送任何内容（场景 5）。
            if !markedString.isEmpty { discardInputMethodMarkedText() }
            clearMarkedText()
            pressedKeyCodes.removeAll()
            endAllDrags()
            setFocus(false)
        }
        return accepted
    }

    func windowKeyStateChanged() {
        let isKey = window?.isKeyWindow ?? false
        // 窗口失去 key 状态时松开事件可能被别的窗口收走，拖动状态必须在这里结束（terminal.md §8.1）。
        if !isKey { endAllDrags() }
        setFocus(isKey && window?.firstResponder === self)
        updateBlinking()
    }

    /// 去重后上报焦点：becomeFirstResponder 与窗口 key 通知会先后触发，sink 只应看到真实变化。
    func setFocus(_ focused: Bool) {
        guard focused != reportedFocus else { return }
        reportedFocus = focused
        inputSink?.focus(focused)
    }
}

// MARK: - 鼠标

extension TerminalView {
    public override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let mouseTrackingArea { removeTrackingArea(mouseTrackingArea) }
        // .inVisibleRect 让系统在视图尺寸变化时自动维护区域，不必在 layout 里重建。
        let area = NSTrackingArea(
            rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self, userInfo: nil
        )
        addTrackingArea(area)
        mouseTrackingArea = area
    }

    public override func mouseDown(with event: NSEvent) { pointerDown(event, button: .left) }
    public override func mouseDragged(with event: NSEvent) { pointerDragged(event, button: .left) }
    public override func mouseUp(with event: NSEvent) { pointerUp(event, button: .left) }

    // 右键：程序开启鼠标上报且未按 Shift 时交给程序（点击不看设置）；否则按 `rightClickShowsMenu` 二选一（terminal.md §8.1）：
    // 弹菜单（先选中指针下的单词），或"复制或粘贴"（有选区复制、没有就粘贴，不选词、不弹菜单）。
    // 这里不调用 super，避免触发 NSView 的 menu(for:)。
    public override func rightMouseDown(with event: NSEvent) {
        if !isMouseCaptured(by: event) {
            if !rightClickShowsMenu {
                // 不发给 core：drag / up 由下面的 activeButtons 判断一并屏蔽。
                // 先抢焦点：点的是别的分屏时，内容进了这个会话，接下来的键盘输入也应该跟过来。
                window?.makeFirstResponder(self)
                if editActions?.hasSelection == true {
                    copySelectionIfPresent()
                    // 复制后清掉选区，下一次右键才能粘贴（与 Windows 控制台一致）。
                    // 只在这条路径清：⌘C 与选中即复制都保留选区。
                    editActions?.clearSelection()
                } else {
                    paste(nil)
                }
                return
            }
            if let menu = contextMenuProvider?() {
                if let point = viewportPoint(for: event) {
                    editActions?.selectWord(at: point)
                }
                addCommandBlockMenuItems(to: menu, event: event)
                window?.makeFirstResponder(self)
                NSMenu.popUpContextMenu(menu, with: event, for: self)
                return
            }
        }
        pointerDown(event, button: .right)
    }
    // drag / up 只在 down 确实交给了 core（activeButtons 有记录）时才转发：被终端自己消费的点击
    // （复制或粘贴、中键粘贴 / 无动作）不能给 core 留下孤立的 drag / up。
    public override func rightMouseDragged(with event: NSEvent) {
        if activeButtons[.right] != nil { pointerDragged(event, button: .right) }
    }
    public override func rightMouseUp(with event: NSEvent) {
        if activeButtons[.right] != nil { pointerUp(event, button: .right) }
    }

    // 只处理中键（buttonNumber == 2）；其他附加键没有定义，忽略。
    // 非上报模式（或按住 Shift）下由终端处理：粘贴或无动作，都不发给 core（terminal.md §8.1）。
    public override func otherMouseDown(with event: NSEvent) {
        guard event.buttonNumber == 2 else { return }
        if isMouseCaptured(by: event) {
            pointerDown(event, button: .middle)
        } else if middleClickPastes {
            window?.makeFirstResponder(self)
            paste(nil)
        }
    }
    public override func otherMouseDragged(with event: NSEvent) {
        if event.buttonNumber == 2, activeButtons[.middle] != nil { pointerDragged(event, button: .middle) }
    }
    public override func otherMouseUp(with event: NSEvent) {
        if event.buttonNumber == 2, activeButtons[.middle] != nil { pointerUp(event, button: .middle) }
    }

    /// 程序开启了鼠标上报且没有按住 Shift：点击交给程序，不看右键 / 中键设置。
    private func isMouseCaptured(by event: NSEvent) -> Bool {
        currentFrame?.flags.contains(.mouseReporting) == true && !KeyModifiers(event.modifierFlags).contains(.shift)
    }

    public override func mouseMoved(with event: NSEvent) {
        if currentFrame?.flags.contains(.mouseReporting) != true, foldableBlock(at: event) != nil {
            NSCursor.pointingHand.set()
            return
        }
        guard let point = viewportPoint(for: event) else { return }
        inputSink?.mouse(TerminalMouseEvent(
            kind: .move, button: .none, point: point, mods: KeyModifiers(event.modifierFlags), clickCount: 0
        ))
    }

    private func pointerDown(_ event: NSEvent, button: TerminalMouseButton) {
        if button == .left {
            if event.clickCount == 1, !isMouseCaptured(by: event), let block = foldableBlock(at: event) {
                window?.makeFirstResponder(self)
                _ = editActions?.toggleCommandBlockFold(id: block.id)
                return
            }
            leftDragIsSelection = currentFrame?.flags.contains(.mouseReporting) != true
                || KeyModifiers(event.modifierFlags).contains(.shift)
            clickedDirectoryChip = event.clickCount == 1 ? directoryChip(at: event) : nil
        }
        guard let point = viewportPoint(for: event) else { return }
        // 拖动不能"粘住"（terminal.md §8.1）：如果上一次按下没有等到松开（松开事件被别的窗口、菜单或
        // 系统手势收走），先补发一个 Up 结束它，core 才不会把新的 Down 当成拖动的延续。
        if let stale = activeButtons[button] {
            var up = stale
            up.kind = .up
            inputSink?.mouse(up)
        }
        let down = TerminalMouseEvent(
            kind: .down, button: button, point: point, mods: KeyModifiers(event.modifierFlags),
            clickCount: UInt8(clamping: max(1, event.clickCount))
        )
        activeButtons[button] = down
        inputSink?.mouse(down)
    }

    private func pointerDragged(_ event: NSEvent, button: TerminalMouseButton) {
        if button == .left {
            clickedDirectoryChip = nil
        }
        guard let point = viewportPoint(for: event) else { return }
        let drag = TerminalMouseEvent(
            kind: .drag, button: button, point: point, mods: KeyModifiers(event.modifierFlags),
            clickCount: UInt8(clamping: max(1, event.clickCount))
        )
        // 记录最新位置：补发 Up 时落在最后已知的位置，选区不会跳。
        activeButtons[button] = drag
        inputSink?.mouse(drag)
    }

    private func pointerUp(_ event: NSEvent, button: TerminalMouseButton) {
        var directoryAnchor: NSRect?
        let shouldAutoCopy = button == .left && copyOnSelect && leftDragIsSelection && activeButtons[button] != nil
        if button == .left {
            leftDragIsSelection = false
            if activeButtons[button]?.kind == .down, event.clickCount == 1,
               currentFrame?.flags.contains(.mouseReporting) != true {
                if let anchor = directoryChip(at: event), anchor == clickedDirectoryChip {
                    directoryAnchor = anchor
                }
            }
            clickedDirectoryChip = nil
        }
        activeButtons[button] = nil
        guard let point = viewportPoint(for: event) else { return }
        inputSink?.mouse(TerminalMouseEvent(
            kind: .up, button: button, point: point, mods: KeyModifiers(event.modifierFlags),
            clickCount: UInt8(clamping: max(1, event.clickCount))
        ))
        // 选中即复制（terminal.md §8.2）：松开鼠标完成选择后，只有确实产生了非空文本选区才复制。
        if shouldAutoCopy, editActions?.hasSelection == true {
            copySelectionIfPresent()
        }
        if let directoryAnchor { onDirectoryChipClick?(directoryAnchor) }
    }

    /// 结束所有进行中的拖动：对每个未松开的按键补发 Up。失去 first responder、窗口失去 key 状态、
    /// 视图离开窗口时调用。
    func endAllDrags() {
        leftDragIsSelection = false
        guard !activeButtons.isEmpty else { return }
        let pending = activeButtons
        activeButtons.removeAll()
        for (_, last) in pending {
            var up = last
            up.kind = .up
            inputSink?.mouse(up)
        }
    }

    /// 点击位置换算成视口单元格，见 `TerminalHitGeometry.viewportPoint`。
    func viewportPoint(for event: NSEvent) -> ViewportPointValue? {
        let p = convert(event.locationInWindow, from: nil)
        // AppKit 视图坐标原点在左下，命中测试几何以左上为原点。
        return hitGeometry()?.viewportPoint(x: Double(p.x), y: Double(bounds.height - p.y))
    }

    /// 当前布局的命中测试几何（pt）。时间戳栏宽度、留白、顶部让位都已包含在 origin 里，与渲染器同源，
    /// 所以不会出现"画在这里、点击算在那里"的偏差。视图还没有有效网格时为 nil。
    func hitGeometry() -> TerminalHitGeometry? {
        guard let renderer, let grid = gridSize else { return nil }
        let scale = currentScaleForInput
        renderer.topInset = safeAreaInsets.top
        let l = renderer.layout(scale: scale)
        return TerminalHitGeometry(
            originX: Double(l.originX) / scale, originY: Double(l.originY) / scale,
            cellWidth: Double(l.metrics.cellWidth) / scale, cellHeight: Double(l.metrics.cellHeight) / scale,
            cols: grid.cols, rows: grid.rows
        )
    }

    var currentScaleForInput: CGFloat {
        window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2
    }
}

// MARK: - 滚轮

extension TerminalView {
    /// 滚轮 / 触控板滚动。
    ///
    /// - 方向：直接使用 `scrollingDeltaY` 的符号。系统已经按用户的"自然滚动"设置翻转过它，
    ///   这里再翻转一次就会与系统设置相反。
    /// - 触控板（`hasPreciseScrollingDeltas`）：增量是像素，累积到一个单元格高度才发一行；
    ///   鼠标滚轮：增量已经是行。动量阶段的事件照常累积，惯性滚动才会自然减速到停。
    public override func scrollWheel(with event: NSEvent) {
        guard let geometry = hitGeometry(), let point = viewportPoint(for: event) else { return }
        // 一次新的触摸手势开始时丢弃上一次遗留的零头，避免第一下就多跳一行。
        if event.phase == .began || event.phase == .mayBegin { scrollAccumulator.reset() }

        let delta = Double(event.scrollingDeltaY)
        let unit = event.hasPreciseScrollingDeltas ? geometry.cellHeight : 1
        let lines = scrollAccumulator.add(delta: delta, unit: unit)
        guard lines != 0 else { return }
        inputSink?.scroll(lines: lines, at: point, mods: KeyModifiers(event.modifierFlags))
    }
}
