import Foundation
import KansoBridge
import KansoUI

@MainActor
final class GitPanelModel {
    static var emptyText: String { String(localized: "Can’t detect changes in this directory; Git repositories only") }
    private let engine: any KansoGitPanelProtocol
    private var context: Context?
    private var pendingTab: TabItem?
    private var hasPendingTab = false
    private var request: UInt64 = 0
    private var readTask: Task<Void, Never>?
    private var diffTasks: [String: Task<Void, Never>] = [:]
    private(set) var summary: FfiGitSummary?
    private(set) var expanded: Set<String> = []
    private(set) var diffs: [String: FfiGitDiff] = [:]
    private(set) var diffErrors: [String: String] = [:]
    private(set) var error: String?
    private(set) var isLoading = false
    private(set) var isWriting = false
    var message = "" { didSet { onChange?() } }
    var onChange: (() -> Void)?

    private struct Context: Equatable {
        let session: SessionID
        let cwd: String
    }

    init(engine: any KansoGitPanelProtocol = KansoGitPanel()) {
        self.engine = engine
    }

    var canWrite: Bool { !isLoading && !isWriting && summary?.files.isEmpty == false }
    var revision: UInt64 { request }
    var canCommit: Bool {
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        return canWrite && !trimmed.isEmpty && trimmed.utf8.count <= 16 * 1024
    }

    func follow(_ tab: TabItem?) {
        if isWriting {
            pendingTab = tab
            hasPendingTab = true
            return
        }
        let next: Context?
        if let tab, tab.kind == .local, tab.connection == .connected, let cwd = tab.cwd, !cwd.isEmpty {
            next = Context(session: tab.id, cwd: cwd)
        } else {
            next = nil
        }
        guard next != context else { return }
        context = next
        invalidate()
        summary = nil
        expanded = []
        diffs = [:]
        diffErrors = [:]
        error = nil
        message = ""
        if next != nil { refresh() } else { onChange?() }
    }

    func refresh(keepingError: Bool = false) {
        guard !isWriting, let context else { return }
        if !keepingError { error = nil }
        invalidate()
        let request = self.request
        let engine = self.engine
        isLoading = true
        onChange?()
        readTask = Task { [weak self] in
            let result = await Task.detached { Result { try engine.read(request: request, cwd: context.cwd) } }.value
            guard let self, engine.isCurrent(request: request), !Task.isCancelled else { return }
            self.isLoading = false
            switch result {
            case .success(let summary):
                self.summary = summary
                self.expanded.formIntersection(Set(summary?.files.map(\.path) ?? []))
                self.diffs = [:]
                self.diffErrors = [:]
                for path in self.expanded { self.loadDiff(path) }
            case .failure(let error):
                self.summary = nil
                self.expanded = []
                self.diffs = [:]
                self.diffErrors = [:]
                self.error = CoreMessageText.text(for: error)
            }
            self.onChange?()
        }
    }

    private func invalidate() {
        readTask?.cancel()
        for task in diffTasks.values { task.cancel() }
        diffTasks = [:]
        request = engine.begin()
        isLoading = false
    }

    func toggle(_ path: String) {
        guard !isWriting, !isLoading else { return }
        if expanded.remove(path) != nil {
            diffTasks.removeValue(forKey: path)?.cancel()
            diffs[path] = nil
            diffErrors[path] = nil
        } else {
            expanded.insert(path)
            loadDiff(path)
        }
        onChange?()
    }

    private func loadDiff(_ path: String) {
        let request = self.request
        let engine = self.engine
        diffTasks[path] = Task { [weak self] in
            let result = await Task.detached { Result { try engine.diff(request: request, path: path) } }.value
            guard let self, engine.isCurrent(request: request), self.expanded.contains(path), !Task.isCancelled else { return }
            switch result {
            case .success(let diff): self.diffs[path] = diff
            case .failure(let error): self.diffErrors[path] = CoreMessageText.text(for: error)
            }
            self.onChange?()
        }
    }

    func dismissError() { error = nil; onChange?() }

    func copyPath(_ path: String, using writer: (String) -> Bool) {
        guard let root = summary?.root else { return }
        if !writer((root as NSString).appendingPathComponent(path)) {
            error = String(localized: "Couldn’t copy to the clipboard.")
            onChange?()
        }
    }

    func commit() async {
        guard canCommit else { return }
        let message = self.message
        await write(isCommit: true) { engine, request in try engine.commit(request: request, message: message) }
    }

    func discard(_ path: String?) async {
        guard canWrite else { return }
        await write(isCommit: false) { engine, request in try engine.discard(request: request, path: path) }
    }

    private func write(isCommit: Bool, operation: @escaping @Sendable (any KansoGitPanelProtocol, UInt64) throws -> Void) async {
        isWriting = true
        error = nil
        onChange?()
        let engine = self.engine
        let request = self.request
        let result = await Task.detached { Result { try operation(engine, request) } }.value
        if case .failure(let error) = result {
            self.error = CoreMessageText.text(for: error) + (isCommit ? "\n" + String(localized: "The commit may have left changes staged.") : "")
        } else if isCommit {
            message = ""
        }
        isWriting = false
        if hasPendingTab {
            let tab = pendingTab
            pendingTab = nil
            hasPendingTab = false
            follow(tab)
        }
        refresh(keepingError: true)
        onChange?()
    }

    func waitUntilIdle() async {
        await readTask?.value
        for task in diffTasks.values { await task.value }
    }

    func shutdown() { invalidate() }
}
