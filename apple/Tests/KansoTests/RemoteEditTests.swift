import KansoUI
@testable import Kanso
import XCTest

private final class EditBackend: FilerRemoteBackend, @unchecked Sendable {
    private let lock = NSLock()
    private var files: [RemoteEditManager.Key: Data] = [:]
    private var revisions: [RemoteEditManager.Key: Int] = [:]
    var reportedSize: UInt64?

    func put(_ text: String, session: SessionID = 1, path: String = "/d/config.txt") {
        put(Data(text.utf8), session: session, path: path)
    }

    func put(_ data: Data, session: SessionID, path: String) {
        lock.withLock {
            let key = RemoteEditManager.Key(session: session, path: path)
            files[key] = data
            revisions[key, default: 0] += 1
        }
    }

    func read(session: SessionID = 1, path: String = "/d/config.txt") throws -> Data {
        try lock.withLock {
            guard let data = files[.init(session: session, path: path)] else { throw FilerError(.notFound) }
            return data
        }
    }

    func stat(session: UInt64, path: String) async throws -> FilerFileVersion {
        try lock.withLock {
            let key = RemoteEditManager.Key(session: session, path: path)
            guard let data = files[key] else { throw FilerError(.notFound) }
            return FilerFileVersion(size: reportedSize ?? UInt64(data.count), modified: Date(timeIntervalSince1970: Double(revisions[key]!)))
        }
    }

    func home(session: UInt64) async throws -> String { "/d" }
    func list(session: UInt64, path: String) throws -> FilerRemoteDirStream { throw FilerError(.notADirectory) }
    func mkdir(session: UInt64, path: String) async throws {}
    func createFile(session: UInt64, path: String) async throws {}
    func remove(session: UInt64, path: String, isDirectory: Bool) async throws {}
    func rename(session: UInt64, from: String, to: String) async throws {}
}

@MainActor
private final class EditHandle: FilerTransferHandle {
    let events: AsyncStream<FilerTransferEvent>
    let continuation: AsyncStream<FilerTransferEvent>.Continuation
    var task: Task<Void, Never>?

    init() { (events, continuation) = AsyncStream.makeStream() }
    func cancel() {
        task?.cancel()
        continuation.yield(.finished(.cancelled))
        continuation.finish()
    }
}

@MainActor
private final class EditEngine: FilerTransferEngine {
    let backend: EditBackend
    var session: SessionID = 1
    var uploadGate: Gate?
    var downloadGate: Gate?
    var uploadFailures = 0
    var downloadResult: FilerTransferResult?
    private(set) var starts: [(SessionID, FilerTransferItem, FilerConflictPolicy)] = []
    var uploads: [FilerTransferItem] { starts.filter { $0.1.kind == .upload }.map { $0.1 } }
    var downloads: [FilerTransferItem] { starts.filter { $0.1.kind == .download }.map { $0.1 } }

    init(backend: EditBackend) { self.backend = backend }

    func start(_ item: FilerTransferItem, policy: FilerConflictPolicy) throws -> FilerTransferHandle {
        let session = session
        starts.append((session, item, policy))
        let handle = EditHandle()
        let gate = item.kind == .upload ? uploadGate : downloadGate
        let failed = item.kind == .upload && uploadFailures > 0
        if failed { uploadFailures -= 1 }
        let result = item.kind == .download ? downloadResult : nil
        handle.task = Task {
            await gate?.wait()
            guard !Task.isCancelled else { return }
            do {
                handle.continuation.yield(.started(total: 100))
                handle.continuation.yield(.progress(done: 50, total: 100))
                if let result {
                    handle.continuation.yield(.finished(result))
                } else if failed {
                    handle.continuation.yield(.finished(.failed(message: "传输断开", alreadyExists: false)))
                } else {
                    if item.kind == .download {
                        try backend.read(session: session, path: item.source).write(to: URL(fileURLWithPath: item.destination).appendingPathComponent(item.name))
                    } else {
                        let path = FilerPath.join(item.destination, item.name)
                        if policy == .fail, (try? backend.read(session: session, path: path)) != nil { throw FilerError(.alreadyExists) }
                        backend.put(try Data(contentsOf: URL(fileURLWithPath: item.source)), session: session, path: path)
                    }
                    handle.continuation.yield(.finished(.completed(skipped: 0)))
                }
            } catch {
                handle.continuation.yield(.finished(.failed(message: error.localizedDescription, alreadyExists: true)))
            }
            handle.continuation.finish()
        }
        return handle
    }
}

