import KansoBridge
@testable import Kanso
import XCTest

/// 本地数据源、本地复制与远端数据源（假 backend）。本地文件操作只在临时目录里做。
@MainActor
final class FilerFileSystemTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("filer-fs-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    private func path(_ name: String) -> String { root.appendingPathComponent(name).path }
    private func write(_ name: String, _ text: String = "x") throws {
        let url = root.appendingPathComponent(name)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: url, atomically: true, encoding: .utf8)
    }

    private func collect(_ ds: FilerDataSource, _ dir: String) async throws -> [[FilerEntry]] {
        var batches: [[FilerEntry]] = []
        for try await b in ds.list(path: dir) { batches.append(b) }
        return batches
    }

    func testLocalListBatchesAndAttributes() async throws {
        for i in 0..<(LocalFilerDataSource.batchSize + 5) { try write("f\(i)") }
        try FileManager.default.createDirectory(atPath: path("dir"), withIntermediateDirectories: false)
        let batches = try await collect(LocalFilerDataSource(), root.path)
        XCTAssertGreaterThan(batches.count, 1, "大目录分批")
        let all = batches.flatMap { $0 }
        XCTAssertEqual(all.count, LocalFilerDataSource.batchSize + 6)
        let dir = try XCTUnwrap(all.first { $0.name == "dir" })
        XCTAssertTrue(dir.isDirectory)
        XCTAssertNil(dir.size)
        let file = try XCTUnwrap(all.first { $0.name == "f0" })
        XCTAssertEqual(file.size, 1)
        XCTAssertNotNil(file.modified)
        XCTAssertNotNil(file.permissions)
    }

    func testLocalListErrorsAreDistinct() async throws {
        let ds = LocalFilerDataSource()
        try write("plain")
        for (target, kind) in [(path("missing"), FilerFailureKind.notFound), (path("plain"), .notADirectory)] {
            do { _ = try await collect(ds, target); XCTFail("应失败") } catch {
                XCTAssertEqual((error as? FilerError)?.kind, kind)
            }
        }
        let locked = path("locked")
        try FileManager.default.createDirectory(atPath: locked, withIntermediateDirectories: false)
        chmod(locked, 0)
        defer { chmod(locked, 0o755) }
        do { _ = try await collect(ds, locked); XCTFail("应失败") } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .permissionDenied)
        }
    }

    func testEntryWithUnreadableAttributesKeepsName() {
        let e = LocalFilerDataSource.entry(named: "ghost", in: root.path)
        XCTAssertEqual(e.name, "ghost")
        XCTAssertNil(e.size)
        XCTAssertNil(e.modified)
    }

    func testCreateIsExclusiveAndNeverTruncates() async throws {
        let ds = LocalFilerDataSource()
        try write("keep", "important")
        do { try await ds.createFile(at: path("keep")); XCTFail("应失败") } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .alreadyExists)
        }
        XCTAssertEqual(try String(contentsOfFile: path("keep"), encoding: .utf8), "important")
        try await ds.createFile(at: path("new"))
        try await ds.createDirectory(at: path("newdir"))
        do { try await ds.createDirectory(at: path("newdir")); XCTFail() } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .alreadyExists)
        }
    }

    func testRenameRefusesToOverwrite() async throws {
        let ds = LocalFilerDataSource()
        try write("a", "A"); try write("b", "B")
        do { try await ds.rename(from: path("a"), to: path("b")); XCTFail("应失败") } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .alreadyExists)
        }
        XCTAssertEqual(try String(contentsOfFile: path("b"), encoding: .utf8), "B")
        try await ds.rename(from: path("a"), to: path("c"))
        XCTAssertTrue(FileManager.default.fileExists(atPath: path("c")))
    }

    func testRemoveNeverRecursesAndLinkRemovalKeepsTarget() async throws {
        let ds = LocalFilerDataSource()
        try write("full/inner")
        let full = LocalFilerDataSource.entry(named: "full", in: root.path)
        do { try await ds.remove(full); XCTFail("非空目录不应被删") } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .notEmpty)
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: path("full/inner")))

        try write("target", "t")
        try FileManager.default.createSymbolicLink(atPath: path("link"), withDestinationPath: path("target"))
        let link = LocalFilerDataSource.entry(named: "link", in: root.path)
        XCTAssertTrue(link.isSymlink)
        try await ds.remove(link)
        XCTAssertFalse(FileManager.default.fileExists(atPath: path("link")))
        XCTAssertTrue(FileManager.default.fileExists(atPath: path("target")), "删除链接只删链接")

        // 已消失的项：notFound；类型变化：拒绝。
        do { try await ds.remove(link); XCTFail() } catch { XCTAssertEqual((error as? FilerError)?.kind, .notFound) }
        let staleFile = FilerEntry(name: "full", path: path("full"), isDirectory: false)
        do { try await ds.remove(staleFile); XCTFail("类型已变化不应删除") } catch {}
        XCTAssertTrue(FileManager.default.fileExists(atPath: path("full")))
        let empty = path("empty")
        try FileManager.default.createDirectory(atPath: empty, withIntermediateDirectories: false)
        try await ds.remove(LocalFilerDataSource.entry(named: "empty", in: root.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: empty))
    }

    func testResolveLinkKinds() async throws {
        let ds = LocalFilerDataSource()
        try write("f"); try FileManager.default.createDirectory(atPath: path("d"), withIntermediateDirectories: false)
        try FileManager.default.createSymbolicLink(atPath: path("lf"), withDestinationPath: path("f"))
        try FileManager.default.createSymbolicLink(atPath: path("ld"), withDestinationPath: path("d"))
        try FileManager.default.createSymbolicLink(atPath: path("lx"), withDestinationPath: path("nope"))
        let target = try await ds.resolveLink(LocalFilerDataSource.entry(named: "ld", in: root.path))
        XCTAssertEqual(target, .directory(path: path("ld")))
        let fileTarget = try await ds.resolveLink(LocalFilerDataSource.entry(named: "lf", in: root.path))
        XCTAssertEqual(fileTarget, .file)
        do { _ = try await ds.resolveLink(LocalFilerDataSource.entry(named: "lx", in: root.path)); XCTFail() } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .notFound)
        }
    }

    // MARK: 本地复制

    func testRenamedDownloadConflictPoliciesAndStagingCleanup() throws {
        for policy in [FilerConflictPolicy.fail, .skip, .overwrite] {
            let staging = path("staging-\(policy)")
            try write("staging-\(policy)/original", "new")
            try write("renamed", "old")
            let result = RemoteTransferEngine.finishStaged(
                .completed(skipped: 0), from: FilerPath.join(staging, "original"),
                to: path("renamed"), staging: staging, policy: policy
            )
            switch policy {
            case .fail:
                guard case .failed(_, alreadyExists: true) = result else { return XCTFail("\(result)") }
            case .skip:
                XCTAssertEqual(result, .completed(skipped: 1))
            case .overwrite:
                XCTAssertEqual(result, .completed(skipped: 0))
            }
            XCTAssertEqual(try String(contentsOfFile: path("renamed"), encoding: .utf8), policy == .overwrite ? "new" : "old")
            XCTAssertFalse(FileManager.default.fileExists(atPath: staging))
        }
    }

    func testRenamedDownloadWithoutConflictMovesAndCleansStaging() throws {
        for policy in [FilerConflictPolicy.fail, .skip, .overwrite] {
            let staging = path("staging-\(policy)")
            try write("staging-\(policy)/original", "new")
            let result = RemoteTransferEngine.finishStaged(
                .completed(skipped: 0), from: FilerPath.join(staging, "original"),
                to: path("renamed-\(policy)"), staging: staging, policy: policy
            )
            XCTAssertEqual(result, .completed(skipped: 0))
            XCTAssertEqual(try String(contentsOfFile: path("renamed-\(policy)"), encoding: .utf8), "new")
            XCTAssertFalse(FileManager.default.fileExists(atPath: staging))
        }
    }

    private func runCopy(_ name: String, to dest: String, policy: FilerConflictPolicy) async -> FilerTransferResult {
        let engine = LocalCopyEngine()
        let handle = try! engine.start(FilerTransferItem(kind: .copy, name: name, source: path("src/\(name)"), destination: dest, isDirectory: true), policy: policy)
        var result: FilerTransferResult?
        for await e in handle.events { if case .finished(let r) = e { result = r } }
        return result!
    }

    func testLocalCopyDirectoryMergesAndPoliciesApply() async throws {
        try write("src/proj/a.txt", "A"); try write("src/proj/sub/b.txt", "B")
        try write("dst/proj/a.txt", "old"); try write("dst/proj/keep.txt", "K")
        // Fail：写任何文件之前就失败。
        var result = await runCopy("proj", to: path("dst"), policy: .fail)
        guard case .failed(_, let exists) = result else { return XCTFail("\(result)") }
        XCTAssertTrue(exists)
        XCTAssertFalse(FileManager.default.fileExists(atPath: path("dst/proj/sub/b.txt")))
        // Skip：已存在的跳过，其余照传。
        result = await runCopy("proj", to: path("dst"), policy: .skip)
        XCTAssertEqual(result, .completed(skipped: 1))
        XCTAssertEqual(try String(contentsOfFile: path("dst/proj/a.txt"), encoding: .utf8), "old")
        XCTAssertEqual(try String(contentsOfFile: path("dst/proj/sub/b.txt"), encoding: .utf8), "B")
        // Overwrite：覆盖；接收侧多余内容保留（合并不是替换）。
        result = await runCopy("proj", to: path("dst"), policy: .overwrite)
        XCTAssertEqual(result, .completed(skipped: 0))
        XCTAssertEqual(try String(contentsOfFile: path("dst/proj/a.txt"), encoding: .utf8), "A")
        XCTAssertEqual(try String(contentsOfFile: path("dst/proj/keep.txt"), encoding: .utf8), "K")
        // 源保留。
        XCTAssertTrue(FileManager.default.fileExists(atPath: path("src/proj/a.txt")))
    }

    func testLocalCopyDoesNotWriteThroughSymlinkAndSkipsLinks() async throws {
        try write("src/d/f", "F")
        try write("outside/f", "safe")
        try FileManager.default.createDirectory(atPath: path("dst/d"), withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(atPath: path("dst/d/f"), withDestinationPath: path("outside/f"))
        let result = await runCopy("d", to: path("dst"), policy: .overwrite)
        if case .failed = result {} else { XCTFail("目标是链接应拒绝：\(result)") }
        XCTAssertEqual(try String(contentsOfFile: path("outside/f"), encoding: .utf8), "safe")
        // 源里的符号链接不被复制。
        try FileManager.default.createSymbolicLink(atPath: path("src/d/lnk"), withDestinationPath: path("outside"))
        let plan = try LocalCopier.plan(source: path("src/d"), destinationDir: path("dst2"), name: "d")
        XCTAssertEqual(plan.files.map { ($0.source as NSString).lastPathComponent }, ["f"])
    }

    func testLocalCopyBlockedDirectoriesMatchRemotePolicies() async throws {
        try write("src/proj/blocked/a", "A")
        try write("src/proj/blocked/sub/b", "B")
        try write("src/proj/blocked-peer/ok", "OK")
        try write("outside/a", "safe")
        for policy in [FilerConflictPolicy.fail, .skip, .overwrite] {
            for blocker in ["file", "link", "dangling"] {
                let destination = "dst-\(policy)-\(blocker)"
                try FileManager.default.createDirectory(atPath: path("\(destination)/proj"), withIntermediateDirectories: true)
                let blocked = path("\(destination)/proj/blocked")
                if blocker == "file" {
                    try write("\(destination)/proj/blocked", "keep")
                } else {
                    try FileManager.default.createSymbolicLink(atPath: blocked, withDestinationPath: path(blocker == "link" ? "outside" : "missing"))
                }
                let result = await runCopy("proj", to: path(destination), policy: policy)
                XCTAssertEqual(result, .completed(skipped: 1), "\(policy): \(blocker)")
                XCTAssertEqual(try String(contentsOfFile: path("\(destination)/proj/blocked-peer/ok"), encoding: .utf8), "OK")
                if blocker == "file" {
                    XCTAssertEqual(try String(contentsOfFile: blocked, encoding: .utf8), "keep")
                } else {
                    XCTAssertEqual(try FileManager.default.destinationOfSymbolicLink(atPath: blocked), path(blocker == "link" ? "outside" : "missing"))
                }
                XCTAssertEqual(try String(contentsOfFile: path("outside/a"), encoding: .utf8), "safe")
                XCTAssertFalse(FileManager.default.fileExists(atPath: path("outside/sub")))
                XCTAssertFalse(FileManager.default.fileExists(atPath: path("missing")))
            }
        }
    }

    func testLocalCopyCancelRemovesPartialFile() async throws {
        try write("src/big", "")
        let big = path("src/big")
        let fh = try FileHandle(forWritingTo: URL(fileURLWithPath: big))
        try fh.truncate(atOffset: 64 << 20)
        try fh.close()
        let engine = LocalCopyEngine()
        let handle = try engine.start(FilerTransferItem(kind: .copy, name: "big", source: big, destination: root.path, isDirectory: false), policy: .fail)
        handle.cancel()
        var result: FilerTransferResult?
        for await e in handle.events { if case .finished(let r) = e { result = r } }
        if result == .cancelled {
            XCTAssertFalse(FileManager.default.fileExists(atPath: path("big")), "取消后不留残缺文件")
        } else {
            XCTAssertEqual(result, .completed(skipped: 0))   // 取消发生在复制完成之后
        }
    }

    // MARK: 远端数据源（假 backend）

    private struct FakeStream: FilerRemoteDirStream {
        let entries: [FilerEntry]
        func nextBatch() async throws -> [FilerEntry]? { entries.isEmpty ? nil : entries }
    }

    private final class FakeBackend: FilerRemoteBackend, @unchecked Sendable {
        var dirs: [String: [FilerEntry]] = [:]
        private(set) var removed: [String] = []
        func home(session: UInt64) async throws -> String { "/home/u" }
        func stat(session: UInt64, path: String) async throws -> FilerFileVersion { FilerFileVersion(size: 1, modified: nil) }
        func list(session: UInt64, path: String) throws -> FilerRemoteDirStream { FakeStream(entries: dirs[path] ?? []) }
        func mkdir(session: UInt64, path: String) async throws {}
        func createFile(session: UInt64, path: String) async throws {}
        func remove(session: UInt64, path: String, isDirectory: Bool) async throws { removed.append(path) }
        func rename(session: UInt64, from: String, to: String) async throws {}
    }

    func testRemoteDeleteChecksEmptyBeforeCoreRecursiveRemove() async throws {
        let backend = FakeBackend()
        backend.dirs["/full"] = [.file("x", in: "/full")]
        let ds = RemoteFilerDataSource(session: 1, backend: backend)
        do { try await ds.remove(.dir("full", in: "/")); XCTFail("非空目录必须拒绝") } catch {
            XCTAssertEqual((error as? FilerError)?.kind, .notEmpty)
        }
        XCTAssertTrue(backend.removed.isEmpty, "core 的递归删除没有被调用")
        try await ds.remove(.dir("empty", in: "/"))
        XCTAssertEqual(backend.removed, ["/empty"])
        try await ds.remove(.file("f", in: "/"))
        XCTAssertEqual(backend.removed.count, 2)
    }

    func testRemoteEntryMappingKeepsUnknownSizeUnknown() {
        let known = FfiDirEntry(name: "a", path: "/a", isDir: false, isSymlink: false, size: 0, mtimeMs: 1_000, permissions: 0o644, owner: "u", group: "g")
        let unknown = FfiDirEntry(name: "b", path: "/b", isDir: false, isSymlink: false, size: 0, mtimeMs: nil, permissions: nil, owner: nil, group: nil)
        XCTAssertEqual(CoreRemoteBackend.entry(known).size, 0, "确实为空的文件仍是 0 字节")
        XCTAssertNil(CoreRemoteBackend.entry(unknown).size, "未知不显示成 0 字节")
        XCTAssertNil(CoreRemoteBackend.entry(unknown).modified)
        let link = FfiDirEntry(name: "l", path: "/l", isDir: true, isSymlink: true, size: 0, mtimeMs: 1, permissions: 0o777, owner: nil, group: nil)
        XCTAssertFalse(CoreRemoteBackend.entry(link).isDirectory, "链接即使指向目录也归为其他条目")
    }

    func testRemoteErrorMapping() {
        func msg(_ key: String, reason: String) -> FfiCoreError {
            .User(message: FfiMessage(key: key, args: [FfiMessageArg(name: "reason", value: .reason(key: reason, detail: nil))]))
        }
        XCTAssertEqual(FilerError.fromCore(msg("sftp-op-failed", reason: "reason-not-found"), path: "/x").kind, .notFound)
        XCTAssertEqual(FilerError.fromCore(msg("sftp-op-failed", reason: "reason-permission-denied"), path: "/x").kind, .permissionDenied)
        XCTAssertEqual(FilerError.fromCore(FfiCoreError.NoSuchSession, path: nil).kind, .disconnected)
        XCTAssertEqual(FilerError.fromCore(FfiCoreError.User(message: FfiMessage(key: "sftp-file-exists", args: [])), path: nil).kind, .alreadyExists)
    }

    // MARK: 图标与格式

    func testIconCategoriesAndCacheReuse() {
        func cat(_ n: String, dir: Bool = false, link: Bool = false) -> FilerIconCategory {
            FilerIconCategory.category(for: FilerEntry(name: n, path: "/\(n)", isDirectory: dir, isSymlink: link))
        }
        XCTAssertEqual(cat("a.TAR.GZ"), .archive, "多段扩展名优先且忽略大小写")
        XCTAssertEqual(cat("main.swift"), .code)
        XCTAssertEqual(cat("p.PNG"), .image)
        XCTAssertEqual(cat("x", dir: true), .folder)
        XCTAssertEqual(cat("x", link: true), .link)
        XCTAssertEqual(cat("noext"), .generic)
        let before = FilerIconCache.creations
        for _ in 0..<500 { _ = FilerIconCache.image(for: .code) }
        XCTAssertLessThanOrEqual(FilerIconCache.creations - before, 1, "同类图标不随每一行重建")
    }

    func testDateFormatAndTooltipDoNotInventMissingAttributes() throws {
        XCTAssertEqual(FilerFormat.date(nil), "—")
        let tz = TimeZone(secondsFromGMT: 8 * 3600)!
        XCTAssertEqual(FilerFormat.date(Date(timeIntervalSince1970: 1_700_000_000), timeZone: tz), "2023/11/15")
        let bare = FilerEntry(name: "n", path: "/n", isDirectory: false)
        let tip = FilerFormat.tooltip(for: bare)
        XCTAssertFalse(tip.contains("0 "), "未知大小不显示")
        XCTAssertTrue(tip.hasPrefix("/n"))
        let full = FilerEntry(name: "n", path: "/n", isDirectory: false, size: 10, permissions: 0o644, owner: "me", group: "staff")
        XCTAssertTrue(FilerFormat.tooltip(for: full).contains("644"))
        XCTAssertTrue(FilerFormat.tooltip(for: full).contains("me:staff"))
    }
}
