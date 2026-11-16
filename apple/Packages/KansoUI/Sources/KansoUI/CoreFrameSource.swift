import KansoBridge
import TerminalKit

/// 由 core 提供帧的 `FrameSource`：`takeFrame(reusing:)` 对应 `KansoCore.takeFrame`。
///
/// 零拷贝：返回的 `FrameSnapshot` 直接读 Rust 持有的缓冲区，并强引用 `FrameHandle` 保活。
/// 复用：把视图当前持有的上一帧的句柄交还 core；core 只在**确实产出新帧**时才回收旧缓冲区，
/// 没有新帧（返回 nil）时旧帧保持有效——`TerminalView` 在没有新帧时仍会重绘旧帧，
/// 所以这一点是内存安全的前提（见 Rust 侧 `KansoCore::take_frame` 的文档）。
@MainActor
public final class CoreFrameSource: FrameSource {
    private let core: KansoCore
    public let session: UInt64

    public init(core: KansoCore, session: UInt64) {
        self.core = core
        self.session = session
    }

    public func takeFrame(reusing previous: FrameSnapshot?) -> FrameSnapshot? {
        guard let handle = core.takeFrame(sessionId: session, reuse: previous?.coreFrameHandle) else {
            return nil
        }
        guard let snapshot = FrameSnapshot(coreFrame: handle) else {
            // core 产出了无法解析的帧是 ABI 违约。此时旧帧的缓冲区可能已被回收，
            // 返回 nil 会让视图继续读它（悬垂读），比立刻崩溃更糟，所以直接终止。
            fatalError("core 返回的帧头校验失败，违反 docs/frame-abi.md")
        }
        return snapshot
    }
}
