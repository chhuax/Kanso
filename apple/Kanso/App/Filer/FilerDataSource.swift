import Foundation
import KansoBridge
import KansoUI

// 面板的数据来源抽象（filer.md §1.2）：本地用 FileManager / POSIX，远端用 core 的 SFTP。
// 面板与 view model 只依赖 `FilerDataSource`，测试注入内存实现，不碰真实文件系统与网络。

/// 符号链接激活时确认出的目标类型（filer.md §4.4）。
enum FilerLinkTarget: Equatable, Sendable {
    /// 指向可读目录，附目标路径（用于进入）。
    case directory(path: String)
    case file
}

protocol FilerDataSource: Sendable {
    var isRemote: Bool { get }
    func homeDirectory() async throws -> String
    /// 流式列出目录：每个元素是一批条目（未排序）。出错时流以错误结束；消费者取消时必须停止读取。
    func list(path: String) -> AsyncThrowingStream<[FilerEntry], Error>
    /// 确认链接目标。悬空或不可访问时抛 `FilerError`。
    func resolveLink(_ entry: FilerEntry) async throws -> FilerLinkTarget
    /// 新建空文件；已存在必须失败（`alreadyExists`），不得清空已有内容（ssh.md §8）。
    func createFile(at path: String) async throws
    func createDirectory(at path: String) async throws
    /// 删除文件、链接或**空**目录；非空目录抛 `notEmpty`。删除链接只删链接本身。
    /// `entry` 是用户确认删除时看到的那一项：实现按路径重新核对类型，已消失抛 `notFound`，
    /// 类型已变化（确认时是文件、现在是目录）拒绝，避免作用于后来占用同一路径的另一项（filer.md §5.4）。
    func remove(_ entry: FilerEntry) async throws
    /// 重命名；目标已存在必须失败，不得覆盖。
    func rename(from: String, to: String) async throws
}

// MARK: - 本地

/// 本机文件。读取在后台执行（`Task.detached`），主线程只接收分批结果。
struct LocalFilerDataSource: FilerDataSource {
    /// 每批条目数：首批尽快出现，后续批次不频繁打断界面。
    static let batchSize = 200

    var isRemote: Bool { false }

    func homeDirectory() async throws -> String { NSHomeDirectory() }

    func list(path: String) -> AsyncThrowingStream<[FilerEntry], Error> {
        list(path: path, maxEntries: .max)
    }

