import AppKit
import KansoBridge

/// 节流用的延迟调度器：`delay` 秒后在主线程执行 `work`，返回取消闭包。
/// 抽成参数是为了测试里手动推进，不真的 sleep。
typealias FindScheduler = @MainActor (_ delay: TimeInterval, _ work: @escaping @MainActor () -> Void) -> (@MainActor () -> Void)

/// 终端查找栏的控制器（terminal.md §11）。
///
/// **建议用法：每个会话一个 controller**。每个 controller 持有自己的搜索文字、开关和 `TerminalSearchTarget`，
/// 切换标签时只显示活动会话的那个、隐藏其余的（`install(in:)` 把视图装进对应会话的终端容器即可），
/// 这样各会话的查找状态互不干扰；不要让多个会话共用同一个 controller 去换 target。
///
/// 接入方的职责：
/// - 创建后调用 `install(in:)`，并设置 `onDismiss`（Esc / 关闭按钮后把焦点还给终端）。
/// - ⌘F → `show()`；⌘G / ⇧⌘G → `findNext()` / `findPrevious()`。
/// - 每次该会话收到 `TerminalDirty` 都可以调 `refreshStatus()`，内部已节流。
@MainActor
final class FindBarController: NSObject, NSTextFieldDelegate {
    /// 状态刷新的最小间隔（秒）。
    static let refreshInterval: TimeInterval = 0.25

    let view = FindBarView()
    private let target: TerminalSearchTarget
    private let scheduler: FindScheduler

    /// 查找栏被用户关闭（Esc / 关闭按钮）后回调，接入方在这里把焦点还给终端。
    var onDismiss: (() -> Void)?

    /// 当前状态文字，供测试与调用方读取；与视图上的 `statusLabel` 一致。
    private(set) var statusText = ""
    private(set) var statusIsError = false

    /// 节流窗口的取消闭包；非 nil 表示窗口内（刚执行过一次）。
    private var cancelThrottle: (@MainActor () -> Void)?
    /// 窗口内是否又收到了 `refreshStatus()`，需要在窗口结束时尾随执行。
    private var trailingPending = false

    init(
        target: TerminalSearchTarget,
        scheduler: @escaping FindScheduler = FindBarController.defaultScheduler
    ) {
        self.target = target
        self.scheduler = scheduler
        super.init()
        view.isHidden = true
        view.searchField.delegate = self
        view.caseSensitiveButton.target = self
        view.caseSensitiveButton.action = #selector(optionsChanged(_:))
        view.regexButton.target = self
        view.regexButton.action = #selector(optionsChanged(_:))
        view.previousButton.target = self
        view.previousButton.action = #selector(previousClicked(_:))
        view.nextButton.target = self
        view.nextButton.action = #selector(nextClicked(_:))
        view.closeButton.target = self
        view.closeButton.action = #selector(closeClicked(_:))
    }

    /// 生产用调度器：主线程上延迟执行，可取消。
    static let defaultScheduler: FindScheduler = { delay, work in
        let task = Task { @MainActor in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            work()
        }
        return { task.cancel() }
    }

    var isVisible: Bool { !view.isHidden }

    /// 当前搜索文字。
    var text: String { view.searchField.stringValue }

    private var isCaseSensitive: Bool { view.caseSensitiveButton.state == .on }
    private var isRegex: Bool { view.regexButton.state == .on }

    // MARK: 安装与显示

