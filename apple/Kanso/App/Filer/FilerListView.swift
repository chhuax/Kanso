import AppKit
import KansoUI

/// 文件图标分类（filer.md §4.2）：按名称 / 扩展名归类，首版用 SF Symbols，不引入第三方图标素材。
enum FilerIconCategory: CaseIterable {
    case folder, link, document, code, image, archive, audio, video, config, executable, generic

    /// 扩展名（小写）→ 类别。更具体的多段扩展名（`tar.gz`）优先于最后一段。
    static func category(for entry: FilerEntry) -> FilerIconCategory {
        if entry.isSymlink { return .link }
        if entry.isDirectory { return .folder }
        let lower = entry.name.lowercased()
        for multi in ["tar.gz", "tar.bz2", "tar.xz", "tar.zst"] where lower.hasSuffix("." + multi) { return .archive }
        let ext = (lower as NSString).pathExtension
        switch ext {
        case "swift", "rs", "c", "h", "cpp", "hpp", "m", "mm", "go", "py", "js", "ts", "tsx", "jsx", "java", "kt", "rb", "php", "sh", "zsh", "bash", "lua", "cs", "html", "css", "scss", "sql":
            return .code
        case "png", "jpg", "jpeg", "gif", "bmp", "tiff", "webp", "heic", "svg", "ico":
            return .image
        case "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst", "tgz", "dmg", "pkg":
            return .archive
        case "mp3", "wav", "flac", "aac", "m4a", "ogg":
            return .audio
        case "mp4", "mov", "mkv", "avi", "webm", "m4v":
            return .video
        case "json", "yaml", "yml", "toml", "ini", "conf", "cfg", "plist", "xml", "env", "lock":
            return .config
        case "txt", "md", "pdf", "doc", "docx", "rtf", "csv", "log", "xls", "xlsx", "ppt", "pptx", "key", "pages":
            return .document
        default:
            if lower.hasPrefix(".") { return .config }
            if let p = entry.permissions, p & 0o111 != 0, ext.isEmpty { return .executable }
            return .generic
        }
    }

    var symbol: String {
        switch self {
        case .folder: "folder.fill"
        case .link: "link"
        case .document: "doc.text"
        case .code: "chevron.left.forwardslash.chevron.right"
        case .image: "photo"
        case .archive: "archivebox"
        case .audio: "waveform"
        case .video: "film"
        case .config: "gearshape"
        case .executable: "terminal"
        case .generic: "doc"
        }
    }

    /// 浅色 / 深色外观下都清晰的系统色（随外观自动解析）。
    var tint: NSColor {
        switch self {
        case .folder: .systemTeal
        case .link: .systemPurple
        case .document: .secondaryLabelColor
        case .code: .systemBlue
        case .image: .systemPink
        case .archive: .systemOrange
        case .audio: .systemIndigo
        case .video: .systemRed
        case .config: .systemGray
        case .executable: .systemGreen
        case .generic: .tertiaryLabelColor
        }
    }
}

/// 图标按类别缓存：同类图标不随每一行重建（旧版大目录曾因此明显停顿，filer.md §4.2）。
@MainActor
enum FilerIconCache {
    private static var cache: [FilerIconCategory: NSImage] = [:]
    /// 测试用：真正创建图标的次数。
    private(set) static var creations = 0

    static func image(for category: FilerIconCategory) -> NSImage {
        if let cached = cache[category] { return cached }
        creations += 1
        let image = NSImage.symbol(category.symbol)
            .withSymbolConfiguration(.init(pointSize: 13, weight: .regular)) ?? NSImage()
        cache[category] = image
        return image
    }
}

/// 日期格式（年 / 月 / 日，本地时区；filer.md §4.1）。未知显示破折号。
enum FilerFormat {
    static func date(_ date: Date?, timeZone: TimeZone = .current) -> String {
        guard let date else { return "—" }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let c = cal.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d/%02d/%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
    }

    /// 悬停说明：完整路径、类型、大小、八进制权限、属主属组；缺失的属性不编造。
    static func tooltip(for e: FilerEntry) -> String {
        var lines = [e.path]
        switch e.kind {
        case .directory: lines.append(String(localized: "Folder"))
        case .link: lines.append(String(localized: "Link"))
        case .file:
            var line = String(localized: "File")
            if let size = e.size { line += " · " + Int64(clamping: size).formatted(ByteCountFormatStyle(style: .file)) }
            lines.append(line)
        }
        if let p = e.permissions { lines.append(String(localized: "Permissions: ") + String(p & 0o7777, radix: 8)) }
        if e.owner != nil || e.group != nil {
            lines.append(String(localized: "Owner: ") + [e.owner, e.group].compactMap { $0 }.joined(separator: ":"))
        }
        return lines.joined(separator: "\n")
    }
}

