import AppKit
import KansoBridge
import KansoUI

enum GitDiffPresentation {
    static func changedRanges(old: String, new: String) -> (old: NSRange, new: NSRange) {
        let oldCharacters = Array(old)
        let newCharacters = Array(new)
        let sharedCount = min(oldCharacters.count, newCharacters.count)
        var prefix = 0
        while prefix < sharedCount && oldCharacters[prefix] == newCharacters[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < sharedCount - prefix
            && oldCharacters[oldCharacters.count - suffix - 1] == newCharacters[newCharacters.count - suffix - 1] {
            suffix += 1
        }
        func range(_ characters: [Character]) -> NSRange {
            let start = characters.prefix(prefix).reduce(0) { $0 + $1.utf16.count }
            let length = characters[prefix..<(characters.count - suffix)].reduce(0) { $0 + $1.utf16.count }
            return NSRange(location: start, length: length)
        }
        return (range(oldCharacters), range(newCharacters))
    }

    static func highlights(_ lines: [FfiGitLine]) -> [NSRange?] {
        var result = [NSRange?](repeating: nil, count: lines.count)
        var index = 0
        while index < lines.count {
            guard lines[index].kind == "-" else { index += 1; continue }
            let removedStart = index
            while index < lines.count && lines[index].kind == "-" { index += 1 }
            let addedStart = index
            while index < lines.count && lines[index].kind == "+" { index += 1 }
            for offset in 0..<min(addedStart - removedStart, index - addedStart) {
                let ranges = changedRanges(old: lines[removedStart + offset].text, new: lines[addedStart + offset].text)
                result[removedStart + offset] = ranges.old
                result[addedStart + offset] = ranges.new
            }
        }
        return result
    }

    static func gaps(_ lines: [FfiGitLine]) -> [Int] {
        var nextNewNumber = 1
        var delta = 0
        return lines.map { line in
            let number = Int(line.number)
            let newNumber = line.kind == "-" ? number + delta : number
            let gap = max(0, newNumber - nextNewNumber)
            nextNewNumber = newNumber + (line.kind == "-" ? 0 : 1)
            if line.kind == "+" { delta += 1 }
            if line.kind == "-" { delta -= 1 }
            return gap
        }
    }
}

private let diffKind = NSAttributedString.Key("GitDiffKind")

@MainActor
private final class GitDiffLayoutManager: NSLayoutManager {
    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: NSPoint) {
        guard let textStorage, let textView = textContainers.first?.textView else { return }
        let width = textView.bounds.width
        enumerateLineFragments(forGlyphRange: glyphsToShow) { rect, _, _, range, _ in
            let character = self.characterIndexForGlyph(at: range.location)
            let kind = textStorage.attribute(diffKind, at: character, effectiveRange: nil) as? String
            let background: String
            switch kind {
            case "+": background = DesignTokens.Color.diffAddedBackground
            case "-": background = DesignTokens.Color.diffRemovedBackground
            case "gap": background = DesignTokens.Color.bgStatusBar
            default: return
            }
            NSColor.token(background).setFill()
            NSRect(x: 0, y: origin.y + rect.minY, width: width, height: rect.height).fill()
            if kind == "+" || kind == "-" {
                NSColor.token(kind == "+" ? DesignTokens.Color.diffAdded : DesignTokens.Color.diffRemoved).setFill()
                NSRect(x: 0, y: origin.y + rect.minY, width: 3, height: rect.height).fill()
            }
        }
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
    }
}

@MainActor
final class GitDiffTextView: NSTextView {
    let contentSize: NSSize

    init(lines: [FfiGitLine], display: DisplaySettings) {
        let font = TerminalFontResolver.font(family: display.fontFamily, size: max(11, display.fontSize - 1)).font
        let lineHeight = ceil(font.pointSize * 1.4)
        let gutter = ceil((String(lines.map(\.number).max() ?? 0) as NSString).size(withAttributes: [.font: font]).width) + 16
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = lineHeight
        paragraph.maximumLineHeight = lineHeight
        paragraph.tabStops = [NSTextTab(textAlignment: .right, location: gutter), NSTextTab(textAlignment: .left, location: gutter + 12)]
        let content = NSMutableAttributedString()
        let gaps = GitDiffPresentation.gaps(lines)
        let highlights = GitDiffPresentation.highlights(lines)
        for (index, line) in lines.enumerated() {
            if gaps[index] > 0 {
                content.append(NSAttributedString(string: "\t\t" + String(localized: "\(gaps[index]) unchanged lines") + "\n", attributes: [diffKind: "gap", .foregroundColor: NSColor.token(DesignTokens.Color.textSecondary)]))
            }
            let prefix = "\t\(line.number)\t"
            let start = content.length
            content.append(NSAttributedString(string: prefix + line.text + "\n", attributes: [diffKind: line.kind, .foregroundColor: NSColor.token(DesignTokens.Color.textPrimary)]))
            content.addAttribute(.foregroundColor, value: NSColor.token(DesignTokens.Color.textSecondary), range: NSRange(location: start, length: prefix.utf16.count))
            if let highlight = highlights[index], highlight.length > 0 {
                content.addAttribute(.backgroundColor,
                                     value: NSColor.token(line.kind == "+" ? DesignTokens.Color.diffAddedHighlight : DesignTokens.Color.diffRemovedHighlight),
                                     range: NSRange(location: start + prefix.utf16.count + highlight.location, length: highlight.length))
            }
        }
        content.addAttributes([.font: font, .paragraphStyle: paragraph], range: NSRange(location: 0, length: content.length))
        if content.length > 0 { content.deleteCharacters(in: NSRange(location: content.length - 1, length: 1)) }
        let storage = NSTextStorage(attributedString: content)
        let manager = GitDiffLayoutManager()
        let container = NSTextContainer(containerSize: NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = false
        container.lineFragmentPadding = 0
        storage.addLayoutManager(manager)
        manager.addTextContainer(container)
        manager.ensureLayout(for: container)
        let used = manager.usedRect(for: container)
        contentSize = NSSize(width: ceil(used.maxX) + 12, height: ceil(used.maxY))
        super.init(frame: NSRect(origin: .zero, size: contentSize), textContainer: container)
        isEditable = false
        isSelectable = true
        drawsBackground = false
        textContainerInset = .zero
        isHorizontallyResizable = true
        isVerticallyResizable = false
        setAccessibilityLabel(String(localized: "Changes"))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("GitDiffTextView 只支持代码创建") }
}

@MainActor
final class GitDiffScrollView: NSScrollView {
    override func scrollWheel(with event: NSEvent) {
        if abs(event.scrollingDeltaY) > abs(event.scrollingDeltaX), !event.modifierFlags.contains(.shift) {
            superview?.enclosingScrollView?.scrollWheel(with: event)
        } else {
            super.scrollWheel(with: event)
        }
    }
}
