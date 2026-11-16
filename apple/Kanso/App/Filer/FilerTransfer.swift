import Foundation
import KansoBridge
import KansoUI

// 传输层抽象与批次执行（filer.md §6）。
// 引擎只做"启动一项传输并给出事件流"：远端走 core 的上传下载，本地复制自己流式复制。
// 批次顺序、冲突询问、失败后停止这些规则在 `FilerBatchRunner`，与具体引擎无关，可以用替身引擎测试。

/// 目标已存在时的策略，对应 Rust 的 `ConflictPolicy`。
enum FilerConflictPolicy: Sendable, Equatable {
    case fail, overwrite, skip

    var ffi: FfiConflictPolicy {
        switch self {
        case .fail: .fail
        case .overwrite: .overwrite
        case .skip: .skip
        }
    }
}

enum FilerTransferKind: Equatable, Sendable {
    case upload, download, copy
}

/// 批次中的一项。
struct FilerTransferItem: Equatable, Sendable {
    var kind: FilerTransferKind
    /// 显示用名称（最后一段）。
    var name: String
    /// 来源：上传 / 复制是本地路径，下载是远端路径。
    var source: String
    /// 目标目录：上传是远端目录，下载 / 复制是本地目录。
    var destination: String
    var isDirectory: Bool
    /// 下载时用户在保存面板里改了名字：core 只会写成"目标目录 / 远端名称"，
    /// 引擎先下载到目标目录下的暂存子目录，成功后再改名到这个文件名（失败或取消时清理暂存）。
    var renamedTo: String?
    /// 首次发起时用的策略。默认 `fail`（遇到已存在先询问）；保存面板里用户已确认替换时为 `overwrite`。
    var initialPolicy: FilerConflictPolicy = .fail

    init(kind: FilerTransferKind, name: String, source: String, destination: String, isDirectory: Bool = false, renamedTo: String? = nil) {
        self.kind = kind
        self.name = name
        self.source = source
        self.destination = destination
        self.isDirectory = isDirectory
        self.renamedTo = renamedTo
    }
}

enum FilerTransferResult: Equatable, Sendable {
    /// `skipped`：因"已存在且选择跳过"而跳过的文件数。
    case completed(skipped: Int)
    case cancelled
    case failed(message: String, alreadyExists: Bool)
}

enum FilerTransferEvent: Equatable, Sendable {
    case started(total: UInt64?)
    case progress(done: UInt64, total: UInt64?)
    case finished(FilerTransferResult)
}

@MainActor
protocol FilerTransferHandle: AnyObject {
    /// 事件流：恰好以一个 `.finished` 结束。
    var events: AsyncStream<FilerTransferEvent> { get }
    func cancel()
}

@MainActor
protocol FilerTransferEngine: AnyObject {
    /// 启动一项传输。参数类错误（本地源不存在等）直接抛出，此时没有事件。
    func start(_ item: FilerTransferItem, policy: FilerConflictPolicy) throws -> FilerTransferHandle
}

// MARK: - 远端（core）

/// core 传输事件的路由：传输 id → 事件流。一个窗口一份，所有会话共用。
///
/// `start` 返回传输 id 之后才登记流是安全的：core 的事件经主队列异步投递，晚于当前这段主线程代码。
@MainActor
final class FilerTransferHub {
    private var continuations: [UInt64: AsyncStream<FilerTransferEvent>.Continuation] = [:]

    func register(_ id: UInt64, _ continuation: AsyncStream<FilerTransferEvent>.Continuation) {
        continuations[id] = continuation
    }

    /// 由 `WorkspaceController` 的事件处理链转发。与本类无关的事件忽略。
    func handle(_ event: FfiCoreEvent) {
        switch event {
        case .transferStarted(let id, _, _, _, _, let total):
            continuations[id]?.yield(.started(total: total))
        case .transferProgress(let id, let done, let total):
            continuations[id]?.yield(.progress(done: done, total: total))
        case .transferFinished(let id, let outcome):
            guard let c = continuations.removeValue(forKey: id) else { return }
            c.yield(.finished(Self.result(outcome)))
            c.finish()
        default:
            break
        }
    }

    static func result(_ outcome: FfiTransferOutcome) -> FilerTransferResult {
        switch outcome {
        case .completed(let skipped): .completed(skipped: Int(skipped))
        case .cancelled: .cancelled
        case .failed(let message, let alreadyExists):
            .failed(message: CoreMessageText.text(message), alreadyExists: alreadyExists)
        }
    }
}

