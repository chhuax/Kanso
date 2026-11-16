import KansoBridge

/// 查找栏操作的搜索后端。抽象出来是为了让查找栏不依赖真实 core，测试用假实现。
///
/// 高亮由 core 写进帧、渲染器绘制；实现方只负责把请求转给 core，查找栏自己不画任何高亮。
@MainActor
protocol TerminalSearchTarget: AnyObject {
    /// 用新的查询（文字 / 开关变化）重新搜索。core 负责增量语义：保持当前项不跳。
    /// - Throws: 非法正则时抛 `FfiCoreError.User`；会话不存在等抛其他错误。
    func search(_ text: String, caseSensitive: Bool, regex: Bool) throws -> FfiSearchStatus

    func status() throws -> FfiSearchStatus

    /// 跳到下一个（`backwards` 为真则上一个）匹配项。
    func next(backwards: Bool) throws -> FfiSearchStatus

    /// 结束搜索：清除高亮，并停止随输出重新搜索（terminal.md §11）。
    func close()
}

/// 基于 `KansoCore` 的搜索后端，一个实例对应一个会话。
@MainActor
final class CoreSearchTarget: TerminalSearchTarget {
    private let core: KansoCore
    private let session: UInt64

    init(core: KansoCore, session: UInt64) {
        self.core = core
        self.session = session
    }

    func search(_ text: String, caseSensitive: Bool, regex: Bool) throws -> FfiSearchStatus {
        try core.search(
            sessionId: session,
            query: FfiSearchQuery(text: text, caseSensitive: caseSensitive, regex: regex)
        )
    }

    func next(backwards: Bool) throws -> FfiSearchStatus {
        try core.searchNext(sessionId: session, backwards: backwards)
    }

    func status() throws -> FfiSearchStatus {
        try core.searchStatus(sessionId: session)
    }

    func close() {
        core.searchClose(sessionId: session)
    }
}
