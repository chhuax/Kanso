import Foundation
import KansoUI
import os

/// 文件浏览器打开路径的耗时日志（`log stream --predicate 'category == "filer-timing"'` 查看），用于排查"点了没反应"。
let filerTimingLog = Logger(subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "filer-timing")

// 辅助 Filer 的 view model（filer.md §1–§8）。纯逻辑：不含视图，不碰真实文件系统与 core，
// 数据来源、传输引擎、系统对话框全部经协议注入，所以能在测试里完整驱动。

/// 面板跟随的标签的快照（只含面板关心的字段）。
struct FilerTabSnapshot: Equatable, Sendable {
    var session: SessionID
    var kind: TabKind
    var name: String
    /// `user@host`，只有 SSH 有。
    var userHost: String?
    var connection: TabConnection
    /// shell 上报的目录所属主机（OSC 7 的 host）。
    var cwdHost: String?

    var source: FilerSource {
        kind == .local ? .local : .remote(session: session, name: name, userHost: userHost)
    }

    var isConnected: Bool { connection == .connected }
    var isClosed: Bool {
        if case .closed = connection { return true }
        return false
    }
}

/// 面板对外部世界的依赖。生产实现在 `FilerCoordinator`，测试注入替身。
@MainActor
protocol FilerEnvironment: AnyObject {
    func dataSource(for tab: FilerTabSnapshot) -> FilerDataSource
    func transferEngine(for tab: FilerTabSnapshot) -> FilerTransferEngine
    var localCopyEngine: FilerTransferEngine { get }
    /// 取 shell 的真实工作目录（`core.workingDirectory`）。
    func workingDirectory(session: SessionID) async throws -> String
    /// 本机主机名集合（含 `localhost`），用于判断 shell 上报的目录是否属于本机。
    func localHostNames() -> Set<String>
}

struct FilerDeleteRequest: Equatable, Sendable {
    struct Item: Equatable, Sendable {
        var name: String
        var path: String
        var kind: FilerEntry.Kind
    }

    var items: [Item]
    var source: FilerSource

    /// 标题：单项显示"删除文件 / 目录 / 链接"，多项显示数量。
    var title: String {
        guard items.count == 1, let item = items.first else {
            return String(localized: "Delete \(items.count) items")
        }
        switch item.kind {
        case .file: return String(localized: "Delete File")
        case .directory: return String(localized: "Delete Folder")
        case .link: return String(localized: "Delete Link")
        }
    }

    /// 不可撤销说明（filer.md §5.4）。批量时合并成一句覆盖所有类型的说明。
    var warning: String {
        guard items.count == 1, let item = items.first else {
            return String(localized: "Items are removed permanently and can’t be restored. Only empty folders can be deleted; links are removed without touching their targets.")
        }
        switch item.kind {
        case .file: return String(localized: "The file will be removed and can’t be restored.")
        case .directory: return String(localized: "Only empty folders can be deleted. This can’t be undone.")
        case .link: return String(localized: "Only the link is removed; its target is not affected. This can’t be undone.")
        }
    }
}

/// 面板对系统对话框的依赖。生产实现是 sheet / `NSOpenPanel` 等；测试注入替身，不弹模态框。
@MainActor
protocol FilerPrompter: AnyObject {
    func chooseEditorApplication() async -> URL?
    func openRemoteCopy(_ url: URL, application: URL?) async throws
    func confirmLargeRemoteEdit(name: String, source: String) async -> Bool
    func resolveRemoteEditConflict(name: String, source: String) async -> RemoteEditConflictChoice
    func showRetainedRemoteEdits(_ urls: [URL]) async
    func confirmDelete(_ request: FilerDeleteRequest) async -> Bool
    func resolveConflict(_ request: FilerConflictRequest) async -> FilerConflictDecision
    /// 选择要上传的文件或目录（多选）；取消返回空数组。
    func chooseUploadSources(directories: Bool) async -> [URL]
    /// 单个文件下载：保存面板，默认名为远端名称；取消返回 nil。
    func chooseDownloadFile(defaultName: String) async -> URL?
    /// 目录或多选下载：选择父目录；取消返回 nil。
    func chooseDownloadDirectory() async -> URL?
    /// 写剪贴板；失败返回 false（必须向用户说明，不能表现为成功）。
    func copyToPasteboard(_ text: String) -> Bool
    /// 用默认应用打开本地文件；失败返回 false。
    func openLocal(_ path: String) -> Bool
}

enum FilerAvailability: Equatable, Sendable {
    case available
    case noTab
    /// 纯 SFTP 标签：主区已是双栏文件浏览器（F1b），辅助面板不重复显示。
    case unavailableSftp
}

enum FilerPhase: Equatable, Sendable {
    case idle
    case waitingConnection
    case loading
    case loaded
    case failed(FilerError)
}

enum FilerEditing: Equatable, Sendable {
    case none
    case createFile
    case createDirectory
    case rename(FilerEntry)
}

struct FilerNotice: Equatable, Sendable {
    var text: String
    var isError: Bool
}

