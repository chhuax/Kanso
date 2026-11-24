import KansoBridge
@testable import KansoUI
@testable import TerminalKit
import XCTest

/// FFI 链路测试：`KansoCore.newMock` → 事件分发 → 零拷贝取帧 → 输入映射 → async 方法 → 错误。
///
/// 事件由 core 在任意线程回调、经主队列投递，所以事件相关的断言都用 expectation 等待。
@MainActor
final class CoreFFITests: XCTestCase {
    private static let grid = FfiGridSize(cols: 80, rows: 24)

    /// 收集 dispatcher 分发出来的事件（主线程）。
    @MainActor
    private final class EventLog {
        var events: [FfiCoreEvent] = []
        var onChange: (() -> Void)?
    }

    private func makeCore() -> (core: KansoCore, dispatcher: CoreEventDispatcher, log: EventLog) {
        let dispatcher = CoreEventDispatcher()
        let log = EventLog()
        dispatcher.addHandler { event in
            log.events.append(event)
            log.onChange?()
        }
        return (KansoCore.newMock(listener: dispatcher), dispatcher, log)
    }

    private func openLocal(_ core: KansoCore) throws -> UInt64 {
        try core.openSession(target: .defaultLocalShell, size: Self.grid)
    }

    private func keyEvent(_ text: String) -> TerminalKeyEvent {
        TerminalKeyEvent(key: .character(text), text: text)
    }

    // MARK: 事件

    func testSessionLifecycleEventsReachMainThreadHandlers() async throws {
        let (core, _, log) = makeCore()
        let all = expectation(description: "收到 Connecting / Connected / TerminalDirty")
        var session: UInt64 = 0
        log.onChange = {
            var connecting = false, connected = false, dirty = false
            for e in log.events {
                switch e {
                case .sessionStateChanged(let s, .connecting) where s == session: connecting = true
                case .sessionStateChanged(let s, .connected) where s == session: connected = true
                case .terminalDirty(let s) where s == session: dirty = true
                default: break
                }
            }
            if connecting && connected && dirty { all.fulfill() }
        }
        session = try openLocal(core)
        await fulfillment(of: [all], timeout: 5)
        XCTAssertTrue(Thread.isMainThread)
    }

    func testTerminalDirtyTriggersAttachedViewToPullFrame() async throws {
        let (core, dispatcher, log) = makeCore()
        let session = try openLocal(core)
        let (view, window) = try InputFixtures.makeView()
        let source = CountingSource(inner: CoreFrameSource(core: core, session: session))
        view.frameSource = source
        dispatcher.attach(view: view, to: session)

        // 先等开会话产生的事件（含 TerminalDirty）全部投递完，再消费初始的"待取帧"。
        let initial = expectation(description: "初始事件")
        log.onChange = {
            if log.events.contains(.terminalDirty(session: session)) { initial.fulfill() }
        }
        await fulfillment(of: [initial], timeout: 5)
        view.pullFrameIfNeeded()
        XCTAssertGreaterThanOrEqual(source.calls, 1)
        let before = source.calls

        // 没有新事件时视图不会再取帧。
        view.pullFrameIfNeeded()
        XCTAssertEqual(source.calls, before)

        // 按键使 core 重新变脏 → 事件 → setNeedsFrame → 视图再取帧。
        let dirtyAgain = expectation(description: "再次 TerminalDirty")
        let baseline = log.events.count
        log.onChange = {
            if log.events.dropFirst(baseline).contains(.terminalDirty(session: session)) { dirtyAgain.fulfill() }
        }
        CoreInputSink(core: core, session: session).key(keyEvent("a"))
        await fulfillment(of: [dirtyAgain], timeout: 5)
        view.pullFrameIfNeeded()
        XCTAssertEqual(source.calls, before + 1)
        _ = window
    }

    // MARK: 帧