    func list(path: String, maxEntries: Int) -> AsyncThrowingStream<[FilerEntry], Error> {
        AsyncThrowingStream { continuation in
            let task = Task.detached {
                do {
                    // 先 stat 目录本身：区分"不存在 / 是文件 / 无权限"，`contentsOfDirectory` 的错误码不够细。
                    var st = stat()
                    guard stat(path, &st) == 0 else { throw FilerError.fromErrno(errno, path: path) }
                    guard (st.st_mode & S_IFMT) == S_IFDIR else { throw FilerError(.notADirectory, path: path) }
                    guard let directory = opendir(path) else { throw FilerError.fromErrno(errno, path: path) }
                    defer { closedir(directory) }
                    var batch: [FilerEntry] = []
                    var count = 0
                    while count < maxEntries {
                        try Task.checkCancellation()
                        errno = 0
                        guard let item = readdir(directory) else {
                            if errno != 0 { throw FilerError.fromErrno(errno, path: path) }
                            break
                        }
                        let name = withUnsafePointer(to: item.pointee.d_name) {
                            $0.withMemoryRebound(to: CChar.self, capacity: Int(item.pointee.d_namlen) + 1) { String(cString: $0) }
                        }
                        if name == "." || name == ".." { continue }
                        count += 1
                        batch.append(Self.entry(named: name, in: path))
                        if batch.count >= Self.batchSize {
                            continuation.yield(batch)
                            batch = []
                        }
                    }
                    if !batch.isEmpty { continuation.yield(batch) }
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// 读取单个条目的属性。**属性读不到时名称仍然保留**（filer.md §4.4：不能悄悄丢掉条目后说目录为空）。
    static func entry(named name: String, in dir: String) -> FilerEntry {
        let path = FilerPath.join(dir, name)
        var st = stat()
        guard lstat(path, &st) == 0 else {
            return FilerEntry(name: name, path: path, isDirectory: false)
        }
        let type = st.st_mode & S_IFMT
        let seconds = TimeInterval(st.st_mtimespec.tv_sec) + TimeInterval(st.st_mtimespec.tv_nsec) / 1e9
        return FilerEntry(
            name: name, path: path, isDirectory: type == S_IFDIR, isSymlink: type == S_IFLNK,
            size: type == S_IFDIR ? nil : UInt64(max(0, st.st_size)), modified: Date(timeIntervalSince1970: seconds),
            permissions: UInt32(st.st_mode & 0o7777),
            owner: getpwuid(st.st_uid).map { String(cString: $0.pointee.pw_name) } ?? String(st.st_uid),
            group: getgrgid(st.st_gid).map { String(cString: $0.pointee.gr_name) } ?? String(st.st_gid)
        )
    }

    func resolveLink(_ entry: FilerEntry) async throws -> FilerLinkTarget {
        var st = stat()
        guard stat(entry.path, &st) == 0 else { throw FilerError.fromErrno(errno, path: entry.path) }
        if (st.st_mode & S_IFMT) == S_IFDIR {
            // 目录还要能读：无权限时在这里说明，而不是进入后才报错。
            guard access(entry.path, R_OK | X_OK) == 0 else { throw FilerError.fromErrno(errno, path: entry.path) }
            return .directory(path: entry.path)
        }
        return .file
    }

    func createFile(at path: String) async throws {
        // O_EXCL：名字被并发创建时失败而不是截断对方的文件。
        let fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0o644)
        guard fd >= 0 else { throw FilerError.fromErrno(errno, path: path) }
        close(fd)
    }

    func createDirectory(at path: String) async throws {
        guard mkdir(path, 0o755) == 0 else { throw FilerError.fromErrno(errno, path: path) }
    }

    func remove(_ entry: FilerEntry) async throws {
        var st = stat()
        guard lstat(entry.path, &st) == 0 else { throw FilerError.fromErrno(errno, path: entry.path) }
        let isDir = (st.st_mode & S_IFMT) == S_IFDIR
        guard isDir == entry.isDirectory else {
            throw FilerError(.other(String(localized: "The item changed after you confirmed; nothing was deleted")), path: entry.path)
        }
        // 直接用 unlink / rmdir 而不是 `FileManager.removeItem`：后者会递归删除非空目录；
        // 这里要的是"永远不递归、不进废纸篓、链接只删链接"。rmdir 对非空目录返回 ENOTEMPTY。
        let result = isDir ? rmdir(entry.path) : unlink(entry.path)
        guard result == 0 else { throw FilerError.fromErrno(errno, path: entry.path) }
    }

    func rename(from: String, to: String) async throws {
        // RENAME_EXCL：目标存在时失败。普通 rename(2) 会静默覆盖目标。
        guard renamex_np(from, to, UInt32(RENAME_EXCL)) == 0 else { throw FilerError.fromErrno(errno, path: to) }
    }
}

// MARK: - 远端

/// 远端目录流的最小接口（`FfiDirStream` 的抽象，测试可替换）。
protocol FilerRemoteDirStream: Sendable {
    func nextBatch() async throws -> [FilerEntry]?
}

/// core SFTP 操作的抽象。生产实现 `CoreRemoteBackend` 转给 `KansoCore`；测试注入内存实现。
protocol FilerRemoteBackend: Sendable {
    func home(session: UInt64) async throws -> String
    func stat(session: UInt64, path: String) async throws -> FilerFileVersion
    func list(session: UInt64, path: String) throws -> FilerRemoteDirStream
    func mkdir(session: UInt64, path: String) async throws
    func createFile(session: UInt64, path: String) async throws
    func remove(session: UInt64, path: String, isDirectory: Bool) async throws
    func rename(session: UInt64, from: String, to: String) async throws
}

struct CoreRemoteBackend: FilerRemoteBackend {
    let core: KansoCore

    private struct Stream: FilerRemoteDirStream {
        let stream: FfiDirStream
        func nextBatch() async throws -> [FilerEntry]? {
            try await stream.nextBatch()?.map(CoreRemoteBackend.entry)
        }
    }

    /// `FfiDirEntry.size` 不是可选值：服务器没给属性时 core 填 0。整条属性都缺（没有时间也没有权限）
    /// 时按"大小未知"处理，避免把未知显示成 0 字节（filer.md §4.1）。
    static func entry(_ e: FfiDirEntry) -> FilerEntry {
        let attributesMissing = e.mtimeMs == nil && e.permissions == nil
        return FilerEntry(
            name: e.name, path: e.path, isDirectory: e.isDir && !e.isSymlink, isSymlink: e.isSymlink,
            size: (e.isDir || attributesMissing) ? nil : e.size,
            modified: e.mtimeMs.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) },
            permissions: e.permissions, owner: e.owner, group: e.group
        )
    }

    func home(session: UInt64) async throws -> String { try await core.sftpHome(sessionId: session) }

    func stat(session: UInt64, path: String) async throws -> FilerFileVersion {
        let value = try await core.sftpStat(sessionId: session, path: path)
        return FilerFileVersion(size: value.size, modified: value.mtimeMs.map { Date(timeIntervalSince1970: Double($0) / 1000) })
    }

    func list(session: UInt64, path: String) throws -> FilerRemoteDirStream {
        Stream(stream: try core.sftpList(sessionId: session, path: path))
    }

    func mkdir(session: UInt64, path: String) async throws { try await core.sftpMkdir(sessionId: session, path: path) }
    func createFile(session: UInt64, path: String) async throws { try await core.sftpCreateFile(sessionId: session, path: path) }
    func remove(session: UInt64, path: String, isDirectory: Bool) async throws {
        try await core.sftpRemove(sessionId: session, path: path, isDir: isDirectory)
    }
    func rename(session: UInt64, from: String, to: String) async throws {
        try await core.sftpRename(sessionId: session, from: from, to: to)
    }
}

extension FilerError {
    /// core 错误 → 归类。远端 SFTP 失败的原因在消息参数 `reason` 里（`reason-not-found` 等）。
    static func fromCore(_ error: Error, path: String?) -> FilerError {
        if let e = error as? FilerError { return e }
        guard let core = error as? FfiCoreError else { return from(error, path: path) }
        switch core {
        case .User(let message):
            switch message.key {
            case "sftp-file-exists": return FilerError(.alreadyExists, path: path)
            case "xfer-session-ended": return FilerError(.disconnected, path: path)
            default: break
            }
            for arg in message.args where arg.name == "reason" {
                if case .reason(let key, _) = arg.value {
                    if key == "reason-not-found" { return FilerError(.notFound, path: path) }
                    if key == "reason-permission-denied" { return FilerError(.permissionDenied, path: path) }
                }
            }
            return FilerError(.other(CoreMessageText.text(message)), path: path)
        case .NoSuchSession:
            return FilerError(.disconnected, path: path)
        default:
            return FilerError(.other(String(describing: core)), path: path)
        }
    }
}

struct RemoteFilerDataSource: FilerDataSource {
    let session: UInt64
    let backend: FilerRemoteBackend

