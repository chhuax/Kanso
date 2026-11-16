import Foundation

// 辅助 Filer 的纯值类型与纯函数（filer.md §3–§5）。这里不依赖 AppKit，也不碰文件系统，
// 方便在没有真实环境的情况下做单元测试。

/// 一个目录条目。本地与远端共用，字段含义与 core 的 `FfiDirEntry` 对齐。
struct FilerEntry: Identifiable, Hashable, Sendable {
    enum Kind: Sendable { case file, directory, link }

    var name: String
    /// 完整路径。
    var path: String
    /// 是真正的目录。**符号链接即使指向目录也为 false**：列举结果不跟随链接，
    /// 目标类型要等用户激活时才确认（filer.md §4.4），排序与统计也据此把链接归为"其他条目"。
    var isDirectory: Bool
    var isSymlink: Bool
    /// nil 表示未知（不能显示成 0 字节，filer.md §4.1）。
    var size: UInt64?
    var modified: Date?
    var permissions: UInt32?
    var owner: String?
    var group: String?

    var id: String { path }
    var isHidden: Bool { name.hasPrefix(".") }
    var kind: Kind { isSymlink ? .link : (isDirectory ? .directory : .file) }

    init(
        name: String, path: String, isDirectory: Bool, isSymlink: Bool = false, size: UInt64? = nil,
        modified: Date? = nil, permissions: UInt32? = nil, owner: String? = nil, group: String? = nil
    ) {
        self.name = name
        self.path = path
        self.isDirectory = isDirectory
        self.isSymlink = isSymlink
        self.size = size
        self.modified = modified
        self.permissions = permissions
        self.owner = owner
        self.group = group
    }
}

/// 面板当前浏览的来源（"本地"或某个 SSH 会话）。
enum FilerSource: Equatable, Sendable {
    case local
    case remote(session: UInt64, name: String, userHost: String?)

    var isRemote: Bool {
        if case .remote = self { return true }
        return false
    }

    /// 面板顶部与确认框里的来源标识。远端带会话名与 `user@host`：同一个已保存配置可以开出两个会话，
    /// 只看目录文字无法判断操作对象（filer.md §1.2）。
    var title: String {
        switch self {
        case .local:
            return String(localized: "Local")
        case .remote(_, let name, let userHost):
            guard let userHost, !userHost.isEmpty, userHost != name else { return name }
            return "\(name) · \(userHost)"
        }
    }
}

/// 排序（filer.md §4.1）：目录在前；每组按名称忽略大小写做字典序；相同时按原名称。
/// 数字当作普通文字比较（`a10` 排在 `a2` 前），**不是自然排序**——不要换成 `localizedStandardCompare`。
enum FilerSort {
    static func areInIncreasingOrder(_ a: FilerEntry, _ b: FilerEntry) -> Bool {
        if a.isDirectory != b.isDirectory { return a.isDirectory }
        let la = a.name.lowercased(), lb = b.name.lowercased()
        if la != lb { return la < lb }
        return a.name < b.name
    }

    static func sorted(_ entries: [FilerEntry]) -> [FilerEntry] {
        entries.sorted(by: areInIncreasingOrder)
    }
}

/// 底部统计（filer.md §4.2）：链接不在普通文件中重复计数。
struct FilerStats: Equatable, Sendable {
    var directories = 0
    var files = 0
    var links = 0

    init(directories: Int = 0, files: Int = 0, links: Int = 0) {
        self.directories = directories
        self.files = files
        self.links = links
    }

    init(_ entries: [FilerEntry]) {
        for e in entries {
            switch e.kind {
            case .directory: directories += 1
            case .file: files += 1
            case .link: links += 1
            }
        }
    }

    var total: Int { directories + files + links }

    var text: String {
        if total == 0 { return String(localized: "0 items") }
        var parts = [String(localized: "\(directories) folders"), String(localized: "\(files) files")]
        if links > 0 { parts.append(String(localized: "\(links) links")) }
        return parts.joined(separator: " · ")
    }
}

