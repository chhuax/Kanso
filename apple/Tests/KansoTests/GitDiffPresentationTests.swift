import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

final class GitDiffPresentationTests: XCTestCase {
    func testChangedRanges() {
        let cases: [(String, String, NSRange, NSRange)] = [
            ("", "", NSRange(location: 0, length: 0), NSRange(location: 0, length: 0)),
            ("", "new", NSRange(location: 0, length: 0), NSRange(location: 0, length: 3)),
            ("old", "", NSRange(location: 0, length: 3), NSRange(location: 0, length: 0)),
            ("abc", "xyz", NSRange(location: 0, length: 3), NSRange(location: 0, length: 3)),
            ("same", "same", NSRange(location: 4, length: 0), NSRange(location: 4, length: 0)),
            ("aab", "ab", NSRange(location: 1, length: 1), NSRange(location: 1, length: 0)),
            ("let title = old()", "let title = new()", NSRange(location: 12, length: 3), NSRange(location: 12, length: 3)),
            ("你好旧内容。", "你好新内容。", NSRange(location: 2, length: 1), NSRange(location: 2, length: 1)),
            ("😀旧👩‍💻", "😀新👩‍💻", NSRange(location: 2, length: 1), NSRange(location: 2, length: 1)),
        ]
        for (old, new, oldRange, newRange) in cases {
            let result = GitDiffPresentation.changedRanges(old: old, new: new)
            XCTAssertEqual(result.old, oldRange, old)
            XCTAssertEqual(result.new, newRange, new)
        }
    }

    func testPairsDeletionAndAdditionRunsInOrder() {
        let lines = [line("-", 1, "old first"), line("-", 2, "old second"), line("+", 1, "new first"),
                     line("+", 2, "new second"), line("+", 3, "unpaired"), line(" ", 4), line("-", 4, "only removed")]
        let ranges = GitDiffPresentation.highlights(lines)
        XCTAssertEqual(ranges[0], NSRange(location: 0, length: 3))
        XCTAssertEqual(ranges[1], NSRange(location: 0, length: 3))
        XCTAssertEqual(ranges[2], NSRange(location: 0, length: 3))
        XCTAssertEqual(ranges[3], NSRange(location: 0, length: 3))
        XCTAssertNil(ranges[4])
        XCTAssertNil(ranges[5])
        XCTAssertNil(ranges[6])
    }

    func testGapsUseNewLineNumbersAndTrackDeletionOffsets() {
        XCTAssertEqual(GitDiffPresentation.gaps([]), [])
        XCTAssertEqual(GitDiffPresentation.gaps([line(" ", 1), line(" ", 2)]), [0, 0])
        XCTAssertEqual(GitDiffPresentation.gaps([line(" ", 8), line(" ", 9), line(" ", 20)]), [7, 0, 10])
        XCTAssertEqual(GitDiffPresentation.gaps([
            line(" ", 1), line("-", 2), line("-", 3), line("+", 2), line(" ", 3),
            line("-", 10), line("+", 9), line(" ", 10)
        ]), [0, 0, 0, 0, 0, 5, 0, 0])
        XCTAssertEqual(GitDiffPresentation.gaps([
            line("+", 1), line("+", 2), line(" ", 3), line("-", 8), line("-", 9), line(" ", 10)
        ]), [0, 0, 0, 6, 0, 0])
        XCTAssertEqual(GitDiffPresentation.gaps([line("-", 1), line("-", 2), line("-", 3)]), [0, 0, 0])
        XCTAssertEqual(GitDiffPresentation.gaps(GitFixtures.diff.lines), [37, 0, 0, 0, 0, 16, 0, 0, 0])
    }

    private func line(_ kind: String, _ number: UInt64, _ text: String = "") -> FfiGitLine {
        FfiGitLine(kind: kind, number: number, text: text)
    }
}