    func testTakeFrameZeroCopyAndReuse() throws {
        let (core, _, _) = makeCore()
        let session = try openLocal(core)
        let source = CoreFrameSource(core: core, session: session)

        let first = try XCTUnwrap(source.takeFrame(reusing: nil), "刚打开的会话是脏的")
        XCTAssertEqual(first.cols, 80)
        XCTAssertEqual(first.rows, 24)
        // 传回 reuse 并拿到新帧之后，旧缓冲区在同一次调用里就被 core 原地复用，旧帧不能再读；先把要比较的值取出来。
        let firstSeq = first.seq
        let firstHandle = try XCTUnwrap(first.coreFrameHandle)
        XCTAssertNotEqual(firstHandle.headerAddress(), 0)
        XCTAssertEqual(firstHandle.headerAddress() % 8, 0)

        // 没变脏：返回 nil，且传入的上一帧保持有效（视图在无新帧时仍会重绘它）。
        XCTAssertNil(source.takeFrame(reusing: first))
        XCTAssertNotEqual(firstHandle.headerAddress(), 0)
        _ = first.cell(row: 0, col: 0)

        // 一个按键使其变脏；取帧成功，序号推进，并把上一帧作为 reuse 传回 -> 旧缓冲区被 core 回收。
        CoreInputSink(core: core, session: session).key(keyEvent("a"))
        let second = try XCTUnwrap(source.takeFrame(reusing: first))
        XCTAssertEqual(second.seq, firstSeq + 1)
        XCTAssertEqual(second.cols, 80)
        XCTAssertEqual(firstHandle.headerAddress(), 0, "传回 reuse 且拿到新帧后旧句柄失效")
        XCTAssertNotEqual(try XCTUnwrap(second.coreFrameHandle).headerAddress(), 0)
        _ = second.cell(row: 0, col: 0)

        // 空闲后再传回新帧也不应使其失效。
        XCTAssertNil(source.takeFrame(reusing: second))
        _ = second.cell(row: 1, col: 1)
    }

    func testResizeMarksDirtyAndChangesFrameSize() throws {
        let (core, _, _) = makeCore()
        let session = try openLocal(core)
        let source = CoreFrameSource(core: core, session: session)
        _ = try XCTUnwrap(source.takeFrame(reusing: nil))
        try core.resize(sessionId: session, size: FfiGridSize(cols: 100, rows: 30), cellWidthPx: 8, cellHeightPx: 16)
        let resized = try XCTUnwrap(source.takeFrame(reusing: nil))
        XCTAssertEqual(resized.cols, 100)
        XCTAssertEqual(resized.rows, 30)
    }

    // MARK: 输入映射

    func testInputMappingCoversAllShapes() {
        let mods: KeyModifiers = [.shift, .altRight, .meta]
        let m = CoreInputSink.map(mods)
        XCTAssertEqual(m, FfiModifiers(shift: true, ctrl: false, altLeft: false, altRight: true, meta: true))

        XCTAssertEqual(CoreInputSink.map(.function(5)), .function(number: 5))
        XCTAssertEqual(CoreInputSink.map(.character("é")), .character(text: "é"))
        XCTAssertEqual(CoreInputSink.map(.delete), .delete)
        XCTAssertEqual(CoreInputSink.map(.pageUp), .pageUp)

        let e = CoreInputSink.map(TerminalKeyEvent(key: .enter, mods: .ctrl, text: nil, kind: .repeat))
        XCTAssertEqual(e.key, .enter)
        XCTAssertEqual(e.mods.ctrl, true)
        XCTAssertNil(e.text)
        XCTAssertEqual(e.kind, .repeat)

        let mouse = CoreInputSink.map(TerminalMouseEvent(
            kind: .drag, button: .middle, point: ViewportPointValue(col: 3, row: 4, side: .right),
            mods: .shift, clickCount: 2
        ))
        XCTAssertEqual(mouse.kind, .drag)
        XCTAssertEqual(mouse.button, .middle)
        XCTAssertEqual(mouse.point, FfiViewportPoint(col: 3, row: 4, side: .right))
        XCTAssertEqual(mouse.clickCount, 2)
        XCTAssertTrue(mouse.mods.shift)
    }