// MARK: - 路径

/// POSIX 风格路径运算（本地 macOS 与远端 SFTP 都用 `/`）。纯字符串运算，不访问文件系统，
/// 所以不会解析符号链接：`..` 是按文字回退的。
enum FilerPath {
    /// 折叠重复的 `/`、去掉 `.`、按文字解析 `..`；根目录保持 `/`，不会走到空路径。
    static func normalize(_ path: String) -> String {
        var parts: [Substring] = []
        for part in path.split(separator: "/", omittingEmptySubsequences: true) {
            if part == "." { continue }
            if part == ".." {
                if !parts.isEmpty { parts.removeLast() }
                continue
            }
            parts.append(part)
        }
        return "/" + parts.joined(separator: "/")
    }

    static func parent(of path: String) -> String {
        let n = normalize(path)
        guard n != "/", let slash = n.lastIndex(of: "/") else { return "/" }
        return slash == n.startIndex ? "/" : String(n[..<slash])
    }

    static func join(_ dir: String, _ name: String) -> String {
        dir.hasSuffix("/") ? dir + name : dir + "/" + name
    }

    static func lastComponent(_ path: String) -> String {
        let n = normalize(path)
        return n == "/" ? "/" : String(n.split(separator: "/").last ?? "/")
    }

    /// `child` 是否等于 `ancestor` 或在其之下（按分量比较，`/a/bc` 不在 `/a/b` 下）。
    static func isSameOrDescendant(_ child: String, of ancestor: String) -> Bool {
        let c = normalize(child), a = normalize(ancestor)
        if a == "/" { return true }
        return c == a || c.hasPrefix(a + "/")
    }

    /// 解析路径栏输入。空输入返回 nil；`~` 仅在传入 `home` 时展开（本地）；
    /// 相对路径以当前浏览目录为基准，而不是应用启动位置（filer.md §3）。
    static func resolve(_ input: String, base: String, home: String?) -> String? {
        let text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        if let home {
            if text == "~" { return normalize(home) }
            if text.hasPrefix("~/") { return normalize(join(home, String(text.dropFirst(2)))) }
        }
        return normalize(text.hasPrefix("/") ? text : join(base, text))
    }

    /// 面包屑分段：`/a/b` → [("/", "/"), ("a", "/a"), ("b", "/a/b")]。
    static func breadcrumbs(_ path: String) -> [(title: String, path: String)] {
        var result = [(title: "/", path: "/")]
        var current = ""
        for part in normalize(path).split(separator: "/") {
            current += "/" + part
            result.append((String(part), current))
        }
        return result
    }
}

/// 新建 / 重命名时的名称校验（filer.md §5.3）。名称必须是当前目录的一个直接子项。
enum FilerNameValidator {
    enum Failure: Error, Equatable, Sendable {
        case empty
        /// `.` / `..`：指向当前目录或上一级。
        case relativeName
        case hasSeparator
        case exists
    }

    /// 返回去掉首尾空白后的名称，或失败原因。`existing` 为当前目录已有的名称（区分大小写，
    /// 大小写折叠的文件系统由创建时的"已存在则失败"兜底，不在这里猜）。
    static func validate(_ raw: String, existing: Set<String>, allowing current: String? = nil) -> Result<String, Failure> {
        let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty { return .failure(.empty) }
        if name == "." || name == ".." { return .failure(.relativeName) }
        if name.contains("/") || name.contains("\0") { return .failure(.hasSeparator) }
        if existing.contains(name), name != current { return .failure(.exists) }
        return .success(name)
    }
}

// MARK: - 错误

/// 读取 / 操作失败的归类。分别说明，不能显示成空目录（filer.md §4.4）。
enum FilerFailureKind: Equatable, Sendable {
    case permissionDenied
    case notFound
    case notADirectory
    case alreadyExists
    case notEmpty
    /// 磁盘不可用、I/O 错误、卷已卸载。
    case diskUnavailable
    case disconnected
    case other(String)
}

