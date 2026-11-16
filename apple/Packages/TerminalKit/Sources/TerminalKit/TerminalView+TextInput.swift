import AppKit

// 输入法接入（NSTextInputClient）。
//
// 整体思路：终端没有可编辑文档，所以"文档"只有输入法正在组字的那一小段（marked text）。
// 1. 输入法的回调都发生在某个按键的 `handleEvent` 期间，`currentDispatch` 把回调与原始 NSEvent 关联；
// 2. 回调的输出分两类：直接由该键产生的字符 → `key`（带 text，让 Ctrl/Alt 仍走按键编码）；
//    组字结束上屏的文字 → `textInput`（不经按键编码）；
// 3. 组字内容只用覆盖层显示，`setMarkedText` / `unmarkText` 不向 sink 发任何东西，
//    文字只在 `insertText` 时发送一次，已上屏的内容不会被重复发送（terminal.md §16 场景 5）。

extension TerminalView: @preconcurrency NSTextInputClient {
    public func insertText(_ string: Any, replacementRange: NSRange) {
        let text = Self.plainString(string)
        // 先记下"之前有没有组字"再清除：insertText 按 Apple 文档语义会替换并结束 marked text。
        let hadMarkedText = !markedString.isEmpty
        clearMarkedText()
        guard !text.isEmpty else { return }
        onInputActivity?()

        let dispatch = currentDispatch
        // 不能用"去重最近发送的文本"之类的办法防重复：两次组字极短时间内先后完成时内容可能相同，
        // 去重会丢掉合法的第二次（§16 场景 4）。防重复靠的是"每个事件只输出一次"，不是比较内容。
        let alreadyFired = dispatch?.callbackFired ?? false
        dispatch?.callbackFired = true

        // 文本就是该键直接产生的字符，且没有经过组字：发按键事件，Ctrl/Alt 组合、kitty 协议才能拿到物理键
        // （场景 1：英文直接输入、中文输入法英文模式下的字母都走这条路径，且只与自己的事件关联）。
        if let dispatch, !hadMarkedText, !alreadyFired, dispatch.event.type == .keyDown,
           text == dispatch.event.characters, emitKey(dispatch.event) {
            return
        }
        // 其余都是输入法给的文字：组字结束上屏（空格 / 回车 / 数字键选词，场景 3、4）、
        // Shift 输入的全角标点（场景 2，文字与按键字符不同）、异步上屏（没有当前事件）。
        // 空格确认词组时空格键本身已被这次 insertText 消耗，不会再作为按键发出。
        inputSink?.textInput(text)
    }

