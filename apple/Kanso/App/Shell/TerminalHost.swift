import AppKit
import KansoBridge
import KansoUI
import TerminalKit

/// 一个会话对应的终端视图及其输入输出链路。
///
/// `TerminalView` 对 `inputSink` / `editActions` 是弱引用，所以必须由 host 强持有这些对象；
/// 视图实例在标签存活期间一直复用（跨分屏移动、切换标签都不重建），这样回滚缓冲与选区才会保留。
@MainActor
final class TerminalHost {
    let session: SessionID
    let view: TerminalView
    let frameSource: CoreFrameSource
    /// 复制 / 粘贴 / 全选的出口（同时是输入出口的底层实现）。
    let editActions: CoreInputSink
    /// 实际交给视图的输入出口。DEBUG 下经 `LoggingInputSink` 记日志再转发给 core；
    /// Release 直接用 `editActions`。
    let inputSink: TerminalInputSink
    /// 本会话的查找栏（每个会话一个，各自保留查询与开关）。装在终端视图里，
    /// 所以会随视图一起切换标签、跨分屏移动，不需要另外管理显隐。
    let findBar: FindBarController
    let completion: CompletionController
    let directoryPicker: DirectoryPickerController

    /// - Parameter keybindings: 快捷键改绑；传入后接到视图的按键拦截点（用户绑定的组合键不漏给 shell，
    ///   普通 Ctrl+字母绑定为复制键时的特殊规则也在那里，terminal.md §8.2）。
    init(core: KansoCore, session: SessionID, frame: CGRect, keybindings: KeybindingController? = nil, remote: Bool = false) {
        self.session = session
        view = TerminalView(frame: frame)
        PerformanceProbe.mark("terminal-view")
        frameSource = CoreFrameSource(core: core, session: session)
        let coreSink = CoreInputSink(core: core, session: session)
        editActions = coreSink
        #if DEBUG
        inputSink = LoggingInputSink(forwardingTo: coreSink)
        #else
        inputSink = coreSink
        #endif
        view.frameSource = frameSource
        view.inputSink = inputSink
        view.editActions = editActions
        completion = CompletionController(view: view, sink: inputSink,
            reader: CompletionSourceReader.live(core: core, session: session, remote: remote,
                environment: ProcessInfo.processInfo.environment,
                localHosts: FilerModel.localHostNames),
            state: { core.completionState(sessionId: session) },
            compute: { core.complete(sessionId: session, input: $0, sources: $1) },
            plan: { core.completionPlan(sessionId: session, input: $0, sources: $1) })
        PerformanceProbe.mark("completion")
        directoryPicker = DirectoryPickerController(terminal: view, sink: inputSink,
            workingDirectory: { try await core.workingDirectory(sessionId: session) })
        if !remote {
            view.onDirectoryChipClick = { [weak directoryPicker, weak completion] anchor in
                completion?.invalidate()
                directoryPicker?.present(anchor: anchor)
            }
        }
        view.completionKeyInterceptor = { [weak completion] in completion?.intercept($0) ?? false }
        view.onInputActivity = { [weak completion] in completion?.inputChanged() }
        view.onFrameChanged = { [weak completion, weak directoryPicker] in
            completion?.refresh()
            directoryPicker?.refresh()
        }
        PerformanceProbe.observe(terminal: view)
        if let keybindings {
            // 弱引用：视图不该延长改绑控制器（与应用同寿命）的使用范围，也避免测试里的循环引用。
            view.keyInterceptor = { [weak keybindings, weak view] event in
                keybindings?.interceptTerminalKey(event, in: view) ?? false
            }
        }
        // 右键菜单：编辑 / 查找 / 清屏 / 拆分 / 在 Filer 中显示工作目录 / 关闭会话。
        // 定位目录带发起标签的 id，右键点的不是活动标签时也作用于被点的这个（filer.md §2：锁定发起时的标签）。
        view.contextMenuProvider = { [weak view, weak keybindings, session] in
            guard let view else { return nil }
            return Self.makeContextMenu(view: view, session: session, keybindings: keybindings)
        }
        findBar = FindBarController(target: CoreSearchTarget(core: core, session: session))
        findBar.install(in: view)
        // Esc / 关闭按钮后把键盘焦点还给终端。
        findBar.onDismiss = { [weak view] in
            guard let view else { return }
            view.window?.makeFirstResponder(view)
        }
    }

    /// 构造终端右键菜单：带 SF Symbol 图标与当前生效快捷键。
    static func makeContextMenu(view: TerminalView, session: SessionID, keybindings: KeybindingController?) -> NSMenu {
        let menu = NSMenu()

        @discardableResult
        func add(_ command: KeyCommand, symbol: String, action: Selector? = nil, target: AnyObject? = nil) -> NSMenuItem {
            let chord = keybindings.map { $0.chord(for: command) } ?? command.defaultChord
            let item = menu.addItem(
                withTitle: command.title,
                action: action ?? command.action,
                keyEquivalent: chord?.menuKeyEquivalent ?? ""
            )
            item.keyEquivalentModifierMask = chord?.modifiers.flags ?? []
            item.image = .symbol(symbol)
            item.target = target
            return item
        }

        add(.copy, symbol: "doc.on.doc", action: #selector(TerminalView.copy(_:)), target: view)
        add(.paste, symbol: "doc.on.clipboard", action: #selector(TerminalView.paste(_:)), target: view)
        add(.selectAll, symbol: "selection.pin.in.out", action: #selector(TerminalView.selectAll(_:)), target: view)
        add(.find, symbol: "magnifyingglass")
        add(.clearBuffer, symbol: "eraser")
        menu.addItem(.separator())
        add(.splitRight, symbol: "rectangle.split.2x1")
        add(.splitDown, symbol: "rectangle.split.1x2")
        let locate = add(.locateWorkingDirectory, symbol: "folder")
        locate.representedObject = session
        let tailSeparator = NSMenuItem.separator()
        tailSeparator.identifier = TerminalView.commandBlockMenuAnchor
        menu.addItem(tailSeparator)
        add(.closeSession, symbol: "xmark")
        return menu
    }
}