enum FilerDropVerdict: Equatable, Sendable {
    case accept(hint: String)
    case reject(reason: String)
    /// 自己拖出的条目又拖回来：静默忽略，不当作上传也不当作错误（filer.md §7.5）。
    case ignore
}

/// 拖放落点判定。按用户真实松手位置（窗口坐标，单位是点）判断，不在这里做像素缩放：
/// AppKit 的 `draggingLocation` 已经是点坐标，把它再除以 backingScaleFactor 会把落点算到面板左边
/// （旧版的 Retina 缺陷，filer.md §7.1）。需要从像素换算的调用方用 `points(fromPixels:scale:)`。
enum FilerDropGeometry {
    static func contains(locationInWindow: CGPoint, panelFrameInWindow: CGRect) -> Bool {
        panelFrameInWindow.contains(locationInWindow)
    }

    static func points(fromPixels point: CGPoint, scale: CGFloat) -> CGPoint {
        CGPoint(x: point.x / scale, y: point.y / scale)
    }
}

@MainActor
final class FilerModel {
    let isDualPane: Bool
    // MARK: 依赖

    private var env: FilerEnvironment
    private let prompter: FilerPrompter
    private let clock: () -> TimeInterval

    // MARK: 状态（只读给视图）

    private(set) var tab: FilerTabSnapshot?
    private(set) var availability: FilerAvailability = .noTab
    private(set) var phase: FilerPhase = .idle
    /// 正在浏览（或正在读取、读取失败）的目录。
    private(set) var directory: String?
    /// 已排序、包含隐藏项的全部条目。
    private(set) var entries: [FilerEntry] = [] { didSet { rebuildListing() } }
    private(set) var listingRevision = 0
    private(set) var visibleEntries: [FilerEntry] = []
    private(set) var stats = FilerStats([])
    /// 最近一次成功读取完成的目录（路径栏 Esc 恢复到它）。
    private(set) var lastSuccessfulDirectory: String?
    private(set) var selectedPaths: [String] = [] { didSet { rebuildSelection() } }
    private(set) var selectedEntries: [FilerEntry] = []
    private(set) var editing: FilerEditing = .none
    private(set) var notice: FilerNotice?
    private(set) var transferStatus: FilerTransferStatus?
    /// 路径栏当前展示的"是否在编辑"由视图管理；这里只提供来源。
    var showsHiddenFiles = true {
        didSet {
            guard showsHiddenFiles != oldValue else { return }
            rebuildListing()
            let visible = Set(visibleEntries.map(\.path))
            selectedPaths = selectedPaths.filter(visible.contains)
            changed()
        }
    }

    /// 面板是否可见。隐藏时不读取目录（避免为看不见的面板读盘 / 占用连接），显示时补读。
    var isVisible = true {
        didSet {
            guard isVisible != oldValue, isVisible else { return }
            loadIfNeeded()
        }
    }

    /// 状态变化回调（视图重绘）。
    var onChange: (() -> Void)?
    /// 需要展开面板（⌘J 定位成功时）。
    var onRequestShow: (() -> Void)?
    /// 需要在状态栏显示的提示（定位结果等）。
    var onStatusMessage: ((FilerNotice) -> Void)?
    var onOpenRemoteFile: ((FilerEntry, FilerTabSnapshot, URL?, Bool) -> Void)?
    var onCancelRemoteDownload: (() -> Void)?
    var recentEditorApplications: [URL] = []
    private(set) var remoteEditNotice: FilerNotice?
    private(set) var remoteEditDownload: FilerTransferStatus?

    func showRemoteEditStatus(_ notice: FilerNotice?, download: FilerTransferStatus?) {
        remoteEditNotice = notice
        remoteEditDownload = download
        changed()
    }

    // MARK: 内部

    private var dataSource: FilerDataSource?
    private var generation = 0
    private var loadTask: Task<Void, Never>?
    private var needsInitialLoad = false
    private var homeDirectory: String?
    /// 每个标签最后成功读取的目录（会话存活期间，D1）。
    private var remembered: [SessionID: String] = [:]
    /// 定位得到、但标签当时不是面板跟随对象的目录：该标签下次被跟随时优先使用。
    private var pendingTarget: [SessionID: String] = [:]
    private var batchRunner: FilerBatchRunner?
    private var batchTask: Task<Void, Never>?
    private var locateToken = 0
    private var linkToken = 0
    /// 目录读完后要选中的路径（命令面板「在 Filer 中定位」）。
    private var pendingRevealPath: String?

    init(env: FilerEnvironment, prompter: FilerPrompter, isDualPane: Bool = false, clock: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.env = env
        self.prompter = prompter
        self.isDualPane = isDualPane
        self.clock = clock
    }

    /// 构造后换环境（协调者在 `self` 可用后接管）。
    func replaceEnvironment(_ env: FilerEnvironment) { self.env = env }

    // MARK: - 派生

    var source: FilerSource? { tab?.source }
    var isStale: Bool { (source?.isRemote == true || isDualPane) && tab?.isClosed == true }
    var isLoading: Bool { phase == .loading }
    var hasActiveBatch: Bool { batchTask != nil }

