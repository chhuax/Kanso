import Foundation
import KansoUI

enum RemoteEditConflictChoice: Equatable {
    case overwrite, redownload, saveCopy(String), cancel
}

@MainActor
final class RemoteEditManager {
    struct Key: Hashable {
        var session: SessionID
        var path: String
    }

    enum Phase: Equatable {
        case downloading, synced, pending, syncing, conflict, failed(String)
    }

    final class Edit {
        let key: Key
        let tab: FilerTabSnapshot
        let entry: FilerEntry
        let file: URL
        var remoteVersion: FilerFileVersion
        var syncedVersion: FilerFileVersion?
        var observedVersion: FilerFileVersion?
        var phase: Phase = .downloading
        var download: FilerTransferStatus?
        var watcher: RemoteEditWatcher?
        var transfer: FilerTransferHandle?
        var job: Task<Void, Never>?
        var debounce: Task<Void, Never>?
        var retry: Task<Void, Never>?
        var retryAttempt = 0
        var active = true

        init(key: Key, tab: FilerTabSnapshot, entry: FilerEntry, file: URL, version: FilerFileVersion) {
            self.key = key
            self.tab = tab
            self.entry = entry
            self.file = file
            remoteVersion = version
        }

        var hasChanges: Bool {
            syncedVersion != nil && (try? FilerFileVersion.local(file)) != syncedVersion
        }

        var notice: FilerNotice {
            let text: String
            let isError: Bool
            switch phase {
            case .downloading: text = String(localized: "Preparing remote edit"); isError = false
            case .synced: text = String(localized: "Synced"); isError = false
            case .pending: text = String(localized: "Waiting to sync"); isError = false
            case .syncing: text = String(localized: "Syncing…"); isError = false
            case .conflict: text = String(localized: "Remote file changed — sync paused"); isError = true
            case .failed(let reason): text = String(localized: "Sync failed") + ": " + reason; isError = true
            }
            return FilerNotice(text: "\(tab.source.title) · \(entry.path) · \(text)", isError: isError)
        }
    }

    private let root: URL
    private let run: URL
    private let environment: FilerEnvironment
    private let backend: FilerRemoteBackend
    private let prompter: FilerPrompter
    private let debounceDelay: Duration
    private let sleep: (Duration) async throws -> Void
    private var sessions: [SessionID: FilerTabSnapshot] = [:]
    private var opening: Set<Key> = []
    private var notices: [SessionID: FilerNotice] = [:]
    private(set) var edits: [Key: Edit] = [:]
    private(set) var recentApplications: [URL] = []
    var onChange: (() -> Void)?
    var onSynced: ((SessionID, String) -> Void)?
    var onNotice: ((FilerNotice) -> Void)?