    public func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        currentDispatch?.callbackFired = true
        let text = Self.plainString(string)
        markedString = text
        onInputActivity?()
        let length = (text as NSString).length
        if selectedRange.location == NSNotFound {
            markedSelection = NSRange(location: length, length: 0)
        } else {
            let location = min(selectedRange.location, length)
            markedSelection = NSRange(location: location, length: min(selectedRange.length, length - location))
        }
        // 只更新显示，不发送（场景 5）。
        updateMarkedOverlay()
        inputContext?.invalidateCharacterCoordinates()
    }

    /// 结束组字：按 Apple 文档的语义，把**仍未上屏的组字内容**作为文字提交。
    ///
    /// 有的输入法只靠 `unmarkText` 提交（例如组字中途切走焦点、点击别处），这里不发就会丢字。
    /// 不会造成重复（场景 5）：`insertText` 上屏前已经先清空了组字，
    /// 所以常见的"insertText 之后再 unmarkText"序列走到这里时组字为空，什么也不发。
    /// 不要改成"无条件不发送"，也不要改成"无条件把组字再发一遍"。
    public func unmarkText() {
        currentDispatch?.callbackFired = true
        let pending = markedString
        clearMarkedText()
        if !pending.isEmpty {
            onInputActivity?()
            inputSink?.textInput(pending)
        }
    }

    public func hasMarkedText() -> Bool { !markedString.isEmpty }

    public func markedRange() -> NSRange {
        markedString.isEmpty
            ? NSRange(location: NSNotFound, length: 0)
            : NSRange(location: 0, length: (markedString as NSString).length)
    }

    /// 终端没有可编辑文档：选区是位于组字末尾的空范围（没有组字时是文档起点）。
    public func selectedRange() -> NSRange {
        NSRange(location: (markedString as NSString).length, length: 0)
    }

    /// 输入法按语义不需要读取文档内容。
    public func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        nil
    }

    /// 不接受输入法给组字文本附加的属性：覆盖层统一用终端字体，选中段由 `selectedRange` 决定。
    public func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }

    /// 候选框位置（场景 7）：返回光标所在单元格加上组字文本宽度偏移的**屏幕坐标**。
    ///
    /// 偏移取 `range.location` 之前的组字文本占的列数：输入法通常传组字起点，候选框贴着组字文本左端；
    /// 传组字内某位置时（如多段转换）则跟着移动。没有窗口时返回视图坐标，调用方（测试）自行判断。
    public func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        actualRange?.pointee = range
        let viewRect = candidateRect(forCharacterLocation: range.location)
        guard let window else { return viewRect }
        return window.convertToScreen(convert(viewRect, to: nil))
    }

    public func characterIndex(for point: NSPoint) -> Int { NSNotFound }

    /// `insertNewline:`、`deleteBackward:`、`moveLeft:` 等：表示"输入法没有吃掉这个键"。
    ///
    /// 不依据 selector 名称推断是哪个键，而是用触发回调的原始 NSEvent 构造 `TerminalKeyEvent`：
    /// 同一个键在不同输入源 / 键位绑定下会映射到不同的 selector，按名称推断一定有遗漏。
    public override func doCommand(by selector: Selector) {
        guard let dispatch = currentDispatch, !dispatch.callbackFired else { return }
        dispatch.callbackFired = true
        // 组字期间的回车、退格、Esc 属于输入法，绝不发给 shell（场景 6）。
        if !markedString.isEmpty { return }
        emitKey(dispatch.event)
    }

    // MARK: 组字状态与覆盖层

    func clearMarkedText() {
        guard !markedString.isEmpty else { return }
        markedString = ""
        markedSelection = NSRange(location: 0, length: 0)
        updateMarkedOverlay()
    }

    /// 同步覆盖层的内容与位置。光标移动（新帧）、布局变化、组字变化时调用。
    func updateMarkedOverlay() {
        guard !markedString.isEmpty, let frame = markedTextFrame() else {
            markedOverlay.isHidden = true
            return
        }
        markedOverlay.font = font
        markedOverlay.palette = currentPalette
        markedOverlay.text = markedString
        markedOverlay.selection = markedSelection
        markedOverlay.frame = frame
        markedOverlay.isHidden = false
        markedOverlay.needsDisplay = true
    }

    /// 组字覆盖层在视图坐标（原点左下）中的位置：起点是光标单元格，宽度是组字文本占的列数。
    ///
    /// 组字文本过长会超出行尾：整体向左平移到刚好贴住视图右边缘，而不是被截断——
    /// 用户正在输入的内容必须完整可见。仍放不下时（比视图还宽）从左边缘开始，由视图裁剪。
    func markedTextFrame() -> NSRect? {
        guard let geometry = hitGeometry() else { return nil }
        let cursor = cursorCell ?? (0, 0)
        let origin = geometry.cellOrigin(col: cursor.col, row: cursor.row)
        let width = Double(CellWidth.columns(of: markedString)) * geometry.cellWidth
        let x = max(0, min(origin.x, Double(bounds.width) - width))
        return NSRect(x: x, y: Double(bounds.height) - origin.y - geometry.cellHeight, width: width, height: geometry.cellHeight)
    }

    /// 候选框锚点矩形（视图坐标）。
    func candidateRect(forCharacterLocation location: Int) -> NSRect {
        guard let geometry = hitGeometry() else { return NSRect(origin: .zero, size: NSSize(width: 1, height: 1)) }
        if let frame = markedTextFrame() {
            let ns = markedString as NSString
            let clamped = location == NSNotFound ? 0 : max(0, min(location, ns.length))
            let offset = Double(CellWidth.columns(of: ns.substring(to: clamped))) * geometry.cellWidth
            return NSRect(x: frame.minX + offset, y: frame.minY, width: geometry.cellWidth, height: geometry.cellHeight)
        }
        let cursor = cursorCell ?? (0, 0)
        let origin = geometry.cellOrigin(col: cursor.col, row: cursor.row)
        return NSRect(
            x: origin.x, y: Double(bounds.height) - origin.y - geometry.cellHeight,
            width: geometry.cellWidth, height: geometry.cellHeight
        )
    }

    private static func plainString(_ value: Any) -> String {
        if let s = value as? String { return s }
        if let a = value as? NSAttributedString { return a.string }
        return ""
    }
}