    /// 写操作（新建、删除、重命名、上传）是否可用：来源可用、没断线、目录已完整读到。
    /// 正在读目录时不允许基于旧条目发起写入（filer.md §4.3）。
    var canWrite: Bool {
        availability == .available && !isStale && phase == .loaded && directory != nil
            && ((!isDualPane && tab?.kind == .local) || tab?.isConnected == true)
    }

    var canUploadOrDownload: Bool { source?.isRemote == true && canWrite }

    private func rebuildSelection() {
        let set = Set(selectedPaths)
        selectedEntries = entries.filter { set.contains($0.path) }
    }

    private func rebuildListing() {
        visibleEntries = showsHiddenFiles ? entries : entries.filter { !$0.isHidden }
        stats = FilerStats(visibleEntries)
        listingRevision += 1
        rebuildSelection()
    }

    /// 无法使用时给状态栏条目的说明。
    var unavailableReason: String? {
        switch availability {
        case .available: nil
        case .noTab: String(localized: "No session is open")
        case .unavailableSftp: String(localized: "The file browser is already open in this tab")
        }
    }

    private func changed() { onChange?() }

    private func setNotice(_ text: String?, isError: Bool = true) {
        notice = text.map { FilerNotice(text: $0, isError: isError) }
        changed()
    }

    // MARK: - 跟随标签

    /// 面板跟随的标签变化（活动 pane 的活动标签变了，或其连接状态 / 名称变了）。幂等。
    func follow(_ newTab: FilerTabSnapshot?) {
        guard newTab != tab else { return }
        let old = tab
        tab = newTab
        guard let t = newTab else {
            resetListing()
            availability = .noTab
            dataSource = nil
            changed()
            return
        }
        guard t.kind != .sftp || isDualPane else {
            resetListing()
            availability = .unavailableSftp
            dataSource = nil
            changed()
            return
        }
        availability = .available
        if old?.session != t.session || dataSource == nil {
            resetListing()
            dataSource = env.dataSource(for: t)
            needsInitialLoad = true
            loadIfNeeded()
            changed()
            return
        }
        // 同一个会话：连接状态变化。断线保留远端身份、最后目录与（过期的）列表；重连后重新读取（§1.2）。
        if (t.source.isRemote || isDualPane), old?.isConnected != true, t.isConnected, directory != nil {
            refresh()
        } else if (t.source.isRemote || isDualPane), !t.isConnected, old?.isConnected == true {
            // 断线：正在读取的目录不再有意义，停掉；已有列表保留并标为过期。
            if phase == .loading || phase == .waitingConnection {
                cancelLoad()
                phase = .idle
            }
            editing = .none
        } else if needsInitialLoad {
            loadIfNeeded()
        }
        changed()
    }

    /// 会话关闭：忘掉它的记忆。
    func forget(session: SessionID) {
        remembered[session] = nil
        pendingTarget[session] = nil
        if tab?.session == session { follow(nil) }
    }

    func reconnect(_ snapshot: FilerTabSnapshot) {
        forget(session: snapshot.session)
        lastSuccessfulDirectory = nil
        homeDirectory = nil
        follow(snapshot)
    }

    private func resetListing() {
        cancelLoad()
        entries = []
        directory = nil
        selectedPaths = []
        editing = .none
        phase = .idle
        needsInitialLoad = false
    }

    private func cancelLoad() {
        loadTask?.cancel()
        loadTask = nil
        generation &+= 1
    }

    /// 首次（或面板重新可见后）读取：优先用定位得到的目录，其次是该标签记住的目录，最后才是家目录。
    private func loadIfNeeded() {
        guard needsInitialLoad, isVisible, availability == .available, let t = tab, dataSource != nil else { return }
        if t.source.isRemote || isDualPane {
            guard t.isConnected else {
                phase = t.isClosed ? .idle : .waitingConnection
                return
            }
        }
        needsInitialLoad = false
        if let target = pendingTarget.removeValue(forKey: t.session) ?? remembered[t.session] {
            startLoad(path: target, keepEntries: false)
        } else {
            startLoad(path: nil, keepEntries: false)
        }
    }

    // MARK: - 读取目录

    /// 开始读取。`path == nil` 表示家目录。每次调用都让之前的读取失效：
    /// 来自旧请求的成功 / 失败 / 完成都不会覆盖新结果（filer.md §4.3）。
    private func startLoad(path: String?, keepEntries: Bool) {
        guard !isDualPane || tab?.isConnected == true else { return }
        guard let ds = dataSource else { return }
        loadTask?.cancel()
        generation &+= 1
        let gen = generation
        phase = .loading
        notice = nil
        if let path { directory = path }
        let sameDirectory = path != nil && path == directory
        // 刷新同一目录时先保留旧条目直到首批到达，避免闪烁；导航到别处立即清空，旧条目不能挂在新路径下。
        if !(keepEntries && sameDirectory) { entries = [] }
        loadTask = Task { [weak self] in
            var target = path
            if target == nil {
                do { target = try await ds.homeDirectory() } catch {
                    self?.finishLoad(gen: gen, error: FilerError.from(error, path: nil), path: nil)
                    return
                }
                guard let self, self.generation == gen else { return }
                self.directory = target
                self.homeDirectory = target
                self.entries = []
                self.changed()
            }
            guard let target else { return }
            var first = true
            do {
                let listStarted = Date()
                for try await batch in ds.list(path: target) {
                    guard let self, self.generation == gen else { return }
                    if first { filerTimingLog.info("首批 \(batch.count) 项 \(Int(Date().timeIntervalSince(listStarted) * 1000)) ms") }
                    if first, keepEntries, sameDirectory { self.entries = [] }
                    first = false
                    self.entries = Self.merge(self.entries, FilerSort.sorted(batch))
                    self.changed()
                }
                guard let self, self.generation == gen else { return }
                if first { self.entries = [] }
                self.phase = .loaded
                self.lastSuccessfulDirectory = target
                if let session = self.tab?.session { self.remembered[session] = target }
                self.loadTask = nil
                if let reveal = self.pendingRevealPath {
                    self.pendingRevealPath = nil
                    if self.entries.contains(where: { $0.path == reveal }) {
                        self.selectedPaths = [reveal]
                    }
                }
                self.changed()
            } catch {
                self?.finishLoad(gen: gen, error: FilerError.from(error, path: target), path: target)
            }
        }
    }

