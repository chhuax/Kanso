import AppKit
import KansoBridge
import TerminalKit

@MainActor
final class CompletionSourceReader {
    let remote: Bool
    private let resolveLocalHosts: () -> Set<String>
    private lazy var localHosts = resolveLocalHosts()
    let localHome: String?
    let workingDirectory: () async throws -> String
    let homeDirectory: () async throws -> String
    let listDirectory: (String) async throws -> [FfiCompletionEntry]
    let readConfigs: () async -> [String]
    var now: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    private var cwdCache: (String, TimeInterval)?
    private var directoryCache: (String, [FfiCompletionEntry], TimeInterval)?
    private var configCache: ([String], TimeInterval)?

    init(remote: Bool, localHosts: @autoclosure @escaping () -> Set<String>, localHome: String?,
         workingDirectory: @escaping () async throws -> String,
         homeDirectory: @escaping () async throws -> String,
         listDirectory: @escaping (String) async throws -> [FfiCompletionEntry],
         readConfigs: @escaping () async -> [String]) {
        self.remote = remote
        resolveLocalHosts = localHosts
        self.localHome = localHome
        self.workingDirectory = workingDirectory
        self.homeDirectory = homeDirectory
        self.listDirectory = listDirectory
        self.readConfigs = readConfigs
    }

    func invalidate() {
        cwdCache = nil
        directoryCache = nil
        configCache = nil
    }

    func initial(_ state: FfiCompletionState) -> FfiCompletionSources {
        let local = state.directoryHost.map { $0.isEmpty || FilerModel.isLocalHost($0, names: localHosts) } ?? true
        return FfiCompletionSources(cwd: state.directory, home: remote ? nil : localHome,
                                    allowPaths: remote || local, entries: [], configs: [])
    }

    func enrich(_ state: FfiCompletionState, request: FfiCompletionSuggestions,
                plan: (FfiCompletionSources) -> FfiCompletionSuggestions) async -> FfiCompletionSources {
        var sources = initial(state)
        if request.needsCwd {
            if let cache = cwdCache, now() - cache.1 < 2 {
                sources.cwd = cache.0
            } else if let cwd = try? await workingDirectory(), !Task.isCancelled {
                sources.cwd = cwd
                cwdCache = (cwd, now())
            }
        }
        if request.needsHome { sources.home = try? await homeDirectory() }
        if Task.isCancelled { return sources }
        let directory = request.needsCwd || request.needsHome ? plan(sources).directory : request.directory
        if let directory {
            if let cache = directoryCache, cache.0 == directory, now() - cache.2 < 1.5 {
                sources.entries = cache.1
            } else if let entries = try? await listDirectory(directory), !Task.isCancelled {
                sources.entries = Array(entries.prefix(2000))
                directoryCache = (directory, sources.entries, now())
            }
        }
        if request.needsKubeNames, !remote, !Task.isCancelled {
            if let cache = configCache, now() - cache.1 < 30 {
                sources.configs = cache.0
            } else {
                sources.configs = await readConfigs()
                if !Task.isCancelled { configCache = (sources.configs, now()) }
            }
        }
        return sources
    }

    static func live(core: KansoCore, session: UInt64, remote: Bool,
                     environment: [String: String], localHosts: @autoclosure @escaping () -> Set<String>) -> CompletionSourceReader {
        let home = environment["HOME"]
        let files: [String]
        if let paths = environment["KUBECONFIG"], !paths.isEmpty {
            files = paths.split(separator: ":").map(String.init)
        } else {
            files = home.map { [$0 + "/.kube/config"] } ?? []
        }
        return CompletionSourceReader(remote: remote, localHosts: localHosts(), localHome: home,
            workingDirectory: { try await core.workingDirectory(sessionId: session) },
            homeDirectory: { try await core.sftpHome(sessionId: session) },
            listDirectory: { path in
                var entries: [FfiCompletionEntry] = []
                if remote {
                    let stream = try core.sftpList(sessionId: session, path: path)
                    while entries.count < 2000 {
                        try Task.checkCancellation()
                        guard let batch = try await stream.nextBatchLimited(limit: UInt32(2000 - entries.count)) else { break }
                        entries += batch.map { FfiCompletionEntry(name: $0.name, directory: $0.isDir) }
                    }
                } else {
                    let source = LocalFilerDataSource()
                    for try await batch in source.list(path: path, maxEntries: 2000) {
                        try Task.checkCancellation()
                        for entry in batch {
                            var directory = entry.isDirectory
                            if entry.isSymlink { directory = (try? await source.resolveLink(entry)) == .directory(path: entry.path) }
                            entries.append(FfiCompletionEntry(name: entry.name, directory: directory))
                        }
                    }
                }
                return entries
            },
            readConfigs: {
                await Task.detached { files.compactMap { try? String(contentsOfFile: $0, encoding: .utf8) } }.value
            })
    }
}