    init(
        root: URL, environment: FilerEnvironment, backend: FilerRemoteBackend, prompter: FilerPrompter,
        debounceDelay: Duration = .milliseconds(300),
        sleep: @escaping (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) throws {
        self.root = root
        run = root.appendingPathComponent(UUID().uuidString)
        self.environment = environment
        self.backend = backend
        self.prompter = prompter
        self.debounceDelay = debounceDelay
        self.sleep = sleep
        try FileManager.default.createDirectory(at: run, withIntermediateDirectories: true)
        if let data = try? Data(contentsOf: root.appendingPathComponent("applications.json")) {
            recentApplications = Array((try JSONDecoder().decode([URL].self, from: data)).prefix(6))
        }
    }

    func cleanPreviousRuns() async throws {
        let root = root
        let run = run
        try await Task.detached(priority: .utility) {
            try RemoteEditFiles.cleanPreviousRuns(in: root, excluding: run)
        }.value
    }

    func updateSessions(_ tabs: [FilerTabSnapshot]) {
        let previous = sessions
        sessions = Dictionary(uniqueKeysWithValues: tabs.map { ($0.session, $0) })
        for edit in Array(edits.values) {
            guard let tab = sessions[edit.key.session], tab.userHost == edit.tab.userHost else { continue }
            if tab.isConnected, previous[tab.session]?.isConnected != true {
                edit.retry?.cancel()
                edit.retry = nil
                if edit.debounce == nil { attempt(edit) }
            }
        }
    }

    func status(for session: SessionID) -> (FilerNotice?, FilerTransferStatus?) {
        let matching = edits.values.filter { $0.key.session == session }.sorted { $0.key.path < $1.key.path }
        let selected = matching.first { $0.phase == .downloading } ?? matching.first { $0.phase != .synced } ?? matching.first
        return (notices[session] ?? selected?.notice, selected?.download)
    }

    func open(_ entry: FilerEntry, tab: FilerTabSnapshot, application: URL? = nil, chooseApplication: Bool = false) async {
        guard tab.kind == .ssh, tab.isConnected, !entry.isDirectory,
              case .success = FilerNameValidator.validate(entry.name, existing: []) else { return }
        let key = Key(session: tab.session, path: entry.path)
        guard opening.insert(key).inserted else { return }
        defer { opening.remove(key) }
        notices[tab.session] = nil
        var openedEdit: Edit?
        var application = application
        if chooseApplication {
            guard let selected = await prompter.chooseEditorApplication() else { return }
            application = selected
        }
        guard isConnected(tab) else { return }
        do {
            if let edit = edits[key] {
                openedEdit = edit
                guard edit.phase != .downloading else { return }
                if edit.hasChanges || edit.job != nil {
                    emitNotice(edit.notice, session: tab.session)
                    try await launch(edit.file, application: application)
                    if edit.phase == .conflict { edit.phase = .pending; attempt(edit) }
                    return
                }
                try await download(edit)
                if edit.active { try await launch(edit.file, application: application) }
                return
            }
            let version = try await backend.stat(session: key.session, path: key.path)
            guard isConnected(tab) else { return }
            let directory = run.appendingPathComponent(String(tab.session)).appendingPathComponent(UUID().uuidString)
            let file = directory.appendingPathComponent("content").appendingPathComponent(entry.name)
            let edit = Edit(key: key, tab: tab, entry: entry, file: file, version: version)
            openedEdit = edit
            edits[key] = edit
            try await download(edit)
            if edit.active { try await launch(file, application: application) }
        } catch is CancellationError {
            if let openedEdit { removeUnopenedCopy(openedEdit) }
        } catch {
            if let openedEdit { removeUnopenedCopy(openedEdit) }
            let reason = FilerError.fromCore(error, path: entry.path).reason
            let notice = FilerNotice(text: "\(tab.source.title) · \(entry.path) · \(reason)", isError: true)
            emitNotice(notice, session: tab.session)
        }
    }

    private func removeUnopenedCopy(_ edit: Edit) {
        guard edits[edit.key] === edit, edit.syncedVersion == nil else { return }
        edits[edit.key] = nil
        try? FileManager.default.removeItem(at: edit.file.deletingLastPathComponent().deletingLastPathComponent())
    }

    private func emitNotice(_ notice: FilerNotice, session: SessionID) {
        notices[session] = notice
        onNotice?(notice)
        onChange?()
    }

    private func launch(_ file: URL, application: URL?) async throws {
        try await prompter.openRemoteCopy(file, application: application)
        if let application {
            recentApplications.removeAll { $0 == application }
            recentApplications.insert(application, at: 0)
            recentApplications = Array(recentApplications.prefix(6))
            try JSONEncoder().encode(recentApplications).write(to: root.appendingPathComponent("applications.json"), options: .atomic)
            onChange?()
        }
    }

    private func isConnected(_ tab: FilerTabSnapshot) -> Bool {
        sessions[tab.session]?.isConnected == true && sessions[tab.session]?.userHost == tab.userHost
    }

    private func download(_ edit: Edit, discardChanges: Bool = false) async throws {
        let version = try await backend.stat(session: edit.key.session, path: edit.key.path)
        if let size = version.size, size > 50_000_000 {
            guard await prompter.confirmLargeRemoteEdit(name: edit.entry.name, source: edit.tab.source.title) else {
                if edit.syncedVersion == nil { edits[edit.key] = nil }
                throw CancellationError()
            }
        }
        guard edit.active else { throw CancellationError() }
        if !discardChanges, edit.hasChanges { return }
        edit.debounce?.cancel()
        edit.debounce = nil
        edit.retry?.cancel()
        edit.retry = nil
        edit.watcher?.stop()
        edit.watcher = nil
        edit.phase = .downloading
        edit.download = FilerTransferStatus(kind: .download, name: edit.entry.name, index: 1, count: 1, origin: edit.tab.source.title)
        onChange?()
        let staging = edit.file.deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("download")
        defer {
            try? FileManager.default.removeItem(at: staging)
            edit.download = nil
            onChange?()
        }
        do {
            try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
            try await transfer(edit, item: FilerTransferItem(
                kind: .download, name: edit.entry.name, source: edit.key.path, destination: staging.path, isDirectory: false
            ), policy: .overwrite)
            guard edit.active else { throw CancellationError() }
            if !discardChanges, edit.hasChanges {
                try watch(edit)
                edit.observedVersion = try FilerFileVersion.local(edit.file)
                edit.phase = .pending
                scheduleSave(edit)
                return
            }
            let content = edit.file.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: content, withIntermediateDirectories: true)
            let downloaded = staging.appendingPathComponent(edit.entry.name)
            if FileManager.default.fileExists(atPath: edit.file.path) {
                _ = try FileManager.default.replaceItemAt(edit.file, withItemAt: downloaded)
            } else {
                try FileManager.default.moveItem(at: downloaded, to: edit.file)
            }
            edit.remoteVersion = version
            let local = try FilerFileVersion.local(edit.file)
            try RemoteEditFiles.markSynced(edit.file, version: local)
            edit.syncedVersion = local
            edit.observedVersion = local
            edit.phase = .synced
            try watch(edit)
        } catch {
            if edit.syncedVersion == nil {
                if edits[edit.key] === edit { edits[edit.key] = nil }
                try? FileManager.default.removeItem(at: edit.file.deletingLastPathComponent().deletingLastPathComponent())
            } else {
                edit.phase = .failed(FilerError.fromCore(error, path: edit.key.path).reason)
                if edit.active { try? watch(edit) }
            }
            throw error
        }
    }