    private func finishLoad(gen: Int, error: FilerError, path: String?) {
        guard generation == gen else { return }
        // 读取失败：清空列表，只显示错误。旧列表挂在新路径文字下面会让用户以为它属于新目录（filer.md §2、§4.4）。
        entries = []
        selectedPaths = []
        pendingRevealPath = nil
        if let path { directory = path }
        phase = .failed(error)
        loadTask = nil
        changed()
    }

    /// 有序合并两个已排序数组。
    static func merge(_ a: [FilerEntry], _ b: [FilerEntry]) -> [FilerEntry] {
        guard !a.isEmpty else { return b }
        guard !b.isEmpty else { return a }
        var result: [FilerEntry] = []
        result.reserveCapacity(a.count + b.count)
        var i = 0, j = 0
        while i < a.count, j < b.count {
            if FilerSort.areInIncreasingOrder(b[j], a[i]) {
                result.append(b[j]); j += 1
            } else {
                result.append(a[i]); i += 1
            }
        }
        result.append(contentsOf: a[i...])
        result.append(contentsOf: b[j...])
        return result
    }

    /// 测试用：等当前批次结束。
    func waitForBatch() async { await batchTask?.value }

    /// 测试用：等当前读取任务结束。
    func waitUntilIdle() async {
        await loadTask?.value
    }

    // MARK: - 导航

    /// 路径栏回车：相对路径以当前目录为基准，本地支持 `~`。输入为空不做事。
    func navigate(toInput input: String) {
        guard availability == .available, let base = directory ?? lastSuccessfulDirectory else { return }
        let home: String? = source?.isRemote == false ? (isDualPane ? homeDirectory : NSHomeDirectory()) : nil
        guard let path = FilerPath.resolve(input, base: base, home: home) else { return }
        open(directory: path)
    }

    func open(directory path: String) {
        guard availability == .available else { return }
        selectedPaths = []
        editing = .none
        pendingRevealPath = nil
        startLoad(path: path, keepEntries: false)
        changed()
    }

    /// 命令面板：展开面板、打开文件所在目录并选中该文件（一层目录内）。
    func reveal(path: String) {
        guard availability == .available else { return }
        onRequestShow?()
        isVisible = true
        needsInitialLoad = false
        selectedPaths = []
        editing = .none
        pendingRevealPath = path
        startLoad(path: FilerPath.parent(of: path), keepEntries: false)
        changed()
    }

    func goUp() {
        guard let directory else { return }
        let parent = FilerPath.parent(of: directory)
        // 已在根目录：停在根目录，不重复读取。
        guard parent != FilerPath.normalize(directory) else { return }
        open(directory: parent)
    }

    func goHome() {
        guard availability == .available else { return }
        selectedPaths = []
        editing = .none
        startLoad(path: nil, keepEntries: false)
        changed()
    }

    /// 刷新当前目录。成功后重置旧选项，防止后续下载或删除仍指向已经消失的条目（filer.md §3）。
    /// `keepingNotice`：操作（新建、删除、传输）完成后的内部刷新保留那次操作的错误提示。
    func refresh(keepingNotice: Bool = false) {
        guard availability == .available else { return }
        let kept = keepingNotice ? notice : nil
        defer { if keepingNotice { notice = kept; changed() } }
        guard let directory else {
            needsInitialLoad = true
            loadIfNeeded()
            return
        }
        selectedPaths = []
        startLoad(path: directory, keepEntries: true)
        changed()
    }

    /// 路径栏 Esc：放弃输入，回到最后成功读取的目录文字。返回应显示的文字。
    var restoredPathText: String { lastSuccessfulDirectory ?? directory ?? "" }

    // MARK: - 选择

    /// 视图把选中的行同步回来（按路径记录，条目因分批插入而移位时选中对象不变）。
    func setSelection(paths: [String]) {
        let visible = Set(visibleEntries.map(\.path))
        let new = paths.filter(visible.contains)
        guard new != selectedPaths else { return }
        selectedPaths = new
        changed()
    }

    // MARK: - 激活