@MainActor
final class GitDiffViewTests: XCTestCase {
    func testUsesTerminalFontAndMinimumSize() {
        var display = DisplaySettings()
        display.fontFamily = "Menlo"
        display.fontSize = 18
        let text = GitDiffTextView(lines: GitFixtures.diff.lines, display: display)
        let font = text.textStorage?.attribute(.font, at: 0, effectiveRange: nil) as? NSFont
        XCTAssertEqual(font, TerminalFontResolver.font(family: "Menlo", size: 17).font)
        let paragraph = text.textStorage?.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        XCTAssertEqual(paragraph?.minimumLineHeight, 24)
        display.fontSize = 9
        let small = GitDiffTextView(lines: GitFixtures.diff.lines, display: display)
        XCTAssertEqual((small.textStorage?.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)?.pointSize, 11)
    }

    func testThreeThousandLinesUseOneInlineTextView() {
        let lines = (1...3000).map { FfiGitLine(kind: "+", number: UInt64($0), text: String(repeating: "long line ", count: 30)) }
        let row = GitFileRow(state: .init(file: GitFixtures.files[0], expanded: true,
                                         diff: FfiGitDiff(lines: lines, notice: nil, truncated: true), error: nil, display: DisplaySettings()))
        row.frame = NSRect(x: 0, y: 0, width: 400, height: row.height)
        row.layoutSubtreeIfNeeded()
        let scroll = row.subviews.compactMap { $0 as? GitDiffScrollView }.first!
        let text = scroll.documentView as! GitDiffTextView
        XCTAssertGreaterThan(row.height, 3000 * 18)
        XCTAssertGreaterThan(text.frame.width, scroll.contentSize.width)
        XCTAssertEqual(scroll.contentSize.height, text.frame.height)
        XCTAssertFalse(scroll.hasVerticalScroller)
        XCTAssertTrue(scroll.hasHorizontalScroller)
        XCTAssertEqual(text.subviews.count, 0)
        XCTAssertEqual(text.string.components(separatedBy: "\n").count, 3000)
    }

    func testHeaderCopyAndTogglePreserveOtherCards() async {
        let model = GitPanelModel(engine: FakeGitEngine())
        let settings = SettingsStore()
        var copied: [String] = []
        let panel = GitPanelView(model: model, settings: settings, copyPath: { copied.append($0); return true })
        panel.frame = NSRect(x: 0, y: 0, width: 420, height: 620)
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        let document = panel.subviews.compactMap { $0 as? NSScrollView }.first!.documentView!
        let rows = document.subviews.compactMap { $0 as? GitFileRow }
        let untouched = rows[1]
        let first = rows[0]
        let copy = first.subviews.compactMap { $0 as? KButton }.first { $0.toolTip == String(localized: "Copy Full Path") }!
        copy.onPress?()
        XCTAssertEqual(copied, ["/fixture/project/" + GitFixtures.files[0].path])
        first.onToggle?()
        await model.waitUntilIdle()
        XCTAssertTrue(document.subviews.contains { $0 === untouched })
        panel.layoutSubtreeIfNeeded()
        let expanded = document.subviews.compactMap { $0 as? GitFileRow }.first { $0.state.expanded }!
        let badge = expanded.subviews.compactMap { $0 as? StyledBoxView }.first { $0.fillToken == DesignTokens.Color.diffBadgeBackground }!
        let counts = badge.subviews.first as! NSTextField
        XCTAssertEqual(counts.stringValue, "+18 · −5")
        XCTAssertGreaterThanOrEqual(counts.bounds.width, counts.attributedStringValue.size().width)
        let hit = expanded.hitTest(NSPoint(x: expanded.frame.maxX - 65, y: expanded.frame.minY + 20))
        XCTAssertTrue(hit === expanded)
        settings.updateDisplay { $0.fontSize = 20 }
        XCTAssertEqual(document.subviews.compactMap { $0 as? GitFileRow }.first?.state.display.fontSize, 20)
    }

    func testCopyFailureUsesExistingErrorNotice() async {
        let model = GitPanelModel(engine: FakeGitEngine())
        model.follow(GitFixtures.tab())
        await model.waitUntilIdle()
        model.copyPath(GitFixtures.files[0].path, using: { _ in false })
        XCTAssertEqual(model.error, String(localized: "Couldn’t copy to the clipboard."))
    }
}