    private func watch(_ edit: Edit) throws {
        edit.watcher = try RemoteEditWatcher(url: edit.file) { [weak self, weak edit] in
            if let edit { self?.fileChanged(edit) }
        }
    }

    func fileChanged(_ edit: Edit) {
        guard edit.active, let version = try? FilerFileVersion.local(edit.file), version != edit.observedVersion else { return }
        edit.observedVersion = version
        if edit.phase != .conflict && edit.phase != .syncing { edit.phase = .pending }
        edit.retry?.cancel()
        edit.retry = nil
        scheduleSave(edit)
        onChange?()
    }

    private func scheduleSave(_ edit: Edit) {
        edit.debounce?.cancel()
        edit.debounce = Task { [weak self, weak edit, sleep, debounceDelay] in
            do { try await sleep(debounceDelay) } catch { return }
            guard let self, let edit, !Task.isCancelled else { return }
            edit.debounce = nil
            self.attempt(edit)
        }
    }

    private func attempt(_ edit: Edit) {
        guard edit.active, edit.syncedVersion != nil, edit.hasChanges, edit.phase != .conflict, edit.job == nil else { return }
        guard isConnected(edit.tab) else {
            edit.phase = .pending
            onChange?()
            return
        }
        edit.job = Task { [weak self, weak edit] in
            guard let self, let edit else { return }
            let succeeded = await self.sync(edit)
            edit.job = nil
            if succeeded, edit.active, edit.hasChanges { self.scheduleSave(edit) }
        }
    }