    /// 双击 / 回车。
    func activate(_ entry: FilerEntry) {
        switch entry.kind {
        case .directory:
            open(directory: entry.path)
        case .file:
            openFile(entry)
        case .link:
            activateLink(entry)
        }
    }

    private func openFile(_ entry: FilerEntry) {
        guard !isDualPane else { return }
        guard source?.isRemote == false else {
            openRemoteFile(entry)
            return
        }
        if !prompter.openLocal(entry.path) {
            setNotice(String(localized: "Couldn’t open “\(entry.name)”."))
        }
    }

    func openRemoteFile(_ entry: FilerEntry, application: URL? = nil, chooseApplication: Bool = false) {
        guard let tab, tab.kind == .ssh, tab.isConnected, !entry.isDirectory else { return }
        onOpenRemoteFile?(entry, tab, application, chooseApplication)
    }

    /// 用户激活链接时才确认目标类型：目录进入，文件按文件打开，悬空或不可访问时说明原因（filer.md §4.4）。
    private func activateLink(_ entry: FilerEntry) {
        guard let ds = dataSource, let tabSession = tab?.session else { return }
        linkToken &+= 1
        let token = linkToken
        Task { [weak self] in
            do {
                let target = try await ds.resolveLink(entry)
                guard let self, self.linkToken == token, self.tab?.session == tabSession else { return }
                switch target {
                case .directory(let path): self.open(directory: path)
                case .file: self.openFile(entry)
                }
            } catch {
                guard let self, self.linkToken == token, self.tab?.session == tabSession else { return }
                self.setNotice(FilerError.from(error, path: entry.path).message(source: self.source))
            }
        }
    }

    // MARK: - 新建 / 重命名

    func beginCreate(directory: Bool) {
        guard guardWritable() else { return }
        editing = directory ? .createDirectory : .createFile
        notice = nil
        changed()
    }

    func beginRename() {
        guard guardWritable() else { return }
        guard selectedEntries.count == 1, let entry = selectedEntries.first else { return }
        editing = .rename(entry)
        notice = nil
        changed()
    }

    func cancelEditing() {
        guard editing != .none else { return }
        editing = .none
        changed()
    }

    private func guardWritable() -> Bool {
        if canWrite { return true }
        if isStale {
            setNotice(String(localized: "The connection is closed. Reconnect before changing files."))
        } else if isLoading {
            setNotice(String(localized: "Wait until the folder finishes loading."), isError: false)
        }
        return false
    }

    /// 提交输入的名称（Enter 或失焦）。返回 true 表示输入行应关闭。
    /// 校验失败：Enter 时保留输入行让用户修改（提示原因）；失焦时直接放弃并提示。
    @discardableResult
    func commitEditing(name raw: String, viaBlur: Bool) async -> Bool {
        let editing = self.editing
        guard editing != .none, canWrite, let directory, let ds = dataSource, let tabSession = tab?.session else {
            self.editing = .none
            changed()
            return true
        }
        let existing = Set(entries.map(\.name))
        let current: String? = if case .rename(let e) = editing { e.name } else { nil }
        switch FilerNameValidator.validate(raw, existing: existing, allowing: current) {
        case .failure(.empty):
            self.editing = .none
            changed()
            return true
        case .failure(let failure):
            let text: String = switch failure {
            case .exists: String(localized: "“\(raw.trimmingCharacters(in: .whitespaces))” already exists.")
            case .hasSeparator: String(localized: "A name can’t contain “/”.")
            default: String(localized: "“\(raw)” isn’t a valid name.")
            }
            if viaBlur { self.editing = .none }
            setNotice(text)
            return viaBlur
        case .success(let name):
            if case .rename(let e) = editing, e.name == name {
                self.editing = .none
                changed()
                return true
            }
            let path = FilerPath.join(directory, name)
            do {
                switch editing {
                case .createFile: try await ds.createFile(at: path)
                case .createDirectory: try await ds.createDirectory(at: path)
                case .rename(let e): try await ds.rename(from: e.path, to: path)
                case .none: break
                }
            } catch {
                self.editing = .none
                // 失败显示名称与原因，不保留失败的输入行（filer.md §5.3）。
                setNotice(String(localized: "Couldn’t create “\(name)”: ") + FilerError.from(error, path: path).message(source: source))
                return true
            }
            self.editing = .none
            if tab?.session == tabSession, self.directory == directory {
                setNotice(nil)
                refresh(keepingNotice: true)
            }
            return true
        }
    }

    // MARK: - 删除

    /// 删除选中项：确认一次（批量只确认一次并列出数量），非空目录不递归。
    func deleteSelected() async {
        guard guardWritable() else { return }
        let items = selectedEntries
        guard !items.isEmpty, let source, let ds = dataSource, let tabSession = tab?.session else { return }
        let request = FilerDeleteRequest(
            items: items.map { .init(name: $0.name, path: $0.path, kind: $0.kind) }, source: source
        )
        guard await prompter.confirmDelete(request) else { return }
        // 确认期间用户可能已切换标签：删除作用于确认时锁定的会话与条目路径，不是当前面板里同一位置的另一项。
        var failures: [String] = []
        for item in items {
            do { try await ds.remove(item) } catch {
                let e = FilerError.from(error, path: item.path)
                failures.append(e.kind == .notFound
                    ? String(localized: "“\(item.name)” no longer exists.")
                    : e.message(source: source))
            }
        }
        if !failures.isEmpty { setNotice(failures.joined(separator: "\n")) }
        if tab?.session == tabSession { refresh(keepingNotice: true) }
    }

