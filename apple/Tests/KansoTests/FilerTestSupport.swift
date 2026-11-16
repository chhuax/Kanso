import AppKit
import KansoUI
@testable import Kanso

/// 手动开关的门：让测试控制异步读取何时返回（构造"旧请求后返回"的竞争）。
final class Gate: @unchecked Sendable {
    private let stream: AsyncStream<Void>
    private let continuation: AsyncStream<Void>.Continuation
    init() { (stream, continuation) = AsyncStream<Void>.makeStream() }
    func wait() async { for await _ in stream { break } }
    func open() { continuation.yield(); continuation.finish() }
}

extension FilerEntry {
    static func dir(_ name: String, in parent: String = "/d") -> FilerEntry {
        FilerEntry(name: name, path: FilerPath.join(parent, name), isDirectory: true)
    }
    static func file(_ name: String, in parent: String = "/d", size: UInt64? = 1) -> FilerEntry {
        FilerEntry(name: name, path: FilerPath.join(parent, name), isDirectory: false, size: size)
    }
    static func link(_ name: String, in parent: String = "/d") -> FilerEntry {
        FilerEntry(name: name, path: FilerPath.join(parent, name), isDirectory: false, isSymlink: true)
    }
}

final class FakeDataSource: FilerDataSource, @unchecked Sendable {
    let isRemote: Bool
    var home = "/home"
    var dirs: [String: [FilerEntry]] = [:]
    var errors: [String: FilerError] = [:]
    var gates: [String: Gate] = [:]
    var linkTargets: [String: Result<FilerLinkTarget, FilerError>] = [:]
    var failCreate: FilerError?
    private let lock = NSLock()
    private var _lists: [String] = []
    private var _homeCalls = 0
    private var _ops: [String] = []

    init(remote: Bool) { isRemote = remote }

    var lists: [String] { lock.lock(); defer { lock.unlock() }; return _lists }
    var homeCalls: Int { lock.lock(); defer { lock.unlock() }; return _homeCalls }
    var ops: [String] { lock.lock(); defer { lock.unlock() }; return _ops }
    private func log(_ op: String) { lock.withLock { _ops.append(op) } }

    func homeDirectory() async throws -> String {
        lock.withLock { _homeCalls += 1 }
        return home
    }

    func list(path: String) -> AsyncThrowingStream<[FilerEntry], Error> {
        lock.withLock { _lists.append(path) }
        let gate = gates[path], error = errors[path], entries = dirs[path] ?? []
        return AsyncThrowingStream { continuation in
            Task {
                await gate?.wait()
                if let error { continuation.finish(throwing: error); return }
                // 分两批，覆盖"分批出现"。
                let mid = entries.count / 2
                if mid > 0 { continuation.yield(Array(entries[..<mid])) }
                continuation.yield(Array(entries[mid...]))
                continuation.finish()
            }
        }
    }

    func resolveLink(_ entry: FilerEntry) async throws -> FilerLinkTarget {
        try (linkTargets[entry.path] ?? .success(.file)).get()
    }
    func createFile(at path: String) async throws {
        log("createFile \(path)")
        if let failCreate { throw failCreate }
    }
    func createDirectory(at path: String) async throws {
        log("mkdir \(path)")
        if let failCreate { throw failCreate }
    }
    func remove(_ entry: FilerEntry) async throws {
        log("remove \(entry.path)")
        if entry.isDirectory, !(dirs[entry.path] ?? []).isEmpty { throw FilerError(.notEmpty, path: entry.path) }
    }
    func rename(from: String, to: String) async throws { log("rename \(from) -> \(to)") }
}

@MainActor
final class ScriptedHandle: FilerTransferHandle {
    let events: AsyncStream<FilerTransferEvent>
    private(set) var cancelled = false
    init(_ results: [FilerTransferEvent]) {
        let (s, c) = AsyncStream<FilerTransferEvent>.makeStream()
        events = s
        for e in results { c.yield(e) }
        c.finish()
    }
    func cancel() { cancelled = true }
}

/// 按项目名 + 第几次尝试给出结果的替身引擎。
@MainActor
final class FakeEngine: FilerTransferEngine {
    var script: [String: [FilerTransferResult]] = [:]
    var extraEvents: [FilerTransferEvent] = []
    private(set) var starts: [(item: FilerTransferItem, policy: FilerConflictPolicy)] = []
    private var attempts: [String: Int] = [:]