    private func sync(_ edit: Edit) async -> Bool {
        var conflictChoiceMade = false
        do {
            let current: FilerFileVersion?
            do { current = try await backend.stat(session: edit.key.session, path: edit.key.path) } catch {
                if FilerError.fromCore(error, path: edit.key.path).kind == .notFound { current = nil } else { throw error }
            }
            guard edit.active else { return false }
            var name = edit.entry.name
            var policy = FilerConflictPolicy.overwrite
            if current != edit.remoteVersion {
                edit.phase = .conflict
                onChange?()
                let choice = await prompter.resolveRemoteEditConflict(name: edit.entry.name, source: edit.tab.source.title)
                guard edit.active else { return false }
                conflictChoiceMade = true
                switch choice {
                case .cancel: return false
                case .redownload: try await download(edit, discardChanges: true); return true
                case .overwrite: break
                case .saveCopy(let input):
                    guard case .success(let valid) = FilerNameValidator.validate(input, existing: [edit.entry.name]) else {
                        throw FilerError(.other(String(localized: "Enter a different file name")))
                    }
                    name = valid
                    policy = .fail
                }
            }
            guard isConnected(edit.tab) else { throw FilerError(.disconnected) }
            edit.phase = .syncing
            onChange?()
            let version = try FilerFileVersion.local(edit.file)
            let staging = edit.file.deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("upload")
            let snapshot = staging.appendingPathComponent(name)
            defer { try? FileManager.default.removeItem(at: staging) }
            let file = edit.file
            try await Task.detached {
                try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
                try FileManager.default.copyItem(at: file, to: snapshot)
            }.value
            guard edit.active else { return false }
            guard (try? FilerFileVersion.local(edit.file)) == version else { edit.phase = .pending; return true }
            let directory = FilerPath.parent(of: edit.key.path)
            try await transfer(edit, item: FilerTransferItem(
                kind: .upload, name: name, source: snapshot.path, destination: directory, isDirectory: false
            ), policy: policy)
            guard edit.active else { return false }
            if name == edit.entry.name {
                edit.remoteVersion = try await backend.stat(session: edit.key.session, path: edit.key.path)
            } else {
                edit.phase = .conflict
            }
            guard edit.active else { return false }
            try RemoteEditFiles.markSynced(edit.file, version: version)
            edit.syncedVersion = version
            edit.retryAttempt = 0
            notices[edit.key.session] = nil
            edit.phase = edit.hasChanges ? (name == edit.entry.name ? .pending : .conflict) : .synced
            onSynced?(edit.key.session, directory)
            onChange?()
            return true
        } catch {
            guard edit.active else { return false }
            let failure = FilerError.fromCore(error, path: edit.key.path)
            if conflictChoiceMade, failure.kind != .disconnected {
                edit.phase = .conflict
                emitNotice(FilerNotice(text: "\(edit.tab.source.title) · \(edit.key.path) · \(failure.reason)", isError: true), session: edit.key.session)
                return false
            }
            edit.phase = .failed(failure.reason)
            onChange?()
            scheduleRetry(edit)
            return false
        }
    }

    private func scheduleRetry(_ edit: Edit) {
        let delay = Self.retryDelay(attempt: edit.retryAttempt)
        edit.retryAttempt += 1
        edit.retry?.cancel()
        edit.retry = Task { [weak self, weak edit, sleep] in
            do { try await sleep(delay) } catch { return }
            guard let self, let edit, !Task.isCancelled else { return }
            edit.retry = nil
            self.attempt(edit)
        }
    }

    static func retryDelay(attempt: Int) -> Duration { .seconds(min(60, 6 << min(attempt, 4))) }

    private func transfer(_ edit: Edit, item: FilerTransferItem, policy: FilerConflictPolicy) async throws {
        let handle = try environment.transferEngine(for: edit.tab).start(item, policy: policy)
        edit.transfer = handle
        defer { edit.transfer = nil }
        for await event in handle.events {
            switch event {
            case .started(let total): edit.download?.totalBytes = total
            case .progress(let done, let total): edit.download?.doneBytes = done; edit.download?.totalBytes = total
            case .finished(.completed(let skipped)):
                if skipped > 0 { throw FilerError(.alreadyExists) }
                return
            case .finished(.cancelled): throw CancellationError()
            case .finished(.failed(let message, let exists)): throw FilerError(exists ? .alreadyExists : .other(message))
            }
            onChange?()
        }
        throw CancellationError()
    }

    func cancelDownload(session: SessionID) {
        edits.values.first { $0.key.session == session && $0.phase == .downloading }?.transfer?.cancel()
    }

    func close(sessions closing: [SessionID]) -> [URL] {
        var retained: [URL] = []
        for edit in Array(edits.values) where closing.contains(edit.key.session) {
            edit.active = false
            edit.watcher?.stop()
            edit.watcher = nil
            edit.debounce?.cancel()
            edit.retry?.cancel()
            edit.job?.cancel()
            edit.transfer?.cancel()
            if edit.hasChanges { retained.append(edit.file) } else if edit.phase != .downloading {
                try? FileManager.default.removeItem(at: edit.file.deletingLastPathComponent().deletingLastPathComponent())
            }
            edits[edit.key] = nil
        }
        for session in closing { sessions[session] = nil; notices[session] = nil }
        onChange?()
        return retained
    }
}