    func testInputSinkDrivesCoreForAllInputKinds() throws {
        let (core, _, _) = makeCore()
        let session = try openLocal(core)
        let sink = CoreInputSink(core: core, session: session)
        let source = CoreFrameSource(core: core, session: session)
        _ = try XCTUnwrap(source.takeFrame(reusing: nil))

        // 鼠标、滚动、焦点不改变 mock 帧；文字输入与按键会。
        sink.mouse(TerminalMouseEvent(kind: .down, button: .left, point: ViewportPointValue(col: 0, row: 0, side: .left)))
        sink.scroll(lines: 3, at: ViewportPointValue(col: 0, row: 0, side: .left), mods: [])
        sink.focus(true)
        XCTAssertNil(source.takeFrame(reusing: nil))
        sink.textInput("你好")
        XCTAssertNotNil(source.takeFrame(reusing: nil))
        sink.key(keyEvent("x"))
        XCTAssertNotNil(source.takeFrame(reusing: nil))
    }

    // MARK: async

    func testAsyncMethodsAwaitSuccessfully() async throws {
        let (core, _, _) = makeCore()
        let local = try openLocal(core)
        let cwd = try await core.workingDirectory(sessionId: local)
        XCTAssertEqual(cwd, "~/workspace/kanso")

        // 种子数据里有一个 SFTP profile。
        let sftpProfile = try XCTUnwrap(core.profiles().first {
            if case .sftp = $0.kind { return true }
            return false
        })
        let sftp = try core.openSession(target: .profile(id: sftpProfile.id), size: Self.grid)
        let home = try await core.sftpHome(sessionId: sftp)
        XCTAssertTrue(home.hasPrefix("/home/"))

        let stream = try core.sftpList(sessionId: sftp, path: "/srv")
        var names: [String] = []
        while let batch = try await stream.nextBatch() {
            names += batch.map(\.name)
        }
        XCTAssertEqual(names, ["docs", "current", "app.log", "README.md", ".bashrc"])
    }

    func testAsyncErrorsPropagateAsThrows() async throws {
        let (core, _, _) = makeCore()
        do {
            _ = try await core.workingDirectory(sessionId: 424242)
            XCTFail("不存在的会话应抛出错误")
        } catch let error as FfiCoreError {
            guard case .NoSuchSession = error else { return XCTFail("非预期错误：\(error)") }
        }
    }

    // MARK: 错误

    func testUserErrorExposesMessageKey() throws {
        let (core, _, _) = makeCore()
        let profile = FfiProfile(
            id: UUID().uuidString.lowercased(), name: "bad", color: nil, group: nil,
            kind: .ssh(profile: FfiSshProfile(
                target: FfiSshTarget(
                    host: "", port: 22, user: "root", auth: .password,
                    privateKeyPath: nil, jump: nil, proxyCommand: nil, keepalive: .defaultKeepalive
                ),
                encoding: "", locale: nil
            ))
        )
        XCTAssertThrowsError(try core.saveProfile(profile: profile)) { error in
            guard case FfiCoreError.User(let message) = error else {
                return XCTFail("非预期错误：\(error)")
            }
            XCTAssertEqual(message.key, "profile-host-empty")
            XCTAssertTrue(message.args.isEmpty)
        }
    }

    func testMessageArgumentsAreFlattened() throws {
        let (core, _, _) = makeCore()
        let ssh = try XCTUnwrap(core.profiles().first {
            if case .ssh = $0.kind { return true }
            return false
        })
        // 把跳板指向自己：成环，core 返回带参数的消息。
        var looped = ssh
        guard case .ssh(var inner) = looped.kind else { return XCTFail() }
        inner.target.jump = looped.id
        looped.kind = .ssh(profile: inner)
        XCTAssertThrowsError(try core.saveProfile(profile: looped)) { error in
            guard case FfiCoreError.User(let message) = error else {
                return XCTFail("非预期错误：\(error)")
            }
            XCTAssertTrue(message.key.hasPrefix("jump-"), message.key)
        }
    }
}

/// 记录被调用次数的 `FrameSource` 包装。
@MainActor
private final class CountingSource: FrameSource {
    let inner: FrameSource
    private(set) var calls = 0
    init(inner: FrameSource) { self.inner = inner }
    func takeFrame(reusing previous: FrameSnapshot?) -> FrameSnapshot? {
        calls += 1
        return inner.takeFrame(reusing: previous)
    }
}