    func start(_ item: FilerTransferItem, policy: FilerConflictPolicy) throws -> FilerTransferHandle {
        starts.append((item, policy))
        let n = attempts[item.name, default: 0]
        attempts[item.name] = n + 1
        let results = script[item.name] ?? [.completed(skipped: 0)]
        let result = results[min(n, results.count - 1)]
        return ScriptedHandle([.started(total: 100)] + extraEvents + [.finished(result)])
    }
}

@MainActor
final class FakeEnv: FilerEnvironment {
    var sources: [SessionID: FakeDataSource] = [:]
    var localSource: FakeDataSource?
    var engine = FakeEngine()
    var copyEngine = FakeEngine()
    var cwd: [SessionID: Result<String, Error>] = [:]
    var cwdGates: [SessionID: Gate] = [:]
    var hostNames: Set<String> = ["my-mac.local"]

    func dataSource(for tab: FilerTabSnapshot) -> FilerDataSource {
        if tab.kind == .local, let localSource { return localSource }
        return sources[tab.session] ?? FakeDataSource(remote: tab.kind != .local)
    }
    func transferEngine(for tab: FilerTabSnapshot) -> FilerTransferEngine { engine }
    var localCopyEngine: FilerTransferEngine { copyEngine }
    func workingDirectory(session: SessionID) async throws -> String {
        await cwdGates[session]?.wait()
        return try (cwd[session] ?? .failure(FilerError(.other("none")))).get()
    }
    func localHostNames() -> Set<String> { hostNames }
}

@MainActor
final class FakePrompter: FilerPrompter {
    var deleteAnswer = true
    var conflictAnswers: [FilerConflictDecision] = []
    var uploadSources: [URL] = []
    var saveURL: URL?
    var downloadDirectory: URL?
    var pasteboardOK = true
    var openOK = true
    private(set) var deleteRequests: [FilerDeleteRequest] = []
    private(set) var conflictRequests: [FilerConflictRequest] = []
    private(set) var pasted: [String] = []
    private(set) var opened: [String] = []
    var editorApplication: URL?
    var largeEditAnswer = true
    var remoteEditChoices: [RemoteEditConflictChoice] = []
    private(set) var largeEditWarnings = 0
    private(set) var remoteEditConflicts = 0
    private(set) var remoteApplications: [URL?] = []
    private(set) var retainedCopies: [URL] = []

    func chooseEditorApplication() async -> URL? { editorApplication }
    func openRemoteCopy(_ url: URL, application: URL?) async throws {
        guard openOK else { throw FilerError(.other("无法启动应用")) }
        opened.append(url.path)
        remoteApplications.append(application)
    }
    func confirmLargeRemoteEdit(name: String, source: String) async -> Bool {
        largeEditWarnings += 1
        return largeEditAnswer
    }
    func resolveRemoteEditConflict(name: String, source: String) async -> RemoteEditConflictChoice {
        remoteEditConflicts += 1
        return remoteEditChoices.isEmpty ? .cancel : remoteEditChoices.removeFirst()
    }
    func showRetainedRemoteEdits(_ urls: [URL]) async { retainedCopies.append(contentsOf: urls) }

    func confirmDelete(_ request: FilerDeleteRequest) async -> Bool { deleteRequests.append(request); return deleteAnswer }
    func resolveConflict(_ request: FilerConflictRequest) async -> FilerConflictDecision {
        conflictRequests.append(request)
        return conflictAnswers.isEmpty ? .init(choice: .cancel, applyToRest: false) : conflictAnswers.removeFirst()
    }
    func chooseUploadSources(directories: Bool) async -> [URL] { uploadSources }
    func chooseDownloadFile(defaultName: String) async -> URL? { saveURL }
    func chooseDownloadDirectory() async -> URL? { downloadDirectory }
    func copyToPasteboard(_ text: String) -> Bool { pasted.append(text); return pasteboardOK }
    func openLocal(_ path: String) -> Bool { opened.append(path); return openOK }
}

@MainActor
enum FilerFixtures {
    static func tab(_ id: SessionID, kind: TabKind = .local, name: String = "tab", connection: TabConnection = .connected, cwdHost: String? = nil) -> FilerTabSnapshot {
        FilerTabSnapshot(session: id, kind: kind, name: name, userHost: kind == .ssh ? "root@\(name).example.com" : nil,
                         connection: connection, cwdHost: cwdHost)
    }

    static func make() -> (FilerModel, FakeEnv, FakePrompter) {
        let env = FakeEnv(), prompter = FakePrompter()
        return (FilerModel(env: env, prompter: prompter, clock: { 0 }), env, prompter)
    }
}
