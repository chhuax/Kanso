import Foundation
import KansoBridge
import KansoUI

struct SenderDraft: Equatable {
    var text = ""
    var name = ""
    var ending = FfiLineEnding.lf
}

enum SenderEnterAction: Equatable { case send, newline, inputMethod }

@MainActor
final class SenderModel {
    let core: KansoCore
    var draft = SenderDraft()
    var allTargets = false
    var isVisible = false {
        didSet { if isVisible && !oldValue { refresh(context: context, tabs: tabs) } }
    }
    var interval = 60
    var count = 0
    var page = 0
    private(set) var editing: String?
    private var originalDraft: SenderDraft?
    private(set) var library: [FfiSnippet] = []
    private(set) var visible: [FfiSnippet] = []
    private(set) var loadError: String?
    private(set) var isLoading = true
    private(set) var context: TabItem?
    private(set) var tabs: [TabItem] = []
    private(set) var profiles: [FfiProfile] = []
    private(set) var groups: [FfiGroup] = []
    private(set) var lastScopeLevel = 0
    private(set) var manualRunning = false
    private(set) var repeatRunning = false
    private(set) var runningTitle = ""
    private(set) var runningTarget = ""
    private(set) var runningInterval = 60
    private(set) var progress = ""
    private(set) var isSaving = false
    var onChange: (() -> Void)?
    var onNotice: ((String, Bool) -> Void)?
    private var timer: Timer?
    private var libraryObserver: NSObjectProtocol?
    private var libraryDirty = true
    private var isReloadingLibrary = false
    private(set) var libraryLoadCount = 0

    init(core: KansoCore) {
        self.core = core
        interval = Int(core.setting(key: "sender.interval") ?? "") ?? 60
        count = Int(core.setting(key: "sender.count") ?? "") ?? 0
        lastScopeLevel = Int(core.setting(key: "sender.scopeLevel") ?? "") ?? 0
        clampRepeatValues()
        libraryObserver = NotificationCenter.default.addObserver(forName: .sessionLibraryDidChange, object: core, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { if self?.isReloadingLibrary == false { self?.libraryChanged() } }
        }
    }

    static func enterAction(shift: Bool, marked: Bool) -> SenderEnterAction {
        marked ? .inputMethod : shift ? .newline : .send
    }