/// 一个远端会话的传输引擎。
@MainActor
final class RemoteTransferEngine: FilerTransferEngine {
    private let core: KansoCore
    private let session: UInt64
    private let hub: FilerTransferHub

    init(core: KansoCore, session: UInt64, hub: FilerTransferHub) {
        self.core = core
        self.session = session
        self.hub = hub
    }

    private final class Handle: FilerTransferHandle {
        let events: AsyncStream<FilerTransferEvent>
        private let onCancel: @MainActor () -> Void
        init(events: AsyncStream<FilerTransferEvent>, onCancel: @escaping @MainActor () -> Void) {
            self.events = events
            self.onCancel = onCancel
        }
        func cancel() { onCancel() }
    }

    func start(_ item: FilerTransferItem, policy: FilerConflictPolicy) throws -> FilerTransferHandle {
        let (stream, continuation) = AsyncStream<FilerTransferEvent>.makeStream()
        let staging: String?
        if item.kind == .download, let renamed = item.renamedTo, renamed != item.name {
            staging = FilerPath.join(item.destination, ".kanso-download-\(UUID().uuidString)")
            try FileManager.default.createDirectory(atPath: staging!, withIntermediateDirectories: false)
        } else {
            staging = nil
        }
        let id: UInt64
        do {
            switch item.kind {
            case .upload:
                id = try core.upload(sessionId: session, localPath: item.source, remoteDir: item.destination, conflict: policy.ffi)
            case .download:
                id = try core.download(sessionId: session, remotePath: item.source, localDir: staging ?? item.destination, conflict: policy.ffi)
            case .copy:
                preconditionFailure("远端引擎不处理本地复制")
            }
        } catch {
            if let staging { try? FileManager.default.removeItem(atPath: staging) }
            throw error
        }
        guard let staging, let renamed = item.renamedTo else {
            hub.register(id, continuation)
            return Handle(events: stream) { [core] in core.cancelTransfer(transfer: id) }
        }
        // 改名下载：包一层，结束时把暂存里的结果移到最终位置，并清理暂存目录。
        let (outer, outerContinuation) = AsyncStream<FilerTransferEvent>.makeStream()
        let (inner, innerContinuation) = AsyncStream<FilerTransferEvent>.makeStream()
        hub.register(id, innerContinuation)
        let destination = FilerPath.join(item.destination, renamed)
        let stagedPath = FilerPath.join(staging, item.name)
        Task { @MainActor in
            for await event in inner {
                if case .finished(let result) = event {
                    let final = Self.finishStaged(result, from: stagedPath, to: destination, staging: staging, policy: policy)
                    outerContinuation.yield(.finished(final))
                    outerContinuation.finish()
                } else {
                    outerContinuation.yield(event)
                }
            }
        }
        return Handle(events: outer) { [core] in core.cancelTransfer(transfer: id) }
    }

    static func finishStaged(
        _ result: FilerTransferResult, from: String, to: String, staging: String, policy: FilerConflictPolicy
    ) -> FilerTransferResult {
        defer { try? FileManager.default.removeItem(atPath: staging) }
        guard case .completed(let skipped) = result else { return result }
        do {
            try moveStaged(from, to: to, replaceExisting: policy == .overwrite)
            return result
        } catch {
            let error = FilerError.from(error, path: to)
            if policy == .skip, error.kind == .alreadyExists { return .completed(skipped: skipped + 1) }
            return .failed(message: error.message(source: nil), alreadyExists: error.kind == .alreadyExists)
        }
    }

    /// 把暂存结果移到最终位置。`replaceExisting`（用户已确认覆盖）才替换已有文件，否则已存在即失败。
    private static func moveStaged(_ from: String, to: String, replaceExisting: Bool) throws {
        let fm = FileManager.default
        if fm.fileExists(atPath: to) {
            guard replaceExisting else { throw FilerError(.alreadyExists, path: to) }
            _ = try fm.replaceItemAt(URL(fileURLWithPath: to), withItemAt: URL(fileURLWithPath: from))
        } else {
            try fm.moveItem(atPath: from, toPath: to)
        }
    }
}

// MARK: - 本地复制

/// 复制取消标记（跨线程）。
final class FilerCancelFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    func cancel() { lock.lock(); cancelled = true; lock.unlock() }
    var isCancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }
}

/// 本地复制的执行体（后台线程调用）。目录合并；文件流式分块复制，内存与文件大小无关（filer.md §8）。
enum LocalCopier {
    struct Plan {
        var directories: [String] = []
        var files: [(source: String, destination: String, size: UInt64, mode: mode_t)] = []
        var total: UInt64 = 0
    }

    static let chunkSize = 1 << 20