    // MARK: - 复制文字

    func copyPath(of entries: [FilerEntry]) { copyText(entries.map(\.path).joined(separator: "\n")) }
    func copyName(of entries: [FilerEntry]) { copyText(entries.map(\.name).joined(separator: "\n")) }
    func copyDirectoryPath() { if let directory { copyText(directory) } }

    private func copyText(_ text: String) {
        guard !text.isEmpty else { return }
        if !prompter.copyToPasteboard(text) { setNotice(String(localized: "Couldn’t copy to the clipboard.")) }
    }

    // MARK: - ⌘J 定位

    /// ⌘J、终端右键菜单、面板内"定位终端目录"三个入口共用（filer.md §2）。
    /// 锁定发起时的标签 → 取真实目录 → 展开面板并**直接读取目标目录**（不先显示家目录）。
    /// - Parameter session: 发起定位的标签；nil 取面板当前跟随的标签。
    func locateWorkingDirectory(session requested: SessionID? = nil) async {
        let locked: FilerTabSnapshot? = requested == nil ? tab : (requested == tab?.session ? tab : lockedTab(requested))
        guard let locked else {
            report(String(localized: "No session to locate"))
            return
        }
        locateToken &+= 1
        let token = locateToken
        let session = locked.session
        guard locked.kind != .sftp else {
            report(String(localized: "A file session has no shell working directory."))
            return
        }
        guard locked.isConnected else {
            report(String(localized: "The session isn’t connected."))
            return
        }
        let path: String
        let started = Date()
        defer { filerTimingLog.info("locate 查询工作目录 \(Int(Date().timeIntervalSince(started) * 1000)) ms") }
        do { path = try await env.workingDirectory(session: session) } catch {
            guard locateToken == token else { return }
            report(String(localized: "Couldn’t get the working directory: ") + FilerError.fromCore(error, path: nil).reason)
            return
        }
        // 过时的结果不能作用于后来切换到的标签：请求期间发起了新的定位，或标签被关闭，直接丢弃。
        guard locateToken == token else { return }
        // 本地 shell 里手工登录另一台机器后，远端上报的目录不是本机路径。
        if locked.kind == .local, let host = locked.cwdHost, !host.isEmpty, !Self.isLocalHost(host, names: env.localHostNames()) {
            report(String(localized: "The terminal’s directory belongs to “\(host)”, not this Mac, so it can’t be shown here."))
            return
        }
        guard path.hasPrefix("/") else {
            report(String(localized: "The working directory “\(path)” isn’t an absolute path."))
            return
        }
        if tab?.session == session {
            onRequestShow?()
            isVisible = true
            needsInitialLoad = false
            selectedPaths = []
            editing = .none
            // 重复请求同一路径也重新读取，不因路径文字没变而忽略。
            startLoad(path: path, keepEntries: false)
            changed()
        } else {
            // 面板已跟随别的标签：只更新原标签的记忆位置，不抢走当前面板。
            pendingTarget[session] = path
        }
        let notice = FilerNotice(text: String(localized: "Located “\(path)” in the file browser."), isError: false)
        onStatusMessage?(notice)
    }

    private var lockedSnapshotProvider: ((SessionID) -> FilerTabSnapshot?)?

    /// 外部（协调者）提供按会话取快照的能力：右键菜单点的可能不是面板当前跟随的标签。
    func setSnapshotProvider(_ provider: @escaping (SessionID) -> FilerTabSnapshot?) {
        lockedSnapshotProvider = provider
    }

    private func lockedTab(_ session: SessionID?) -> FilerTabSnapshot? {
        session.flatMap { lockedSnapshotProvider?($0) }
    }

    private func report(_ text: String) {
        let n = FilerNotice(text: text, isError: true)
        onStatusMessage?(n)
    }

    /// shell 上报的主机是否是本机。
    /// 本机名集合：`gethostname` 的结果及其不带域名的短名。
    ///
    /// 不能用 `Host.current().names`：它会做反向 DNS 解析，首次调用在主线程上实测要 1 秒以上，
    /// 点文件浏览器时要点好几次才出来就是它造成的。OSC 7 里的主机名来自 shell 的 `$HOST`，
    /// 也就是 `gethostname`，所以只比对它就够了。结果只算一次。
    static let localHostNames: Set<String> = {
        var buf = [CChar](repeating: 0, count: 256)
        guard gethostname(&buf, buf.count - 1) == 0 else { return [] }
        let name = String(cString: buf)
        guard !name.isEmpty else { return [] }
        var names: Set<String> = [name]
        if let short = name.split(separator: ".").first { names.insert(String(short)) }
        return names
    }()

    static func isLocalHost(_ host: String, names: Set<String>) -> Bool {
        let h = host.lowercased()
        return h == "localhost" || names.contains { $0.lowercased() == h }
    }