@MainActor
private final class EditEnvironment: FilerEnvironment {
    let engine: EditEngine
    init(engine: EditEngine) { self.engine = engine }
    func dataSource(for tab: FilerTabSnapshot) -> FilerDataSource { FakeDataSource(remote: true) }
    func transferEngine(for tab: FilerTabSnapshot) -> FilerTransferEngine { engine.session = tab.session; return engine }
    var localCopyEngine: FilerTransferEngine { engine }
    func workingDirectory(session: SessionID) async throws -> String { "/d" }
    func localHostNames() -> Set<String> { [] }
}

@MainActor
final class RemoteEditTests: XCTestCase {
    private var root: URL!
    private var managers: [RemoteEditManager] = []
    private let tab = FilerFixtures.tab(1, kind: .ssh, name: "server")
    private let entry = FilerEntry.file("config.txt")

    override func setUp() async throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("remote-edit-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        for manager in managers { _ = manager.close(sessions: Array(Set(manager.edits.keys.map(\.session)))) }
        managers = []
        try FileManager.default.removeItem(at: root)
    }

    private func make(
        delay: Duration = .milliseconds(30), sleep: @escaping (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) throws -> (RemoteEditManager, EditEngine, EditBackend, FakePrompter) {
        let backend = EditBackend()
        backend.put("remote")
        let engine = EditEngine(backend: backend)
        let prompter = FakePrompter()
        let manager = try RemoteEditManager(root: root, environment: EditEnvironment(engine: engine), backend: backend,
                                            prompter: prompter, debounceDelay: delay, sleep: sleep)
        manager.updateSessions([tab])
        managers.append(manager)
        return (manager, engine, backend, prompter)
    }

    private func edit(_ manager: RemoteEditManager) throws -> RemoteEditManager.Edit {
        try XCTUnwrap(manager.edits[.init(session: 1, path: entry.path)])
    }

    private func save(_ edit: RemoteEditManager.Edit, _ text: String, atomic: Bool = true) throws {
        try Data(text.utf8).write(to: edit.file, options: atomic ? .atomic : [])
    }

    private func waitFor(_ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(5)
        while !condition(), ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(condition())
    }

    func testOpenDownloadsBeforeLaunchAndIsolatesSessionsAndPaths() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let original = try edit(manager)
        XCTAssertEqual(try Data(contentsOf: original.file), Data("remote".utf8))
        XCTAssertEqual(prompter.opened, [original.file.path])
        XCTAssertNotNil(original.watcher)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(engine.uploads.isEmpty)
        backend.put("other", path: "/other/config.txt")
        backend.put("second", session: 2)
        let second = FilerFixtures.tab(2, kind: .ssh, name: "second")
        manager.updateSessions([tab, second])
        await manager.open(.file("config.txt", in: "/other"), tab: tab)
        await manager.open(entry, tab: second)
        XCTAssertEqual(Set(manager.edits.values.map(\.file)).count, 3)
        XCTAssertTrue(manager.edits.values.allSatisfy { $0.file.lastPathComponent == "config.txt" })
    }

    func testChooseApplicationCancelsBeforeDownloadAndRecentsAreBounded() async throws {
        let (manager, engine, _, prompter) = try make()
        await manager.open(entry, tab: tab, chooseApplication: true)
        XCTAssertTrue(engine.starts.isEmpty)
        for index in 0..<7 {
            prompter.editorApplication = root.appendingPathComponent("Editor\(index).app")
            await manager.open(entry, tab: tab, chooseApplication: true)
        }
        let last = try XCTUnwrap(prompter.editorApplication)
        XCTAssertEqual(manager.recentApplications.count, 6)
        XCTAssertEqual(manager.recentApplications.first, last)
        await manager.open(entry, tab: tab, application: manager.recentApplications.last)
        XCTAssertEqual(manager.recentApplications.count, 6)
        XCTAssertEqual(prompter.remoteApplications.last!, manager.recentApplications.first)
    }

    func testDownloadFailureAndCancellationNeverLaunchOrWatch() async throws {
        let (manager, engine, _, prompter) = try make()
        for result in [FilerTransferResult.failed(message: "失败", alreadyExists: false), .cancelled] {
            engine.downloadResult = result
            await manager.open(entry, tab: tab)
            XCTAssertTrue(manager.edits.isEmpty)
            XCTAssertTrue(prompter.opened.isEmpty)
        }
    }