    /// 扫描来源，生成复制计划。符号链接与特殊文件不复制（不跟随：不能通过链接把内容写到目录外）。
    static func plan(source: String, destinationDir: String, name: String) throws -> Plan {
        var plan = Plan()
        var st = stat()
        guard lstat(source, &st) == 0 else { throw FilerError.fromErrno(errno, path: source) }
        let target = FilerPath.join(destinationDir, name)
        switch st.st_mode & S_IFMT {
        case S_IFREG:
            plan.files.append((source, target, UInt64(max(0, st.st_size)), st.st_mode & 0o7777))
            plan.total = UInt64(max(0, st.st_size))
        case S_IFDIR:
            try scan(dir: source, into: target, plan: &plan, depth: 0)
        default:
            break
        }
        return plan
    }

    private static func scan(dir: String, into target: String, plan: inout Plan, depth: Int) throws {
        guard depth < 64 else { return }
        plan.directories.append(target)
        let names: [String]
        do { names = try FileManager.default.contentsOfDirectory(atPath: dir).sorted() } catch {
            throw FilerError.from(error, path: dir)
        }
        for name in names {
            let child = FilerPath.join(dir, name)
            var st = stat()
            guard lstat(child, &st) == 0 else { continue }
            switch st.st_mode & S_IFMT {
            case S_IFREG:
                plan.files.append((child, FilerPath.join(target, name), UInt64(max(0, st.st_size)), st.st_mode & 0o7777))
                plan.total += UInt64(max(0, st.st_size))
            case S_IFDIR:
                try scan(dir: child, into: FilerPath.join(target, name), plan: &plan, depth: depth + 1)
            default:
                continue
            }
        }
    }

    /// 执行。返回跳过的文件与目录冲突数；挡路的非目录与远端下载一样按一个冲突计数。
    /// `Fail` 时先检查未被目录冲突阻挡的文件，在写入任何内容之前失败。
    static func run(
        _ plan: Plan, policy: FilerConflictPolicy, cancel: FilerCancelFlag, progress: @Sendable (UInt64) -> Void
    ) throws -> Int {
        var blocked: [String] = []
        for dir in plan.directories {
            if blocked.contains(where: { dir.hasPrefix($0 + "/") }) { continue }
            var st = stat()
            if lstat(dir, &st) == 0, (st.st_mode & S_IFMT) != S_IFDIR { blocked.append(dir) }
        }
        if policy == .fail {
            for file in plan.files {
                if blocked.contains(where: { file.destination.hasPrefix($0 + "/") }) { continue }
                var st = stat()
                if lstat(file.destination, &st) == 0 { throw FilerError(.alreadyExists, path: file.destination) }
            }
        }
        for dir in plan.directories {
            if blocked.contains(where: { dir == $0 || dir.hasPrefix($0 + "/") }) { continue }
            var st = stat()
            if lstat(dir, &st) == 0 {
                // 已存在的目标必须是真正的目录；是链接或文件就拒绝，不穿过链接写出去。
                guard (st.st_mode & S_IFMT) == S_IFDIR else { throw FilerError(.alreadyExists, path: dir) }
            } else if mkdir(dir, 0o755) != 0 {
                throw FilerError.fromErrno(errno, path: dir)
            }
        }
        var done: UInt64 = 0
        var skipped = blocked.count
        var buffer = [UInt8](repeating: 0, count: chunkSize)
        for file in plan.files {
            if cancel.isCancelled { throw CancellationError() }
            if blocked.contains(where: { file.destination.hasPrefix($0 + "/") }) {
                done += file.size
                progress(done)
                continue
            }
            let copied = try copyFile(file, policy: policy, cancel: cancel, buffer: &buffer) { done += $0; progress(done) }
            if !copied {
                skipped += 1
                done += file.size
                progress(done)
            }
        }
        return skipped
    }