    /// 把查找栏装进终端容器：钉在右上角（距边 `Spacing.sm`）、固定宽度、默认隐藏。
    /// 容器应是终端区域本身（不含侧栏 / 状态栏），浮层才会盖在终端内容上。
    func install(in container: NSView) {
        view.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(view)
        NSLayoutConstraint.activate([
            view.topAnchor.constraint(equalTo: container.topAnchor, constant: DesignTokens.Spacing.sm),
            view.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -DesignTokens.Spacing.sm),
            view.widthAnchor.constraint(equalToConstant: FindBarView.barWidth),
        ])
        view.isHidden = true
    }

    /// 显示并聚焦文本框。已有文字时全选（再次 ⌘F 方便直接改写），并立即重新搜索以恢复高亮
    /// （隐藏时 `target.close()` 已清掉了高亮）。
    func show() {
        view.isHidden = false
        view.window?.makeFirstResponder(view.searchField)
        if !text.isEmpty {
            view.searchField.selectText(nil)
            performSearch()
        }
    }

    /// 关闭：清高亮、隐藏、通知接入方还焦点。
    func dismiss() {
        cancelThrottleWindow()
        target.close()
        view.isHidden = true
        onDismiss?()
    }

    // MARK: 查找

    /// ⌘G。栏隐藏时先显示（并恢复高亮）；文字为空什么都不做。
    func findNext() { step(backwards: false) }

    /// ⇧⌘G。语义同 `findNext()`。
    func findPrevious() { step(backwards: true) }

    private func step(backwards: Bool) {
        if !isVisible { show() }
        guard !text.isEmpty else { return }
        do {
            apply(try target.next(backwards: backwards))
        } catch {
            apply(error: error)
        }
    }

    /// 新输出到来时由接入方调用（每次 `TerminalDirty` 都可以调）。
    ///
    /// 节流：窗口外立即执行一次并开启 250ms 窗口；窗口内的调用只记一个"尾随"标记，窗口结束时补执行一次，
    /// 保证最后一次输出之后的计数是准确的。栏隐藏或文字为空时什么都不做。
    func refreshStatus() {
        guard isVisible, !text.isEmpty else { return }
        if cancelThrottle != nil {
            trailingPending = true
            return
        }
        readStatus()
        startThrottleWindow()
    }

    private func startThrottleWindow() {
        cancelThrottle = scheduler(Self.refreshInterval) { [weak self] in
            guard let self else { return }
            cancelThrottle = nil
            guard trailingPending else { return }
            trailingPending = false
            // 栏可能在窗口期间被关闭或清空；此时不再读取状态。
            guard isVisible, !text.isEmpty else { return }
            readStatus()
            // 尾随执行本身也开新窗口，保证任意时刻的执行频率不超过 4 次/秒。
            startThrottleWindow()
        }
    }

    private func cancelThrottleWindow() {
        cancelThrottle?()
        cancelThrottle = nil
        trailingPending = false
    }

    private func readStatus() {
        do {
            apply(try target.status())
        } catch {
            apply(error: error)
        }
    }

    /// 用当前文字和开关搜索；文字为空则关闭搜索并清空状态（不显示"无结果"）。
    private func performSearch() {
        guard !text.isEmpty else {
            target.close()
            setStatus("", isError: false)
            return
        }
        do {
            apply(try target.search(text, caseSensitive: isCaseSensitive, regex: isRegex))
        } catch {
            apply(error: error)
        }
    }

    // MARK: 状态

    private func apply(_ status: FfiSearchStatus) {
        if status.total == 0 {
            setStatus(String(localized: "No Results"), isError: false)
        } else if let current = status.current {
            setStatus("\(current)/\(status.total)", isError: false)
        } else {
            // 有匹配但当前项不在其中（例如滚动位置之外）：用 en dash 占位。
            setStatus("–/\(status.total)", isError: false)
        }
    }

    /// 非法正则时 core 抛 `FfiCoreError.User`（只有它带可展示的消息，这里统一显示固定文案）；
    /// 其他错误（会话已不存在等）静默清空状态，不弹窗。
    private func apply(error: Error) {
        if case .User = error as? FfiCoreError {
            setStatus(String(localized: "Invalid Regular Expression"), isError: true)
        } else {
            setStatus("", isError: false)
        }
    }

    private func setStatus(_ text: String, isError: Bool) {
        statusText = text
        statusIsError = isError
        view.setStatus(text, isError: isError)
    }

    // MARK: 控件事件

    /// 文字或开关变化：立即搜索（不走节流，用户操作需要即时反馈）。
    func queryChanged() {
        performSearch()
    }

    @objc private func optionsChanged(_ sender: Any?) { queryChanged() }
    @objc private func previousClicked(_ sender: Any?) { findPrevious() }
    @objc private func nextClicked(_ sender: Any?) { findNext() }
    @objc private func closeClicked(_ sender: Any?) { dismiss() }

    func controlTextDidChange(_ obj: Notification) {
        queryChanged()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        let shift = NSApp.currentEvent?.modifierFlags.contains(.shift) ?? false
        return handleCommand(commandSelector, shift: shift)
    }

    /// 处理文本框的编辑命令。从 `control(_:textView:doCommandBy:)` 拆出来，让测试不依赖当前事件。
    ///
    /// AppKit 对 Return 发 `insertNewline:`，Option+Return 发 `insertNewlineIgnoringFieldEditor:`，
    /// Shift+Return 在不同输入环境下可能走其中任一个（或 `insertLineBreak:`），
    /// 所以三者都接，方向统一由 Shift 修饰键决定。
    func handleCommand(_ selector: Selector, shift: Bool) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)),
             #selector(NSResponder.insertNewlineIgnoringFieldEditor(_:)),
             #selector(NSResponder.insertLineBreak(_:)):
            if shift { findPrevious() } else { findNext() }
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            dismiss()
            return true
        default:
            return false
        }
    }
}
