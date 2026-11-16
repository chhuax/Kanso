import Dispatch
import KansoBridge
import TerminalKit

/// core 事件的接收端：把 core 在任意线程回调的事件投递到主线程，再分发给注册的处理者。
///
/// 契约（与 `FfiEventListener` / Rust `EventSink` 一致）：
/// - `onEvent` 可能在**任意线程**被调用，只做"投递到主线程"这一件事就返回，不得阻塞；
/// - 绝不在 `onEvent` 里同步回调 `KansoCore`：core 可能正持有内部锁，会死锁；
///   处理者在主线程运行，已经与 core 的回调栈脱钩，可以安全调用 core。
///
/// 为什么用 `DispatchQueue.main.async` 而不是 `Task { @MainActor in }`：
/// 主队列是严格 FIFO，保证"同一会话内的事件保序"这一契约；两个独立 Task 之间没有形式化的顺序保证。
public final class CoreEventDispatcher: FfiEventListener {
    /// 处理者与终端视图登记表。只在主线程访问，由 `@MainActor` 保证；
    /// 盒子本身是 `@MainActor` 类（隐式 `Sendable`），因此本类可以不用 `@unchecked Sendable`。
    private let registry = Registry()

    public init() {}

    /// 事件处理者。总是在主线程调用。
    public typealias Handler = @MainActor (FfiCoreEvent) -> Void

    /// 注册处理者，返回令牌，用于 `removeHandler`。处理者按注册顺序调用。
    @MainActor
    @discardableResult
    public func addHandler(_ handler: @escaping Handler) -> Int {
        registry.add(handler)
    }

    /// 移除处理者。
    @MainActor
    public func removeHandler(_ token: Int) {
        registry.remove(token)
    }

    /// 登记某会话对应的终端视图：该会话的 `TerminalDirty` 会转成 `view.setNeedsFrame()`。
    /// 视图是弱引用，视图销毁后自动失效，无需手动取消登记。
    @MainActor
    public func attach(view: TerminalView, to session: UInt64) {
        registry.views[session] = WeakView(view)
    }

    // MARK: FfiEventListener

    public func onEvent(event: FfiCoreEvent) {
        DispatchQueue.main.async { [registry] in
            // 主队列上的块一定在主线程执行；闭包类型不带 @MainActor 标注，这里显式声明隔离。
            MainActor.assumeIsolated { registry.dispatch(event) }
        }
    }
}

private struct WeakView {
    weak var view: TerminalView?
    init(_ view: TerminalView) { self.view = view }
}

@MainActor
private final class Registry {
    private var handlers: [(token: Int, handler: CoreEventDispatcher.Handler)] = []
    private var nextToken = 0
    var views: [UInt64: WeakView] = [:]

    // 构造不触碰任何隔离状态；声明为 nonisolated 才能在 `CoreEventDispatcher`（非隔离）的属性初始化里创建。
    nonisolated init() {}

    func add(_ handler: @escaping CoreEventDispatcher.Handler) -> Int {
        nextToken += 1
        handlers.append((nextToken, handler))
        return nextToken
    }

    func remove(_ token: Int) {
        handlers.removeAll { $0.token == token }
    }

    func dispatch(_ event: FfiCoreEvent) {
        if case .terminalDirty(let session) = event {
            views[session]?.view?.setNeedsFrame()
        }
        // for-in 遍历的是数组快照，处理者里增删处理者不影响本轮分发。
        for entry in handlers { entry.handler(event) }
    }
}