    var isRemote: Bool { true }

    func homeDirectory() async throws -> String {
        do { return try await backend.home(session: session) } catch { throw FilerError.fromCore(error, path: nil) }
    }

    func list(path: String) -> AsyncThrowingStream<[FilerEntry], Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    let stream = try backend.list(session: session, path: path)
                    while let batch = try await stream.nextBatch() {
                        try Task.checkCancellation()
                        continuation.yield(batch)
                    }
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: FilerError.fromCore(error, path: path))
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// core 没有单独的 stat：按目录尝试列出来确认目标类型。能列出就是可读目录；
    /// 列不出（目标是文件、悬空）无法进一步区分，统一按"不是可读目录"处理（`.file`），
    /// 由调用方给出说明——远端文件的"打开"属于 F1c，不会因此去下载。权限不足与断线仍如实抛出。
    func resolveLink(_ entry: FilerEntry) async throws -> FilerLinkTarget {
        do {
            let stream = try backend.list(session: session, path: entry.path)
            _ = try await stream.nextBatch()
            return .directory(path: entry.path)
        } catch {
            let mapped = FilerError.fromCore(error, path: entry.path)
            switch mapped.kind {
            case .permissionDenied, .disconnected: throw mapped
            default: return .file
            }
        }
    }

    func createFile(at path: String) async throws {
        do { try await backend.createFile(session: session, path: path) } catch { throw FilerError.fromCore(error, path: path) }
    }

    func createDirectory(at path: String) async throws {
        do { try await backend.mkdir(session: session, path: path) } catch { throw FilerError.fromCore(error, path: path) }
    }

    func remove(_ entry: FilerEntry) async throws {
        if entry.isDirectory {
            // core 的 `sftp_remove(is_dir)` 是递归的，与 §5.4"不递归删除非空目录"冲突：删除前先确认目录为空。
            // 检查与删除之间目录被并发写入的窗口无法消除，但覆盖了用户可见的常见情形。
            do {
                let stream = try backend.list(session: session, path: entry.path)
                if let first = try await stream.nextBatch(), !first.isEmpty { throw FilerError(.notEmpty, path: entry.path) }
            } catch {
                throw FilerError.fromCore(error, path: entry.path)
            }
        }
        do {
            try await backend.remove(session: session, path: entry.path, isDirectory: entry.isDirectory)
        } catch {
            throw FilerError.fromCore(error, path: entry.path)
        }
    }

    func rename(from: String, to: String) async throws {
        do { try await backend.rename(session: session, from: from, to: to) } catch { throw FilerError.fromCore(error, path: to) }
    }
}
