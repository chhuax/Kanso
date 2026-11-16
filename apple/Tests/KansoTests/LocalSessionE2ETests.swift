import KansoBridge
@testable import KansoUI
import TerminalKit
import XCTest

/// 真实 core 的端到端测试：`KansoCore(config:listener:)` → 本地 shell（PTY）→ 终端内核 → 帧。
///
/// 隔离：数据目录是临时目录；shell 是 `/bin/sh -i`（带参数，所以不会被自动加 `-l`，不读 `/etc/profile`
/// 与 `~/.profile`）。Swift 侧没有办法改子进程的 `HOME`（子进程继承测试进程的环境），但这条命令行不读任何用户配置文件。
@MainActor
final class LocalSessionE2ETests: XCTestCase {
    /// 每个测试一个临时目录，测试结束后删除。
    private func makeTempDir() -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kanso-e2e-\(UUID().uuidString)", isDirectory: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return dir
    }

    private func makeCore(dispatcher: CoreEventDispatcher, dataRoot: URL) throws -> KansoCore {
        let config = FfiCoreConfig(
            dataDir: dataRoot.appendingPathComponent("data").path,
            appVersion: "0.0.0-test",
            bundleId: "test.kanso",
            preferredLanguage: "en",
            region: nil,
            defaultShell: "/bin/sh -i",
            shellIntegration: false
        )
        // 必须传内存假实现：测试不能碰用户真实的钥匙串。
        return try KansoCore(config: config, listener: dispatcher, secrets: InMemorySecretStore())
    }

    private func text(of frame: FrameSnapshot) -> String {
        var out = ""
        for row in 0..<frame.rows {
            for col in 0..<frame.cols {
                let ch = frame.cell(row: row, col: col).ch
                out.append(ch == 0 ? " " : Character(Unicode.Scalar(ch) ?? "?"))
            }
            out.append("\n")
        }
        return out
    }

    func testRealCoreRunsShellAndFrameContainsOutput() async throws {
        let dispatcher = CoreEventDispatcher()
        let root = makeTempDir()
        let core = try makeCore(dispatcher: dispatcher, dataRoot: root)
        defer { core.shutdown() }
        // 数据目录由 core 创建。
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("data").path))

        let session = try core.openSession(target: .defaultLocalShell, size: FfiGridSize(cols: 80, rows: 24))
        let sink = CoreInputSink(core: core, session: session)
        let source = CoreFrameSource(core: core, session: session)

        // 用算术展开让输出与回显的输入不同：只有 shell 真正执行了命令才会出现 kanso-e2e。
        sink.textInput("echo kanso-e$((1+1))e\r")
        var previous: FrameSnapshot?
        var screen = ""
        let deadline = Date().addingTimeInterval(20)
        while !screen.contains("kanso-e2e"), Date() < deadline {
            if let frame = source.takeFrame(reusing: previous) {
                previous = frame
                screen = text(of: frame)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTAssertTrue(screen.contains("kanso-e2e"), "屏幕上没有 shell 的输出：\n\(screen)")
        // 会话信息来自真实 core。
        XCTAssertEqual(core.sessions().count, 1)
    }

    func testShellExitIsReportedAsClosedEventWithExitCode() async throws {
        let dispatcher = CoreEventDispatcher()
        let exited = expectation(description: "收到 Closed(Exited(7))")
        let core = try makeCore(dispatcher: dispatcher, dataRoot: makeTempDir())
        defer { core.shutdown() }
        let session = try core.openSession(target: .defaultLocalShell, size: FfiGridSize(cols: 80, rows: 24))
        dispatcher.addHandler { event in
            if case .sessionStateChanged(let id, .closed(.exited(let code))) = event, id == session, code == 7 {
                exited.fulfill()
            }
        }
        CoreInputSink(core: core, session: session).textInput("exit 7\r")
        await fulfillment(of: [exited], timeout: 20)
    }
}