@MainActor
final class CompletionController {
    private let state: () -> FfiCompletionState?
    private let compute: (String, FfiCompletionSources) -> FfiCompletionSuggestions
    private let plan: (String, FfiCompletionSources) -> FfiCompletionSuggestions
    private weak var view: TerminalView?
    private let sink: TerminalInputSink
    private let reader: CompletionSourceReader
    private(set) var model = CompletionModel()
    private var task: Task<Void, Never>?
    private var deadline: Task<Void, Never>?
    private var panel: FloatingPanel?
    private var popup: CompletionPopupView?
    private var lastState: FfiCompletionState?
    private var awaitingEcho = false
    var enabled = false { didSet { invalidate(); if enabled { refresh() } } }
    var visible = false { didSet { if !visible { invalidate() } else { refresh() } } }

    init(view: TerminalView, sink: TerminalInputSink, reader: CompletionSourceReader,
         state: @escaping () -> FfiCompletionState?,
         compute: @escaping (String, FfiCompletionSources) -> FfiCompletionSuggestions,
         plan: @escaping (String, FfiCompletionSources) -> FfiCompletionSuggestions) {
        self.state = state
        self.compute = compute
        self.plan = plan
        self.view = view
        self.sink = sink
        self.reader = reader
    }

    func invalidate(clearCache: Bool = false) {
        task?.cancel()
        deadline?.cancel()
        task = nil
        deadline = nil
        model.invalidate()
        hide()
        lastState = nil
        awaitingEcho = false
        if clearCache { reader.invalidate() }
    }

    func inputChanged() {
        task?.cancel()
        deadline?.cancel()
        model.invalidate()
        hide()
        awaitingEcho = true
    }

    func intercept(_ event: NSEvent) -> Bool {
        guard enabled, visible, event.modifierFlags.intersection([.command, .control, .option, .shift]).isEmpty else { return false }
        let key: CompletionModel.Key = switch event.keyCode {
        case 125: .down
        case 126: .up
        case 53: .escape
        case 48: .tab
        default: .other
        }
        switch model.key(key) {
        case .forward: return false
        case .consume:
            if key == .escape { task?.cancel(); deadline?.cancel() }
            render()
            return true
        case .accept(let candidate):
            guard state() == lastState else { invalidate(); return true }
            inputChanged()
            CompletionModel.replace(candidate, through: sink)
            return true
        }
    }

    func refresh() {
        guard enabled, visible, let view, view.window?.firstResponder === view, !view.hasMarkedText(),
              let state = state() else { invalidate(); return }
        if state == lastState { render(); return }
        if awaitingEcho, state.input == lastState?.input { return }
        awaitingEcho = false
        task?.cancel()
        deadline?.cancel()
        let request = model.begin()
        hide()
        lastState = state
        let initial = reader.initial(state)
        let fast = compute(state.input, initial)
        let external = fast.directory != nil || fast.needsKubeNames || fast.needsCwd || fast.needsHome
        model.receive(fast.candidates, generation: request, finished: !external)
        render()
        guard external else { return }
        task = Task { [weak self] in
            guard let self else { return }
            let sources = await reader.enrich(state, request: fast) { plan(state.input, $0) }
            guard !Task.isCancelled, model.generation == request,
                  self.state() == state, view.window?.firstResponder === view, !view.hasMarkedText() else { return }
            deadline?.cancel()
            model.receive(compute(state.input, sources).candidates, generation: request, finished: true)
            render()
        }
        let timeout: Duration = reader.remote ? .milliseconds(1500) : .milliseconds(300)
        deadline = Task { [weak self] in
            do { try await Task.sleep(for: timeout) } catch { return }
            guard let self, model.generation == request else { return }
            task?.cancel()
            model.receive(fast.candidates, generation: request, finished: true)
            render()
        }
    }

    private func hide() {
        panel?.onDismiss = nil
        panel?.dismiss()
        panel = nil
        popup = nil
    }

    private func render() {
        guard !model.candidates.isEmpty, let view, let window = view.window,
              let state = lastState, let anchor = view.completionAnchor(startColumn: state.startColumn) else { hide(); return }
        if let popup, popup.candidates.count == model.candidates.count {
            popup.candidates = model.candidates
            popup.selected = model.selected
            panel?.position(below: window.convertToScreen(view.convert(anchor, to: nil)),
                            within: window.convertToScreen(view.convert(view.bounds, to: nil)))
            return
        }
        hide()
        let size = CompletionPopupView.size(count: model.candidates.count, width: min(460, view.bounds.width - 16))
        let popup = CompletionPopupView(frame: NSRect(origin: .zero, size: size))
        popup.candidates = model.candidates
        popup.selected = model.selected
        let panel = FloatingPanel(content: popup, size: size, passive: true)
        panel.appearance = view.effectiveAppearance
        panel.onDismiss = { [weak self] in self?.inputChanged() }
        panel.present(below: window.convertToScreen(view.convert(anchor, to: nil)), parent: window,
                      within: window.convertToScreen(view.convert(view.bounds, to: nil)))
        self.panel = panel
        self.popup = popup
    }
}