    /// 返回 false 表示按 Skip 跳过。
    private static func copyFile(
        _ file: (source: String, destination: String, size: UInt64, mode: mode_t), policy: FilerConflictPolicy,
        cancel: FilerCancelFlag, buffer: inout [UInt8], advance: (UInt64) -> Void
    ) throws -> Bool {
        let src = open(file.source, O_RDONLY)
        guard src >= 0 else { throw FilerError.fromErrno(errno, path: file.source) }
        defer { close(src) }
        // O_NOFOLLOW：目标是符号链接时失败而不是写到链接指向的位置。
        var flags = O_WRONLY | O_CREAT | O_NOFOLLOW
        flags |= policy == .overwrite ? O_TRUNC : O_EXCL
        let dst = open(file.destination, flags, 0o600)
        if dst < 0 {
            let code = errno
            if code == EEXIST, policy == .skip { return false }
            throw FilerError.fromErrno(code, path: file.destination)
        }
        var succeeded = false
        defer {
            close(dst)
            // 失败或取消：删除残缺文件，避免被误认为完整副本。
            if !succeeded { unlink(file.destination) }
        }
        while true {
            if cancel.isCancelled { throw CancellationError() }
            let n = buffer.withUnsafeMutableBytes { read(src, $0.baseAddress, $0.count) }
            if n < 0 { throw FilerError.fromErrno(errno, path: file.source) }
            if n == 0 { break }
            var written = 0
            while written < n {
                let w = buffer.withUnsafeBytes { write(dst, $0.baseAddress! + written, n - written) }
                if w < 0 { throw FilerError.fromErrno(errno, path: file.destination) }
                written += w
            }
            advance(UInt64(n))
        }
        // 只保留权限位，不承诺属主、扩展属性与时间戳（filer.md §8）。
        fchmod(dst, file.mode)
        succeeded = true
        return true
    }
}

@MainActor
final class LocalCopyEngine: FilerTransferEngine {
    private final class Handle: FilerTransferHandle {
        let events: AsyncStream<FilerTransferEvent>
        let flag: FilerCancelFlag
        init(events: AsyncStream<FilerTransferEvent>, flag: FilerCancelFlag) {
            self.events = events
            self.flag = flag
        }
        func cancel() { flag.cancel() }
    }

    func start(_ item: FilerTransferItem, policy: FilerConflictPolicy) throws -> FilerTransferHandle {
        let (stream, continuation) = AsyncStream<FilerTransferEvent>.makeStream()
        let flag = FilerCancelFlag()
        let source = item.source, destination = item.destination, name = item.name
        Task.detached {
            do {
                let plan = try LocalCopier.plan(source: source, destinationDir: destination, name: name)
                continuation.yield(.started(total: plan.total))
                let last = LastEmit()
                let skipped = try LocalCopier.run(plan, policy: policy, cancel: flag) { done in
                    // 进度最多每 50 ms 一次（与引擎约定一致）；最终值由 finished 前的强制事件保证。
                    if last.shouldEmit() { continuation.yield(.progress(done: done, total: plan.total)) }
                }
                continuation.yield(.progress(done: plan.total, total: plan.total))
                continuation.yield(.finished(.completed(skipped: skipped)))
            } catch is CancellationError {
                continuation.yield(.finished(.cancelled))
            } catch {
                let e = FilerError.from(error, path: nil)
                continuation.yield(.finished(.failed(message: e.message(source: .local), alreadyExists: e.kind == .alreadyExists)))
            }
            continuation.finish()
        }
        return Handle(events: stream, flag: flag)
    }

    private final class LastEmit: @unchecked Sendable {
        private let lock = NSLock()
        private var last = Date.distantPast
        func shouldEmit() -> Bool {
            lock.lock(); defer { lock.unlock() }
            let now = Date()
            guard now.timeIntervalSince(last) >= 0.05 else { return false }
            last = now
            return true
        }
    }
}

// MARK: - 批次

struct FilerConflictRequest: Equatable, Sendable {
    var itemName: String
    var isDirectory: Bool
    var kind: FilerTransferKind
    /// 目标位置的说明（"本地"或会话名 + 目录）。
    var destinationDescription: String
}

enum FilerConflictChoice: Equatable, Sendable { case overwrite, skip, cancel }

struct FilerConflictDecision: Equatable, Sendable {
    var choice: FilerConflictChoice
    /// 勾选"应用于本批其余冲突"。
    var applyToRest: Bool
}

/// 底部进度区显示的状态（filer.md §6.2）。
struct FilerTransferStatus: Equatable, Sendable {
    enum Phase: Equatable, Sendable {
        case running
        case completed(skipped: Int)
        case cancelled
        case failed(String)
    }

    var kind: FilerTransferKind
    var name: String
    /// 第几项 / 共几项。
    var index: Int
    var count: Int
    var doneBytes: UInt64 = 0
    /// 未知时界面显示不确定进度；零总量不能显示成"传完了"。
    var totalBytes: UInt64?
    var bytesPerSecond: Double?
    var phase: Phase = .running
    /// 批次的来源说明（会话 / 目录），让反馈始终能归属到原会话（filer.md §6.2）。
    var origin: String = ""

    var isRunning: Bool { phase == .running }