struct FilerError: Error, Equatable, Sendable {
    var kind: FilerFailureKind
    /// 正在访问的路径。
    var path: String?

    init(_ kind: FilerFailureKind, path: String? = nil) {
        self.kind = kind
        self.path = path
    }

    var reason: String {
        switch kind {
        case .permissionDenied: String(localized: "Permission denied")
        case .notFound: String(localized: "Doesn’t exist")
        case .notADirectory: String(localized: "Not a folder")
        case .alreadyExists: String(localized: "Already exists")
        case .notEmpty: String(localized: "Only empty folders can be deleted")
        case .diskUnavailable: String(localized: "The disk is unavailable")
        case .disconnected: String(localized: "Connection closed")
        case .other(let detail): detail
        }
    }

    /// 完整说明：原因 + 路径 + 本地 / 远端身份。
    func message(source: FilerSource?) -> String {
        var text = reason
        if let path { text += " — " + path }
        if let source { text += " (\(source.title))" }
        return text
    }

    /// 根据 POSIX errno 归类（本地文件操作）。
    static func fromErrno(_ code: Int32, path: String?) -> FilerError {
        switch code {
        case EACCES, EPERM: return FilerError(.permissionDenied, path: path)
        case ENOENT: return FilerError(.notFound, path: path)
        case ENOTDIR: return FilerError(.notADirectory, path: path)
        case EEXIST: return FilerError(.alreadyExists, path: path)
        case ENOTEMPTY: return FilerError(.notEmpty, path: path)
        case EIO, ENXIO, ENODEV, ENOTCONN, ETIMEDOUT, EHOSTDOWN, ENETDOWN, ESTALE, EBUSY, EROFS:
            return FilerError(.diskUnavailable, path: path)
        default:
            return FilerError(.other(String(cString: strerror(code))), path: path)
        }
    }

    /// 根据 Foundation 错误归类：优先取底层 POSIX 错误码，其次看 Cocoa 错误码。
    static func from(_ error: Error, path: String?) -> FilerError {
        if let e = error as? FilerError { return e }
        let ns = error as NSError
        if ns.domain == NSPOSIXErrorDomain { return fromErrno(Int32(ns.code), path: path) }
        if let under = ns.userInfo[NSUnderlyingErrorKey] as? NSError, under.domain == NSPOSIXErrorDomain {
            return fromErrno(Int32(under.code), path: path)
        }
        if ns.domain == NSCocoaErrorDomain {
            switch ns.code {
            case NSFileReadNoPermissionError, NSFileWriteNoPermissionError: return FilerError(.permissionDenied, path: path)
            case NSFileNoSuchFileError, NSFileReadNoSuchFileError: return FilerError(.notFound, path: path)
            case NSFileWriteFileExistsError: return FilerError(.alreadyExists, path: path)
            default: break
            }
        }
        return FilerError(.other(ns.localizedDescription), path: path)
    }
}

// MARK: - 速率

/// 最近约 1 秒的传输速率（filer.md §6.2：不是从任务建立开始的终身平均值）。
/// 时间由调用方传入，测试不依赖真实时钟。
struct RateMeter: Sendable {
    private var samples: [(time: TimeInterval, bytes: UInt64)] = []
    let window: TimeInterval

    init(window: TimeInterval = 1) { self.window = window }

    mutating func record(bytes: UInt64, at time: TimeInterval) {
        samples.append((time, bytes))
        // 保留窗口内的样本，外加窗口之前最近的一个，作为速率的起点。
        while samples.count > 2, samples[1].time <= time - window { samples.removeFirst() }
    }

    /// 窗口内样本不足两个或时间没有前进时为 nil（界面不显示速率，而不是显示 0）。
    var bytesPerSecond: Double? {
        guard let first = samples.first, let last = samples.last, last.time > first.time, last.bytes >= first.bytes else {
            return nil
        }
        return Double(last.bytes - first.bytes) / (last.time - first.time)
    }
}