    // MARK: - 上传 / 下载 / 复制

    private func guardNoBatch() -> Bool {
        guard batchTask == nil else {
            setNotice(String(localized: "Busy. Wait for the current transfer to finish."))
            return false
        }
        return true
    }

    /// 上传菜单：系统选择器多选文件或目录。
    func chooseAndUpload(directories: Bool) async {
        guard source?.isRemote == true else {
            setNotice(String(localized: "Uploading only works in a connected SSH session."), isError: false)
            return
        }
        guard guardWritable(), guardNoBatch() else { return }
        // 锁定发起时的会话与目录：选择器打开期间用户可能切标签。
        guard let t = tab, let dir = directory else { return }
        let urls = await prompter.chooseUploadSources(directories: directories)
        guard !urls.isEmpty else { return }
        guard tab?.session == t.session, directory == dir, canWrite, guardNoBatch() else {
            setNotice(String(localized: "The folder changed while choosing. Try the upload again."), isError: false)
            return
        }
        startUpload(urls: urls, tab: t, directory: dir)
    }

    /// 拖放入口也走这里（已通过 `dropVerdict`）。
    func startUpload(urls: [URL], tab t: FilerTabSnapshot, directory dir: String) {
        let items = urls.map { url in
            FilerTransferItem(
                kind: .upload, name: url.lastPathComponent, source: url.path, destination: dir,
                isDirectory: (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory ?? false
            )
        }
        let description = "\(t.source.title) · \(dir)"
        runBatch(items: items, engine: env.transferEngine(for: t), origin: description, destinationDescription: description,
                 session: t.session, directory: dir)
    }

    func transfer(_ entries: [FilerEntry], kind: FilerTransferKind, to destination: String,
                  destinationDescription: String, onFinish: @escaping () -> Void) {
        guard canWrite, guardNoBatch(), let tab else { return }
        let items = entries.map {
            FilerTransferItem(kind: kind, name: $0.name, source: $0.path, destination: destination, isDirectory: $0.isDirectory)
        }
        runBatch(items: items, engine: env.transferEngine(for: tab), origin: tab.source.title,
                 destinationDescription: destinationDescription, session: tab.session,
                 directory: nil, onFinish: onFinish)
    }

    /// 下载选中项：单个文件用保存面板，目录或多选选父目录。
    func downloadSelected() async {
        guard source?.isRemote == true else { return }
        guard tab?.isConnected == true, !isStale else {
            setNotice(String(localized: "The connection is closed. Reconnect before downloading."))
            return
        }
        guard guardNoBatch() else { return }
        let picked = selectedEntries
        guard !picked.isEmpty, let t = tab, let dir = directory else { return }
        var items: [FilerTransferItem] = []
        if picked.count == 1, let one = picked.first, !one.isDirectory {
            guard let url = await prompter.chooseDownloadFile(defaultName: one.name) else { return }
            var item = FilerTransferItem(kind: .download, name: one.name, source: one.path,
                                         destination: url.deletingLastPathComponent().path)
            if url.lastPathComponent != one.name { item.renamedTo = url.lastPathComponent }
            // 保存面板里用户已经确认过替换：直接覆盖，不再询问第二次。
            if FileManager.default.fileExists(atPath: url.path) { item.initialPolicy = .overwrite }
            items = [item]
        } else {
            guard let parent = await prompter.chooseDownloadDirectory() else { return }
            items = picked.map {
                FilerTransferItem(kind: .download, name: $0.name, source: $0.path, destination: parent.path, isDirectory: $0.isDirectory)
            }
        }
        guard tab?.session == t.session, guardNoBatch() else { return }
        let description = items.first.map { $0.destination } ?? ""
        runBatch(items: items, engine: env.transferEngine(for: t), origin: "\(t.source.title) · \(dir)",
                 destinationDescription: description, session: t.session, directory: nil)
    }

    /// 拖放判定（不做事）：高亮与底部说明、不可接受时的原因都来自这里。
    func dropVerdict(urls: [URL], isFromSelf: Bool) -> FilerDropVerdict {
        if isFromSelf { return .ignore }
        switch availability {
        case .noTab: return .reject(reason: String(localized: "No session is open"))
        case .unavailableSftp: return .reject(reason: String(localized: "Use the file browser in this tab to transfer files"))
        case .available: break
        }
        if isStale { return .reject(reason: String(localized: "The connection is closed")) }
        if directory == nil || phase != .loaded {
            return .reject(reason: isLoading ? String(localized: "Wait until the folder finishes loading") : String(localized: "Open a folder first"))
        }
        if hasActiveBatch { return .reject(reason: String(localized: "Busy. Wait for the current transfer to finish.")) }
        let files = urls.filter(\.isFileURL)
        guard !files.isEmpty else { return .reject(reason: String(localized: "Only files and folders can be dropped here")) }
        if source?.isRemote == true { return .accept(hint: String(localized: "Drop to upload")) }
        let usable = files.filter { Self.localCopyProblem($0.path, into: directory ?? "") == nil }
        if usable.isEmpty, let first = files.first, let problem = Self.localCopyProblem(first.path, into: directory ?? "") {
            return .reject(reason: problem)
        }
        return .accept(hint: String(localized: "Drop to copy here"))
    }

    /// 本地复制的前置检查：已在此目录 / 复制到自身或后代。
    static func localCopyProblem(_ source: String, into directory: String) -> String? {
        if FilerPath.parent(of: source) == FilerPath.normalize(directory) {
            return String(localized: "Already in this folder")
        }
        if FilerPath.isSameOrDescendant(directory, of: source) {
            return String(localized: "Can’t copy a folder into itself")
        }
        return nil
    }

    /// 松手。返回是否已经开始处理（拒绝时已给出原因）。
    @discardableResult
    func performDrop(urls: [URL], isFromSelf: Bool) -> Bool {
        switch dropVerdict(urls: urls, isFromSelf: isFromSelf) {
        case .ignore:
            return false
        case .reject(let reason):
            setNotice(reason)
            return false
        case .accept:
            guard let t = tab, let dir = directory else { return false }
            let files = urls.filter(\.isFileURL)
            if source?.isRemote == true {
                startUpload(urls: files, tab: t, directory: dir)
                return true
            }
            var skippedHere = 0
            var items: [FilerTransferItem] = []
            var rejected: [String] = []
            for url in files {
                if let problem = Self.localCopyProblem(url.path, into: dir) {
                    if problem == String(localized: "Already in this folder") { skippedHere += 1 } else { rejected.append(problem) }
                    continue
                }
                items.append(FilerTransferItem(
                    kind: .copy, name: url.lastPathComponent, source: url.path, destination: dir,
                    isDirectory: (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory ?? false
                ))
            }
            var extra: [String] = []
            if skippedHere > 0 { extra.append(String(localized: "\(skippedHere) already in this folder, skipped")) }
            extra.append(contentsOf: rejected)
            runBatch(items: items, engine: env.localCopyEngine, origin: "\(source?.title ?? "") · \(dir)",
                     destinationDescription: "\(source?.title ?? "") · \(dir)", session: t.session, directory: dir,
                     preface: extra)
            return true
        }
    }

    /// 面板外发起的传输（file promise 交付）的进度显示。批次进行中不覆盖批次的进度。
    func showExternalStatus(_ status: FilerTransferStatus) {
        guard batchTask == nil else { return }
        transferStatus = status
        changed()
    }

    /// 取消当前批次（进度区的取消按钮）。
    func cancelTransfers() {
        if let batchRunner { batchRunner.cancel() } else { onCancelRemoteDownload?() }
    }

    func dismissTransferStatus() {
        guard batchTask == nil, transferStatus != nil else { return }
        transferStatus = nil
        changed()
    }

    private func runBatch(
        items: [FilerTransferItem], engine: FilerTransferEngine, origin: String, destinationDescription: String,
        session: SessionID, directory lockedDirectory: String?, preface: [String] = [], onFinish: (() -> Void)? = nil
    ) {
        guard !items.isEmpty else {
            if !preface.isEmpty { setNotice(preface.joined(separator: "\n"), isError: false) }
            return
        }
        guard batchTask == nil else { return }
        let runner = FilerBatchRunner()
        batchRunner = runner
        notice = nil
        let task = Task { [weak self] in
            guard let self else { return }
            let outcome = await runner.run(
                items: items, engine: engine, origin: origin, destinationDescription: destinationDescription,
                resolveConflict: { await self.prompter.resolveConflict($0) },
                now: { self.clock() },
                update: { status in
                    self.transferStatus = status
                    self.changed()
                }
            )
            self.finishBatch(outcome, items: items, session: session, directory: lockedDirectory, preface: preface)
            onFinish?()
        }
        batchTask = task
        changed()
    }

    private func finishBatch(
        _ outcome: FilerBatchOutcome, items: [FilerTransferItem], session: SessionID, directory lockedDirectory: String?,
        preface: [String]
    ) {
        batchTask = nil
        batchRunner = nil
        var status = transferStatus ?? FilerTransferStatus(kind: items[0].kind, name: items[0].name, index: 1, count: items.count)
        status.bytesPerSecond = nil
        var messages = preface
        switch outcome.stop {
        case .none:
            status.phase = .completed(skipped: outcome.skippedFiles)
            if outcome.skippedFiles > 0 { messages.append(String(localized: "\(outcome.skippedFiles) existing files skipped")) }
        case .cancelled:
            status.phase = .cancelled
        case .failed(let text):
            // 详细错误保留在错误区域直到下一次操作清除；进度区的短提示消失后原因不丢（filer.md §6.2）。
            status.phase = .failed(text)
            messages.append(String(localized: "Failed: “\(status.name)”: ") + text)
        }
        transferStatus = status
        if !messages.isEmpty { notice = FilerNotice(text: messages.joined(separator: "\n"), isError: outcome.stop != .none && outcome.stop != .cancelled) }
        // 刷新接收侧：只在面板仍停在锁定的会话与目录时刷新，不把用户正在浏览的别处切回来。
        if tab?.session == session, lockedDirectory == nil || directory == lockedDirectory {
            if lockedDirectory != nil { refresh(keepingNotice: true) }
        }
        changed()
    }
}