    func testLaunchFailureRemainsVisibleAndDoesNotEnterRecents() async throws {
        let (manager, _, _, prompter) = try make()
        prompter.openOK = false
        await manager.open(entry, tab: tab, application: root.appendingPathComponent("Editor.app"))
        XCTAssertTrue(manager.status(for: 1).0?.text.contains("无法启动应用") == true)
        XCTAssertTrue(manager.status(for: 1).0?.isError == true)
        XCTAssertTrue(manager.recentApplications.isEmpty)
        XCTAssertNotNil(try edit(manager).watcher)
    }

    func testReopenDownloadFailureAndCancellationPreserveExistingCopyAndMonitoring() async throws {
        let (manager, engine, _, _) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        for result in [FilerTransferResult.failed(message: "失败", alreadyExists: false), .cancelled] {
            engine.downloadResult = result
            await manager.open(entry, tab: tab)
            XCTAssertEqual(try Data(contentsOf: edit.file), Data("remote".utf8))
            XCTAssertNotNil(edit.watcher)
        }
        try save(edit, "save after failed reopen")
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
    }

    func testDownloadProgressAndCancelUseExistingTransferHandle() async throws {
        let (manager, engine, _, prompter) = try make()
        let gate = Gate()
        engine.downloadGate = gate
        let opening = Task { await manager.open(entry, tab: tab) }
        try await waitFor { engine.downloads.count == 1 }
        XCTAssertNotNil(manager.status(for: 1).1)
        manager.cancelDownload(session: 1)
        await opening.value
        XCTAssertTrue(manager.edits.isEmpty)
        XCTAssertTrue(prompter.opened.isEmpty)
        gate.open()
    }