    /// 进度 0...1；总量未知或为 0 时为 nil（不确定进度）。完成时恒为 1。
    var fraction: Double? {
        if case .completed = phase { return 1 }
        guard let total = totalBytes, total > 0 else { return nil }
        return min(1, Double(doneBytes) / Double(total))
    }

    var actionText: String {
        switch kind {
        case .upload: String(localized: "Uploading")
        case .download: String(localized: "Downloading")
        case .copy: String(localized: "Copying")
        }
    }

    /// 例：`1.5 MB / 12 MB · 3.2 MB/s`。总量未知只显示已传。
    var byteText: String {
        let style = ByteCountFormatStyle(style: .file)
        var text = Int64(clamping: doneBytes).formatted(style)
        if let total = totalBytes, total > 0 { text += " / " + Int64(clamping: total).formatted(style) }
        if isRunning, let rate = bytesPerSecond { text += " · " + Int64(rate).formatted(style) + "/s" }
        return text
    }
}

struct FilerBatchOutcome: Equatable, Sendable {
    enum Stop: Equatable, Sendable { case none, cancelled, failed(String) }
    /// 已完成的项数。
    var completedItems = 0
    /// 因冲突选择"跳过"而跳过的文件数。
    var skippedFiles = 0
    var stop: Stop = .none
}

/// 按顺序逐项执行一批传输（filer.md §6.2）：每次只处理一项；当前项失败或取消就停止余下项目，已完成项保留。
/// 冲突（D4）：每项先用 `fail` 发起，收到 `alreadyExists` 失败后询问用户，再用所选策略重试同一项；
/// 勾选"应用于本批其余冲突"后，后续项直接用该策略发起。
@MainActor
final class FilerBatchRunner {
    private var currentHandle: FilerTransferHandle?
    private(set) var isCancelRequested = false

    /// 取消当前项并阻止余下项目开始。
    func cancel() {
        isCancelRequested = true
        currentHandle?.cancel()
    }

    func run(
        items: [FilerTransferItem], engine: FilerTransferEngine, origin: String, destinationDescription: String,
        resolveConflict: (FilerConflictRequest) async -> FilerConflictDecision,
        now: () -> TimeInterval, update: (FilerTransferStatus) -> Void
    ) async -> FilerBatchOutcome {
        var outcome = FilerBatchOutcome()
        var remembered: FilerConflictPolicy?
        for (index, item) in items.enumerated() {
            if isCancelRequested { outcome.stop = .cancelled; break }
            var policy = remembered ?? item.initialPolicy
            itemLoop: while true {
                var status = FilerTransferStatus(kind: item.kind, name: item.name, index: index + 1, count: items.count, origin: origin)
                update(status)
                let handle: FilerTransferHandle
                do { handle = try engine.start(item, policy: policy) } catch {
                    let text = FilerError.fromCore(error, path: nil).message(source: nil)
                    outcome.stop = .failed(text)
                    break
                }
                currentHandle = handle
                if isCancelRequested { handle.cancel() }
                var meter = RateMeter()
                var result: FilerTransferResult?
                for await event in handle.events {
                    switch event {
                    case .started(let total):
                        status.totalBytes = total
                    case .progress(let done, let total):
                        status.doneBytes = done
                        if let total { status.totalBytes = total }
                        meter.record(bytes: done, at: now())
                        status.bytesPerSecond = meter.bytesPerSecond
                    case .finished(let r):
                        result = r
                    }
                    update(status)
                }
                currentHandle = nil
                switch result ?? .failed(message: String(localized: "The transfer ended unexpectedly"), alreadyExists: false) {
                case .completed(let skipped):
                    outcome.completedItems += 1
                    outcome.skippedFiles += skipped
                    break itemLoop
                case .cancelled:
                    outcome.stop = .cancelled
                    break itemLoop
                case .failed(let message, let alreadyExists):
                    guard alreadyExists, policy == .fail, !isCancelRequested else {
                        outcome.stop = isCancelRequested ? .cancelled : .failed(message)
                        break itemLoop
                    }
                    let decision = await resolveConflict(FilerConflictRequest(
                        itemName: item.name, isDirectory: item.isDirectory, kind: item.kind,
                        destinationDescription: destinationDescription
                    ))
                    switch decision.choice {
                    case .cancel:
                        outcome.stop = .cancelled
                        break itemLoop
                    case .overwrite:
                        policy = .overwrite
                        if decision.applyToRest { remembered = .overwrite }
                    case .skip:
                        policy = .skip
                        if decision.applyToRest { remembered = .skip }
                    }
                    // 用所选策略重试同一项。
                }
            }
            if outcome.stop != .none { break }
        }
        return outcome
    }
}