@MainActor
final class FilerCellView: NSView {
    private let icon = NSImageView()
    private let nameLabel = NSTextField(labelWithString: "")
    private let dateLabel = NSTextField(labelWithString: "")
    private let sizeLabel = NSTextField(labelWithString: "")
    private var dual = false
    static let iconSize: CGFloat = 16
    static let dateWidth: CGFloat = 78
    static let pad: CGFloat = 12
    static let sizeWidth: CGFloat = 68
    static func dateWidth(dual: Bool) -> CGFloat { dual ? 132 : dateWidth }

    override init(frame: NSRect) {
        super.init(frame: frame)
        icon.imageScaling = .scaleProportionallyDown
        nameLabel.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
        nameLabel.lineBreakMode = .byTruncatingMiddle
        dateLabel.font = .monospacedDigitSystemFont(ofSize: DesignTokens.Font.uiCaption, weight: .regular)
        dateLabel.textColor = .token(DesignTokens.Color.textTertiary)
        dateLabel.alignment = .right
        sizeLabel.font = dateLabel.font
        sizeLabel.textColor = dateLabel.textColor
        sizeLabel.alignment = .right
        for v in [icon, nameLabel, dateLabel, sizeLabel] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("FilerCellView 只支持代码创建") }

    override var isFlipped: Bool { true }

    func configure(_ e: FilerEntry, dual: Bool = false) {
        self.dual = dual
        let category = FilerIconCategory.category(for: e)
        icon.image = FilerIconCache.image(for: category)
        icon.contentTintColor = category.tint
        nameLabel.stringValue = e.name
        // 链接用不同的名称颜色辨认（图标之外的第二个线索）。
        nameLabel.textColor = .token(e.isSymlink ? DesignTokens.Color.accent : DesignTokens.Color.textPrimary)
        dateLabel.stringValue = dual ? (e.modified?.formatted(date: .numeric, time: .shortened) ?? "—") : FilerFormat.date(e.modified)
        sizeLabel.isHidden = !dual
        sizeLabel.stringValue = e.isDirectory ? "—" : (e.size.map { Int64(clamping: $0).formatted(ByteCountFormatStyle(style: .file)) } ?? "—")
        toolTip = FilerFormat.tooltip(for: e)
        needsLayout = true
    }

    var nameText: String { nameLabel.stringValue }
    var dateText: String { dateLabel.stringValue }

    override func layout() {
        super.layout()
        let h = bounds.height
        icon.frame = NSRect(x: Self.pad, y: (h - Self.iconSize) / 2, width: Self.iconSize, height: Self.iconSize)
        let dateWidth = Self.dateWidth(dual: dual)
        let dateX = bounds.width - Self.pad - dateWidth
        dateLabel.frame = NSRect(x: dateX, y: (h - 14) / 2, width: dateWidth, height: 14)
        let sizeX = dateX - Self.sizeWidth - 8
        sizeLabel.frame = NSRect(x: sizeX, y: (h - 14) / 2, width: Self.sizeWidth, height: 14)
        let nameX = icon.frame.maxX + 8
        nameLabel.frame = NSRect(x: nameX, y: (h - 16) / 2, width: max(0, (dual ? sizeX : dateX) - nameX - 6), height: 16)
    }
}

@MainActor
final class FilerRowView: NSTableRowView {
    override func drawSelection(in dirtyRect: NSRect) {
        NSColor.token(DesignTokens.Color.bgTabSelected).setFill()
        let r = bounds.insetBy(dx: 4, dy: 1)
        NSBezierPath(roundedRect: r, xRadius: DesignTokens.Radius.sm, yRadius: DesignTokens.Radius.sm).fill()
    }
    override var isEmphasized: Bool { get { false } set {} }
}

/// 文件列表。选择交给 NSTableView（⌘ 点选、⇧ 范围、方向键都是系统行为）；
/// 回车 / ⌘⌫ / F2 与右键菜单在这里转成回调。
@MainActor
final class FilerTableView: NSTableView {
    var onReturn: (() -> Void)?
    var onDelete: (() -> Void)?
    var onRename: (() -> Void)?
    /// 右键：row 为 -1 表示空白处。返回要弹出的菜单。
    var menuProvider: ((Int) -> NSMenu?)?

    override func keyDown(with event: NSEvent) {
        let command = event.modifierFlags.contains(.command)
        switch event.keyCode {
        case 36 where !command, 76 where !command: onReturn?()
        case 51 where command: onDelete?()
        case 120: onRename?()
        default: super.keyDown(with: event)
        }
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = self.row(at: convert(event.locationInWindow, from: nil))
        // 右键未选中的行：先只选中它（与访达一致）；已在选中集合里则保持多选。
        if row >= 0, !selectedRowIndexes.contains(row) { selectRowIndexes([row], byExtendingSelection: false) }
        if row < 0 { deselectAll(nil) }
        return menuProvider?(row)
    }
}
