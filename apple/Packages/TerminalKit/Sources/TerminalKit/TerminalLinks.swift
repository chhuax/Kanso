import Foundation
import KansoBridge

/// 链接在终端网格上覆盖的单行区段（`endCol` 不含）。
public struct TerminalLinkSegment: Equatable, Sendable {
    public let row: Int
    public let startCol: Int
    public let endCol: Int

    public init(row: Int, startCol: Int, endCol: Int) {
        self.row = row
        self.startCol = startCol
        self.endCol = endCol
    }
}

/// 命中的终端链接（OSC 8 超链接或自动识别的 URL）。
public struct TerminalLinkHit: Equatable, Sendable {
    public let url: String
    public let segments: [TerminalLinkSegment]

    public init(url: String, segments: [TerminalLinkSegment]) {
        self.url = url
        self.segments = segments
    }

    /// 某个单元格是否在链接内。
    public func contains(row: Int, col: Int) -> Bool {
        segments.contains { $0.row == row && $0.startCol <= col && col < $0.endCol }
    }
}

/// 链接识别与安全校验（terminal.md §10）。
public enum TerminalLinks {
    /// 允许交给系统打开的协议白名单（小写）。
    private static let openableSchemes: Set<String> = ["http", "https", "ftp", "ftps", "mailto"]
    /// 自动从屏幕文字中识别的协议（只认明确的 `scheme://`）。
    private static let plainSchemes: [[Unicode.Scalar]] = [
        Array("http".unicodeScalars),
        Array("https".unicodeScalars),
        Array("ftp".unicodeScalars),
        Array("ftps".unicodeScalars),
    ]

    /// 校验地址是否可安全交给系统打开，并返回解析后的 `URL`。
    public static func openableURL(from raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let colon = trimmed.firstIndex(of: ":") else { return nil }
        let scheme = trimmed[..<colon].lowercased()
        let rest = trimmed[trimmed.index(after: colon)...]
        guard !rest.isEmpty, openableSchemes.contains(scheme),
              let url = URL(string: trimmed),
              let parsedScheme = url.scheme?.lowercased(),
              openableSchemes.contains(parsedScheme) else { return nil }
        return url
    }

    public static func isOpenable(_ raw: String) -> Bool {
        openableURL(from: raw) != nil
    }

    /// 以 `(row, col)` 为指针位置查找链接：先查 OSC 8 超链接，再在软换行逻辑行内识别普通 URL。
    public static func link(
        in frame: FrameSnapshot, row: Int, col: Int, resolveHyperlink: (UInt32) -> String?
    ) -> TerminalLinkHit? {
        guard row >= 0, row < frame.rows, col >= 0, col < frame.cols else { return nil }

        var first = row
        while first > 0 && frame.lineMeta(row: first - 1).flags.contains(.wrapped) {
            first -= 1
        }
        var last = row
        while last + 1 < frame.rows && frame.lineMeta(row: last).flags.contains(.wrapped) {
            last += 1
        }

        if let id = hyperlinkId(in: frame, row: row, col: col),
           let raw = resolveHyperlink(id), !raw.isEmpty {
            return osc8Hit(in: frame, id: id, url: raw, rows: first...last, targetRow: row, targetCol: col)
        }

        return plainURLHit(in: frame, rows: first...last, targetRow: row, targetCol: col)
    }

    // MARK: - OSC 8 超链接

    private static func hyperlinkId(in frame: FrameSnapshot, row: Int, col: Int) -> UInt32? {
        let cell = frame.cell(row: row, col: col)
        if let id = frame.hyperlinkId(for: cell) { return id }
        if cell.flags.contains(.wideSpacer), col > 0 {
            return frame.hyperlinkId(for: frame.cell(row: row, col: col - 1))
        }
        return nil
    }

    private static func osc8Hit(
        in frame: FrameSnapshot, id: UInt32, url: String, rows: ClosedRange<Int>, targetRow: Int, targetCol: Int
    ) -> TerminalLinkHit {
        let cols = frame.cols
        let count = rows.count * cols
        let origin = rows.lowerBound
        let targetIndex = (targetRow - origin) * cols + targetCol

        var start = targetIndex
        while start > 0 {
            let prev = start - 1
            let r = origin + prev / cols
            let c = prev % cols
            if hyperlinkId(in: frame, row: r, col: c) == id {
                start = prev
            } else {
                break
            }
        }

        var end = targetIndex + 1
        while end < count {
            let r = origin + end / cols
            let c = end % cols
            if hyperlinkId(in: frame, row: r, col: c) == id {
                end += 1
            } else {
                break
            }
        }

        var segments: [TerminalLinkSegment] = []
        for idx in start..<end {
            let r = origin + idx / cols
            let c = idx % cols
            appendCell(row: r, col: c, width: 1, to: &segments)
        }
        return TerminalLinkHit(url: url, segments: segments)
    }

    // MARK: - 普通 URL 扫描