    var hasContent: Bool { !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var canSave: Bool { hasContent && !isSaving && (editing != nil || library.count < 1000) }
    var connectedCount: Int { tabs.filter { $0.kind != .sftp && $0.connection == .connected }.count }
    var currentCount: Int { context.map { $0.kind != .sftp && $0.connection == .connected ? 1 : 0 } ?? 0 }
    var pageCount: Int { max(1, (visible.count + 23) / 24) }
    var pageItems: [FfiSnippet] { Array(visible.dropFirst(page * 24).prefix(24)) }
    var scopes: [(title: String, scope: FfiSnippetScope, level: Int)] {
        var result: [(String, FfiSnippetScope, Int)] = [(String(localized: "Global"), .global, 0)]
        guard let context else { return result }
        if context.kind != .sftp {
            result.append((context.kind == .ssh ? String(localized: "SSH sessions") : String(localized: "Local Shell sessions"), .kind(kind: context.kind == .ssh ? .ssh : .local), 1))
        }
        if let saved = profiles.first(where: { $0.id == context.profileID }) {
            if let group = groups.first(where: { $0.id == saved.group }) {
                result.append((String(localized: "Group · \(group.name)"), .group(id: group.id), 2))
            }
            result.append((String(localized: "Session · \(saved.name)"), .profile(id: saved.id), 3))
        }
        return result
    }
    var defaultScopeIndex: Int { scopes.lastIndex(where: { $0.level <= lastScopeLevel }) ?? 0 }

    func refresh(context: TabItem?, tabs: [TabItem], external: Bool = true) {
        self.context = context
        self.tabs = tabs
        guard isVisible || !isLoading else { return }
        if libraryDirty {
            let all = core.snippets().sorted { $0.name.lowercased() < $1.name.lowercased() }
            let savedProfiles = core.profiles()
            let savedGroups = core.groups()
            if external && (all != library || savedProfiles != profiles || savedGroups != groups) { cancelEditing() }
            library = all
            profiles = savedProfiles
            groups = savedGroups
            loadError = nil
            libraryDirty = false
            libraryLoadCount += 1
        }
        let saved = profiles.first { $0.id == context?.profileID }
        visible = library.filter { snippet in
            switch snippet.scope {
            case .global: true
            case .profile(let id): saved?.id == id
            case .group(let id): saved?.group == id
            case .kind(let kind): (kind == .local && context?.kind == .local) || (kind == .ssh && context?.kind == .ssh)
            }
        }
        page = min(page, pageCount - 1)
        isLoading = false
        onChange?()
    }

    private func libraryChanged() {
        libraryDirty = true
        if isVisible { refresh(context: context, tabs: tabs) }
    }

    func edit(_ snippet: FfiSnippet) {
        if originalDraft == nil { originalDraft = draft }
        editing = snippet.id
        draft = SenderDraft(text: snippet.text, name: snippet.name, ending: snippet.lineEnding)
        onChange?()
    }

    func cancelEditing() {
        if let originalDraft { draft = originalDraft }
        originalDraft = nil
        editing = nil
        onChange?()
    }

    func clampRepeatValues() {
        interval = min(86400, max(1, interval))
        count = min(100000, max(0, count))
    }

    func save(scopeIndex: Int) {
        guard canSave, scopes.indices.contains(scopeIndex) else { return }
        let choice = scopes[scopeIndex]
        let name = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
        let fallback = draft.text.components(separatedBy: .newlines).first { !$0.trimmingCharacters(in: .whitespaces).isEmpty } ?? ""
        let snippet = FfiSnippet(id: editing ?? UUID().uuidString, name: String((name.isEmpty ? fallback : name).prefix(80)), text: draft.text, lineEnding: draft.ending, scope: choice.scope)
        isSaving = true
        defer { isSaving = false; onChange?() }
        do {
            try core.saveSnippet(snippet: snippet)
            lastScopeLevel = choice.level
            editing = nil
            originalDraft = nil
            reload()
            do { try core.setSetting(key: "sender.scopeLevel", json: String(lastScopeLevel)) }
            catch { onNotice?(String(localized: "The command was saved, but its save location preference could not be stored: \(CoreMessageText.text(for: error))"), true) }
        } catch { onNotice?(String(localized: "Failed to save command: \(CoreMessageText.text(for: error))"), true) }
    }

    func update(_ snippet: FfiSnippet) {
        do { try core.saveSnippet(snippet: snippet); reload() }
        catch { onNotice?(String(localized: "Failed to update command: \(CoreMessageText.text(for: error))"), true) }
    }

    func delete(_ snippet: FfiSnippet) {
        do {
            try core.deleteSnippet(id: snippet.id)
            if editing == snippet.id { cancelEditing() }
            reload()
        } catch { onNotice?(String(localized: "Failed to delete command: \(CoreMessageText.text(for: error))"), true) }
    }

    func reload() {
        libraryDirty = true
        refresh(context: context, tabs: tabs, external: false)
        isReloadingLibrary = true
        core.notifyLibraryChanged()
        isReloadingLibrary = false
    }

    func scopeTitle(_ scope: FfiSnippetScope) -> String {
        switch scope {
        case .global: String(localized: "Global")
        case .kind(let kind): kind == .ssh ? String(localized: "SSH sessions") : String(localized: "Local Shell sessions")
        case .group(let id): String(localized: "Group · \(groups.first { $0.id == id }?.name ?? id)")
        case .profile(let id): String(localized: "Session · \(profiles.first { $0.id == id }?.name ?? id)")
        }
    }

    func start(repeating: Bool, snippet: FfiSnippet? = nil) {
        let text = snippet?.text ?? draft.text
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        guard allTargets || (context != nil && context?.kind != .sftp) else {
            onNotice?(String(localized: "No sessions available for sending"), true)
            return
        }
        clampRepeatValues()
        do {
            try core.senderStart(request: FfiSenderRequest(text: text, ending: snippet?.lineEnding ?? draft.ending, target: allTargets ? nil : context?.id, repeating: repeating, intervalSeconds: UInt32(interval), count: UInt32(count)), sessions: tabs.map(\.id))
            if repeating {
                runningTitle = text.components(separatedBy: .newlines).first { !$0.trimmingCharacters(in: .whitespaces).isEmpty } ?? ""
                runningTarget = allTargets ? String(localized: "All sessions") : String(localized: "Current session · \(context?.name ?? "")")
                runningInterval = interval
                do {
                    try core.setSetting(key: "sender.interval", json: String(interval))
                    try core.setSetting(key: "sender.count", json: String(count))
                } catch { onNotice?(String(localized: "The repeat task started, but its settings could not be saved: \(CoreMessageText.text(for: error))"), true) }
            }
            tick()
            if manualRunning || repeatRunning { scheduleTimer() }
        } catch { onNotice?(String(localized: "Failed to send: \(CoreMessageText.text(for: error))"), true) }
    }

    func stop(repeating: Bool) { core.senderStop(repeating: repeating); tick() }

    func shutdown() {
        if let libraryObserver { NotificationCenter.default.removeObserver(libraryObserver) }
        libraryObserver = nil
        timer?.invalidate()
        timer = nil
        core.senderStop(repeating: false)
        core.senderStop(repeating: true)
    }

    private func scheduleTimer() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    func tick() {
        let status = core.senderTick(sessions: tabs.map(\.id))
        manualRunning = status.manualRunning
        repeatRunning = status.repeatRunning
        let rounds = status.count == 0 ? String(localized: "Rounds: \(status.rounds)") : String(localized: "Rounds: \(status.rounds) / \(status.count)")
        progress = "\(rounds) · " + (status.nextInMs == 0 ? String(localized: "Sending") : String(localized: "Sending in \((status.nextInMs + 999) / 1000) seconds"))
        if !status.notices.isEmpty { onNotice?(status.notices.map { CoreMessageText.text($0) }.joined(separator: String(localized: "; ")), status.hasFailures) }
        if !manualRunning && !repeatRunning { timer?.invalidate(); timer = nil }
        onChange?()
    }
}
