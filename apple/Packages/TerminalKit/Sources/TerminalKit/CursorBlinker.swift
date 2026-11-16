import Foundation

/// 光标闪烁的相位计时（terminal.md §15）。
///
/// 只管"现在该显示还是该隐藏"，不管绘制：相位变化通过 `onChange` 通知视图重绘。
/// 规则：激活时每 `interval` 翻转一次；`reset()`（有按键或新输出）立刻回到可见并重新计时，
/// 这样用户打字时光标不会恰好在隐藏相位里"消失"；停用（失焦、光标隐藏、程序要求不闪烁）时固定为可见。
///
/// 计时源可注入：默认用主线程 `Timer`，测试注入手动时钟，不依赖真实时间。必须在主线程使用。
///
/// 生命周期：`Timer` 被 run loop 强持有，对象释放不会让它停下，所以使用方必须在不再需要时
/// 调用 `setActive(false)`（`TerminalView` 在离开窗口时做这件事）。
@MainActor
final class CursorBlinker {
    /// 一个相位的时长。530 ms 是常见终端的惯用周期（约 1 Hz 的整周期）。
    static let interval: TimeInterval = 0.53

    /// 取消一次已安排的重复计时。
    typealias Cancel = () -> Void
    /// 安排重复计时：每隔 `interval` 调用一次 `fire`，返回取消闭包。
    typealias Scheduler = (_ interval: TimeInterval, _ fire: @escaping @MainActor () -> Void) -> Cancel

    /// 默认计时源。用 `.common` 模式：菜单跟踪、窗口拖动期间也能继续走，否则光标会冻在某个相位上。
    static let systemScheduler: Scheduler = { interval, fire in
        let timer = Timer(timeInterval: interval, repeats: true) { _ in
            MainActor.assumeIsolated { fire() }
        }
        RunLoop.main.add(timer, forMode: .common)
        return { timer.invalidate() }
    }

    private let scheduler: Scheduler
    private var cancelTimer: Cancel?

    /// 当前相位：true 为显示。
    private(set) var isVisible = true
    /// 相位真正变化时回调（新值）。
    var onChange: ((Bool) -> Void)?

    private(set) var isActive = false

    init(scheduler: @escaping Scheduler = CursorBlinker.systemScheduler) {
        self.scheduler = scheduler
    }

    /// 开始或停止闪烁。停止时固定为可见；重复设置同一状态无副作用（不会重置相位）。
    func setActive(_ active: Bool) {
        guard active != isActive else { return }
        isActive = active
        if active {
            startTimer()
        } else {
            cancelTimer?()
            cancelTimer = nil
            setVisible(true)
        }
    }

    /// 有按键或新输出：立刻可见并从头计时。未激活时只确保可见。
    func reset() {
        setVisible(true)
        guard isActive else { return }
        cancelTimer?()
        startTimer()
    }

    private func startTimer() {
        cancelTimer = scheduler(Self.interval) { [weak self] in
            guard let self else { return }
            setVisible(!isVisible)
        }
    }

    private func setVisible(_ visible: Bool) {
        guard visible != isVisible else { return }
        isVisible = visible
        onChange?(visible)
    }
}