    private static func plainURLHit(
        in frame: FrameSnapshot, rows: ClosedRange<Int>, targetRow: Int, targetCol: Int
    ) -> TerminalLinkHit? {
        var chars: [Character] = []
        var positions: [(row: Int, col: Int, width: Int)] = []
        let cols = frame.cols
        chars.reserveCapacity(rows.count * cols)
        positions.reserveCapacity(rows.count * cols)

        for r in rows {
            for c in 0..<cols {
                let cell = frame.cell(row: r, col: c)
                if cell.flags.contains(.wideSpacer) {
                    if let lastIdx = positions.indices.last, positions[lastIdx].row == r {
                        positions[lastIdx].width += 1
                    }
                    continue
                }
                let ch = visibleChar(in: frame, cell: cell)
                chars.append(ch)
                positions.append((row: r, col: c, width: 1))
            }
        }

        guard let hitIndex = positions.firstIndex(where: {
            $0.row == targetRow && $0.col <= targetCol && targetCol < $0.col + $0.width
        }) else {
            return nil
        }

        guard let range = scanPlainURLs(chars).first(where: { $0.0 <= hitIndex && hitIndex < $0.1 }) else {
            return nil
        }

        var segments: [TerminalLinkSegment] = []
        for pos in positions[range.0..<range.1] {
            appendCell(row: pos.row, col: pos.col, width: pos.width, to: &segments)
        }
        return TerminalLinkHit(url: String(chars[range.0..<range.1]), segments: segments)
    }

    private static func visibleChar(in frame: FrameSnapshot, cell: FrameCell) -> Character {
        if cell.extra != 0, let first = frame.grapheme(for: cell)?.first {
            return isControl(first) ? " " : first
        }
        guard cell.ch != 0, let scalar = UnicodeScalar(cell.ch) else { return " " }
        let ch = Character(scalar)
        return isControl(ch) ? " " : ch
    }

    private static func isControl(_ ch: Character) -> Bool {
        ch.unicodeScalars.count == 1 && (ch.unicodeScalars.first!.value < 0x20 || ch.unicodeScalars.first!.value == 0x7F)
    }

    private static func appendCell(row: Int, col: Int, width: Int, to segments: inout [TerminalLinkSegment]) {
        if let last = segments.last, last.row == row, last.endCol == col {
            segments[segments.count - 1] = TerminalLinkSegment(row: row, startCol: last.startCol, endCol: col + width)
        } else {
            segments.append(TerminalLinkSegment(row: row, startCol: col, endCol: col + width))
        }
    }

    static func scanPlainURLs(_ chars: [Character]) -> [(Int, Int)] {
        var out: [(Int, Int)] = []
        var i = 0
        while i < chars.count {
            let boundary = i == 0 || !isASCIIAlphanumeric(chars[i - 1])
            guard boundary, let body = schemeAt(chars, index: i) else {
                i += 1
                continue
            }
            var end = body
            while end < chars.count && isURLChar(chars[end]) {
                end += 1
            }
            while end > body {
                let c = chars[end - 1]
                let trim: Bool
                switch c {
                case ".", ",", ";", ":", "!", "?":
                    trim = true
                case ")", "]", "}":
                    let open = opener(for: c)
                    let opens = chars[i..<end].count { $0 == open }
                    let closes = chars[i..<end].count { $0 == c }
                    trim = closes > opens
                default:
                    trim = false
                }
                if !trim { break }
                end -= 1
            }
            if end > body {
                out.append((i, end))
                i = end
            } else {
                i = body
            }
        }
        return out
    }

    private static func schemeAt(_ chars: [Character], index i: Int) -> Int? {
        for scheme in plainSchemes {
            let end = i + scheme.count
            guard end + 3 <= chars.count else { continue }
            var matched = true
            for (offset, target) in scheme.enumerated() {
                let scalars = chars[i + offset].unicodeScalars
                guard scalars.count == 1, asciiLower(scalars.first!) == target else {
                    matched = false
                    break
                }
            }
            if matched && chars[end] == ":" && chars[end + 1] == "/" && chars[end + 2] == "/" {
                return end + 3
            }
        }
        return nil
    }

    private static func isASCIIAlphanumeric(_ ch: Character) -> Bool {
        guard ch.unicodeScalars.count == 1 else { return false }
        let v = ch.unicodeScalars.first!.value
        return (0x30...0x39).contains(v) || (0x41...0x5A).contains(v) || (0x61...0x7A).contains(v)
    }

    private static func asciiLower(_ scalar: Unicode.Scalar) -> Unicode.Scalar {
        let v = scalar.value
        if (0x41...0x5A).contains(v) {
            return Unicode.Scalar(v + 32)!
        }
        return scalar
    }

    private static func isURLChar(_ ch: Character) -> Bool {
        guard ch.unicodeScalars.count == 1 else { return false }
        let v = ch.unicodeScalars.first!.value
        guard (0x21...0x7E).contains(v) else { return false }
        switch ch {
        case "<", ">", "\"", "'", "`", "\\": return false
        default: return true
        }
    }

    private static func opener(for close: Character) -> Character {
        switch close {
        case ")": "("
        case "]": "["
        default: "{"
        }
    }
}