    func testRealEventsDebounceAndKeepWatchingAfterAtomicReplacement() async throws {
        let (manager, engine, backend, _) = try make(delay: .milliseconds(300))
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        try save(edit, "first", atomic: false)
        try await Task.sleep(for: .milliseconds(150))
        try save(edit, "second replacement")
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertTrue(engine.uploads.isEmpty)
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
        XCTAssertEqual(try backend.read(), Data("second replacement".utf8))
        try save(edit, "third replacement")
        try await waitFor { engine.uploads.count == 2 && edit.phase == .synced }
        try Data("backup".utf8).write(to: edit.file.deletingLastPathComponent().appendingPathComponent("config.txt~"))
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: edit.file.path)
        try await Task.sleep(for: .milliseconds(450))
        XCTAssertEqual(engine.uploads.count, 2)
    }

    func testSaveDuringUploadSendsSecondSnapshotToOriginalSession() async throws {
        let (manager, engine, backend, _) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        let gate = Gate()
        engine.uploadGate = gate
        try save(edit, "first save")
        try await waitFor { engine.uploads.count == 1 }
        try save(edit, "second save while uploading")
        try await Task.sleep(for: .milliseconds(80))
        manager.updateSessions([tab, FilerFixtures.tab(2, kind: .ssh, name: "other")])
        engine.uploadGate = nil
        gate.open()
        try await waitFor { engine.uploads.count == 2 && edit.phase == .synced }
        XCTAssertEqual(try backend.read(), Data("second save while uploading".utf8))
        XCTAssertTrue(engine.starts.allSatisfy { $0.0 == 1 })
    }

    func testRemoteChangeOverwriteAndRefresh() async throws {
        let (manager, engine, backend, prompter) = try make()
        var refreshed: [String] = []
        manager.onSynced = { session, directory in refreshed.append("\(session):\(directory)") }
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        backend.put("changed by someone else")
        prompter.remoteEditChoices = [.overwrite]
        try save(edit, "local override")
        try await waitFor { edit.phase == .synced && engine.uploads.count == 1 }
        XCTAssertEqual(try backend.read(), Data("local override".utf8))
        XCTAssertEqual(refreshed, ["1:/d"])
        try save(edit, "another local save")
        try await waitFor { engine.uploads.count == 2 && edit.phase == .synced }
        XCTAssertEqual(prompter.remoteEditConflicts, 1)
    }

    func testRemoteChangeRedownloadDiscardsOnlyAfterExplicitChoice() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        backend.put("new remote")
        prompter.remoteEditChoices = [.redownload]
        try save(edit, "discarded local")
        try await waitFor { engine.downloads.count == 2 && edit.phase == .synced }
        XCTAssertEqual(try Data(contentsOf: edit.file), Data("new remote".utf8))
        XCTAssertTrue(engine.uploads.isEmpty)
        try save(edit, "save after redownload")
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
        XCTAssertEqual(prompter.remoteEditConflicts, 1)
    }

    func testRemoteChangeSaveCopyNeverOverwritesOriginal() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        backend.put("someone else's content")
        prompter.remoteEditChoices = [.saveCopy("config-copy.txt")]
        try save(edit, "local copy")
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
        XCTAssertEqual(try backend.read(), Data("someone else's content".utf8))
        XCTAssertEqual(try backend.read(path: "/d/config-copy.txt"), Data("local copy".utf8))
        XCTAssertEqual(engine.starts.last?.2, .fail)
    }

    func testSaveCopyCollisionKeepsLocalChangesAndPausesWithoutRepeatedPrompts() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        backend.put("remote conflict")
        backend.put("existing copy", path: "/d/config-copy.txt")
        prompter.remoteEditChoices = [.saveCopy("config-copy.txt")]
        try save(edit, "unsent local copy")
        try await waitFor { engine.uploads.count == 1 && edit.job == nil }
        XCTAssertEqual(edit.phase, .conflict)
        XCTAssertEqual(try backend.read(), Data("remote conflict".utf8))
        XCTAssertEqual(try backend.read(path: "/d/config-copy.txt"), Data("existing copy".utf8))
        XCTAssertEqual(try Data(contentsOf: edit.file), Data("unsent local copy".utf8))
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(prompter.remoteEditConflicts, 1)
        XCTAssertEqual(manager.close(sessions: [1]), [edit.file])
    }

    func testCancelConflictPausesUntilUserReopensAndNeverLosesPendingCopy() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        backend.put("remote conflict")
        try save(edit, "pending local")
        try await waitFor { prompter.remoteEditConflicts == 1 && edit.job == nil }
        try save(edit, "newer pending local")
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(prompter.remoteEditConflicts, 1)
        XCTAssertTrue(engine.uploads.isEmpty)
        prompter.remoteEditChoices = [.overwrite]
        await manager.open(entry, tab: tab)
        try await waitFor { edit.phase == .synced }
        XCTAssertEqual(engine.downloads.count, 1)
        XCTAssertEqual(try backend.read(), Data("newer pending local".utf8))
    }

    func testLargeFileWarningBeforeDownloadAndBoundary() async throws {
        let (manager, engine, backend, prompter) = try make()
        backend.reportedSize = 50_000_001
        prompter.largeEditAnswer = false
        await manager.open(entry, tab: tab)
        XCTAssertEqual(prompter.largeEditWarnings, 1)
        XCTAssertTrue(engine.downloads.isEmpty)
        backend.reportedSize = 50_000_000
        await manager.open(entry, tab: tab)
        XCTAssertEqual(engine.downloads.count, 1)
        XCTAssertEqual(prompter.largeEditWarnings, 1)
        backend.reportedSize = 50_000_001
        prompter.largeEditAnswer = true
        await manager.open(entry, tab: tab)
        XCTAssertEqual(engine.downloads.count, 2)
        XCTAssertEqual(prompter.largeEditWarnings, 2)
    }

    func testDisconnectedSaveRetriesAfterOriginalSessionReconnectAndChecksRemoteVersion() async throws {
        let (manager, engine, backend, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        let closed = FilerFixtures.tab(1, kind: .ssh, name: "server", connection: .closed(reason: "offline"))
        manager.updateSessions([closed])
        try save(edit, "offline save")
        try await waitFor { edit.phase == .pending }
        let other = FilerFixtures.tab(2, kind: .ssh, name: "server")
        manager.updateSessions([closed, other])
        try await Task.sleep(for: .milliseconds(80))
        XCTAssertTrue(engine.uploads.isEmpty)
        backend.put("changed while offline")
        prompter.remoteEditChoices = [.overwrite]
        manager.updateSessions([tab, other])
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
        XCTAssertEqual(prompter.remoteEditConflicts, 1)
        XCTAssertEqual(try backend.read(), Data("offline save".utf8))
        XCTAssertEqual(engine.starts.last?.0, 1)
    }

    func testRetryBackoffAndNewSaveWakesRetry() async throws {
        var delays: [Duration] = []
        var gates: [Gate] = []
        let (manager, engine, backend, _) = try make { duration in
            if duration >= .seconds(6) {
                delays.append(duration)
                let gate = Gate()
                gates.append(gate)
                await gate.wait()
            } else { try await Task.sleep(for: duration) }
        }
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        engine.uploadFailures = 2
        try save(edit, "retry content")
        try await waitFor { delays.count == 1 }
        gates[0].open()
        try await waitFor { delays.count == 2 }
        XCTAssertEqual(delays, [.seconds(6), .seconds(12)])
        try save(edit, "new save wakes retry")
        try await waitFor { engine.uploads.count == 3 && edit.phase == .synced }
        XCTAssertEqual(try backend.read(), Data("new save wakes retry".utf8))
        XCTAssertEqual((0..<7).map(RemoteEditManager.retryDelay), [6, 12, 24, 48, 60, 60, 60].map { .seconds($0) })
        gates[1].open()
    }

    func testCloseBeforeDebouncePreservesAndReportsPendingCopyAcrossStartupCleanup() async throws {
        let (manager, engine, _, prompter) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        try save(edit, "unsent changes")
        let retained = manager.close(sessions: [1])
        await prompter.showRetainedRemoteEdits(retained)
        XCTAssertEqual(retained, [edit.file])
        XCTAssertEqual(prompter.retainedCopies, retained)
        try RemoteEditFiles.cleanPreviousRuns(in: root)
        XCTAssertEqual(try Data(contentsOf: edit.file), Data("unsent changes".utf8))
        XCTAssertTrue(manager.edits.isEmpty)
        XCTAssertNil(edit.watcher)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(engine.uploads.isEmpty)
    }

    func testStartupRemovesUnchangedSyncedCopy() async throws {
        let record = root.appendingPathComponent("previous/1/record")
        let content = record.appendingPathComponent("content")
        try FileManager.default.createDirectory(at: content, withIntermediateDirectories: true)
        let file = content.appendingPathComponent("config.txt")
        try Data("synced".utf8).write(to: file)
        try RemoteEditFiles.markSynced(file, version: FilerFileVersion.local(file))
        let (manager, _, _, _) = try make()
        XCTAssertTrue(FileManager.default.fileExists(atPath: record.path))
        try await manager.cleanPreviousRuns()
        XCTAssertFalse(FileManager.default.fileExists(atPath: record.path))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path).filter { $0 != "previous" }.count, 1)
    }

    func testCloseDuringUploadRetainsCopyAndSyncedCloseCleansCopy() async throws {
        let (manager, engine, _, _) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        let gate = Gate()
        engine.uploadGate = gate
        try save(edit, "upload still pending")
        try await waitFor { engine.uploads.count == 1 }
        XCTAssertEqual(manager.close(sessions: [1]), [edit.file])
        gate.open()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(FileManager.default.fileExists(atPath: edit.file.path))
        engine.uploadGate = nil
        manager.updateSessions([tab])
        await manager.open(entry, tab: tab)
        let synced = try self.edit(manager)
        XCTAssertTrue(manager.close(sessions: [1]).isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: synced.file.path))
    }

    func testReopenPendingEditUsesSameCopyWithoutDownloading() async throws {
        let (manager, engine, _, prompter) = try make(delay: .seconds(10))
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        try save(edit, "pending")
        await manager.open(entry, tab: tab)
        XCTAssertEqual(engine.downloads.count, 1)
        XCTAssertEqual(prompter.opened, [edit.file.path, edit.file.path])
        XCTAssertEqual(try Data(contentsOf: edit.file), Data("pending".utf8))
    }

    func testSaveDuringReopenDownloadKeepsLocalChangesAndUploadsThem() async throws {
        let (manager, engine, backend, _) = try make()
        await manager.open(entry, tab: tab)
        let edit = try edit(manager)
        let gate = Gate()
        engine.downloadGate = gate
        let reopening = Task { await manager.open(entry, tab: tab) }
        try await waitFor { engine.downloads.count == 2 }
        try save(edit, "saved during download")
        gate.open()
        await reopening.value
        XCTAssertEqual(try Data(contentsOf: edit.file), Data("saved during download".utf8))
        XCTAssertNotNil(edit.watcher)
        try await waitFor { engine.uploads.count == 1 && edit.phase == .synced }
        XCTAssertEqual(try backend.read(), Data("saved during download".utf8))
    }
}
