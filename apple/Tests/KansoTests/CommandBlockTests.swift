import AppKit
import CKansoFFI
import KansoBridge
import XCTest
@testable import TerminalKit

@MainActor
final class CommandBlockTests: XCTestCase {
    private func frame(flags: FrameFlags = [], rows: UInt16 = 12) throws -> FrameSnapshot {
        let handle = FrameHandle.mockCommandBlocks(cols: 80, rows: rows)
        let header = try XCTUnwrap(UnsafeMutablePointer<KansoFrameHeader>(bitPattern: UInt(handle.headerAddress())))
        header.pointee.flags |= flags.rawValue
        return try XCTUnwrap(FrameSnapshot(coreFrame: handle))
    }

    private func point(_ row: Int, in view: TerminalView) throws -> NSPoint {
        let geometry = try XCTUnwrap(view.hitGeometry())
        return NSPoint(x: geometry.originX + geometry.cellWidth,
                       y: view.bounds.height - geometry.originY - (Double(row) + 0.5) * geometry.cellHeight)
    }

    private func click(_ row: Int, in view: TerminalView, count: Int = 1) throws {
        let location = try point(row, in: view)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: location, clickCount: count))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: location, clickCount: count))
    }

    func testSingleClickTogglesAndOutsideOrEmptyBlockClears() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        view.currentFrame = try frame()
        try click(1, in: view)
        XCTAssertEqual(view.selectedCommandBlock, 1)
        try click(2, in: view)
        XCTAssertNil(view.selectedCommandBlock)
        try click(0, in: view)
        try click(4, in: view)
        XCTAssertEqual(view.selectedCommandBlock, 2)
        try click(6, in: view)
        XCTAssertNil(view.selectedCommandBlock)
        try click(4, in: view)
        try click(10, in: view)
        XCTAssertNil(view.selectedCommandBlock)
        try click(4, in: view)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: NSPoint(x: -5, y: 10)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: NSPoint(x: -5, y: 10)))
        XCTAssertNil(view.selectedCommandBlock)
    }

    func testDragAndMultipleClicksDoNotSelectBlock() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        view.currentFrame = try frame()
        try click(1, in: view)
        let location = try point(1, in: view)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: location))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: try point(2, in: view)))
        XCTAssertNil(view.selectedCommandBlock)
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: location))
        XCTAssertNil(view.selectedCommandBlock)
        for count in [2, 3] {
            try click(1, in: view)
            try click(1, in: view, count: count)
            XCTAssertNil(view.selectedCommandBlock)
        }
    }

    func testShellKeyAndCommittedTextClearBlockSelection() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        view.inputSink = sink
        view.inputMethod = FakeInputMethod()
        view.currentFrame = try frame()
        try click(1, in: view)
        view.keyDown(with: InputFixtures.key(.keyDown, VK.a, characters: "a"))
        XCTAssertNil(view.selectedCommandBlock)
        XCTAssertEqual(sink.keys.count, 1)
        try click(1, in: view)
        view.insertText("你好", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertNil(view.selectedCommandBlock)
        XCTAssertEqual(sink.texts, ["你好"])
        try click(1, in: view)
        view.setMarkedText("输入", selectedRange: NSRange(location: 2, length: 0),
                           replacementRange: NSRange(location: NSNotFound, length: 0))
        view.unmarkText()
        XCTAssertNil(view.selectedCommandBlock)
        XCTAssertEqual(sink.texts, ["你好", "输入"])
    }

    func testReportingAndAlternateScreenDoNotSelectBlock() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        view.inputSink = sink
        for flags: FrameFlags in [.mouseReporting, .altScreen] {
            view.currentFrame = try frame()
            try click(1, in: view)
            view.currentFrame = try frame(flags: flags)
            XCTAssertNil(view.selectedCommandBlock)
            try click(1, in: view)
            XCTAssertNil(view.selectedCommandBlock)
        }
        XCTAssertEqual(sink.mice.count, 8)
        XCTAssertEqual(sink.mice.suffix(2).map(\.kind), [.down, .up])
    }

    func testRemovedBlocksClearSelection() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        view.currentFrame = try frame()
        try click(4, in: view)
        view.currentFrame = try frame(rows: 3)
        XCTAssertNil(view.selectedCommandBlock)
        view.selectedCommandBlock = 1
        view.currentFrame = FrameSnapshot.mock(cols: 80, rows: 12, seq: 2)
        XCTAssertNil(view.selectedCommandBlock)
    }

    func testCopySelectedBlockAndPreferTextSelection() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        let actions = CommandBlockEditActions()
        view.editActions = actions
        view.selectedCommandBlock = 2
        var writes: [String] = []
        var notices = 0
        view.writeCopiedText = { writes.append($0); return true }
        view.onCopied = { notices += 1 }
        let menu = NSMenuItem(title: "", action: #selector(TerminalView.copy(_:)), keyEquivalent: "")
        XCTAssertTrue(view.validateMenuItem(menu))
        view.copy(nil)
        XCTAssertEqual(actions.copied, [.all])
        actions.hasSelection = true
        view.copy(nil)
        XCTAssertEqual(actions.copied, [.all])
        XCTAssertEqual(writes, ["command\noutput", "selected text"])
        XCTAssertEqual(notices, 2)
    }

    func testJumpSelectsReturnedTargetAndPreservesSelectionAtEndpoint() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        let actions = CommandBlockEditActions()
        view.editActions = actions
        view.previousCommandBlock(nil)
        XCTAssertEqual(view.selectedCommandBlock, 1)
        view.nextCommandBlock(nil)
        XCTAssertEqual(view.selectedCommandBlock, 2)
        actions.jumpTarget = nil
        view.nextCommandBlock(nil)
        XCTAssertEqual(view.selectedCommandBlock, 2)
    }

    func testHighlightCoversOnlySelectedRowsAndBodyPadding() throws {
        let snapshot = try frame()
        let renderer = try XCTUnwrap(TerminalRenderer())
        for mode: GutterMode in [.off, .both] {
            renderer.gutterMode = mode
            let layout = renderer.layout(scale: 2)
            for appearance: TerminalAppearance in [.dark, .light] {
                renderer.selectedCommandBlock = nil
                let baseline = try XCTUnwrap(renderer.renderImage(frame: snapshot,
                    size: CGSize(width: 800, height: 300), scale: 2, appearance: appearance))
                let original = try SnapshotTests.pixels(baseline)
                renderer.selectedCommandBlock = 2
                let image = try XCTUnwrap(renderer.renderImage(frame: snapshot,
                    size: CGSize(width: 800, height: 300), scale: 2, appearance: appearance))
                let pixels = try SnapshotTests.pixels(image)
                let palette: TerminalPalette = appearance == .dark ? .kansoDark : .kansoLight
                for row in 0..<snapshot.rows {
                    let middle = layout.originY + row * layout.metrics.cellHeight + layout.metrics.cellHeight / 2
                    for column in [layout.gutterWidth + 3, layout.originX - 2, image.width - 2] {
                        let offset = (middle * image.width + column) * 4
                        let expected = (3...5).contains(row)
                            ? [palette.blockSelection.r, palette.blockSelection.g, palette.blockSelection.b]
                            : Array(original[offset..<(offset + 3)])
                        XCTAssertEqual(Array(pixels[offset..<(offset + 3)]), expected, "row=\(row), x=\(column)")
                    }
                    if layout.gutterWidth > 0 {
                        let offset = (middle * image.width + layout.gutterWidth / 2) * 4
                        XCTAssertEqual(Array(pixels[offset..<(offset + 3)]), Array(original[offset..<(offset + 3)]))
                    }
                }
                let stripe = ((layout.originY + 4 * layout.metrics.cellHeight) * image.width
                    + layout.originX - layout.paddingX / 2) * 4
                XCTAssertEqual(Array(pixels[stripe..<(stripe + 3)]), [palette.ansi[1].r, palette.ansi[1].g, palette.ansi[1].b])
            }
        }
    }

    func testFailureStripeStaysBetweenGutterAndText() throws {
        let snapshot = try frame()
        let renderer = try XCTUnwrap(TerminalRenderer())
        for mode: GutterMode in [.off, .both] {
            renderer.gutterMode = mode
            let layout = renderer.layout(scale: 2)
            let stripeX = layout.originX - layout.paddingX / 2
            XCTAssertGreaterThan(stripeX, layout.gutterWidth)
            XCTAssertLessThan(stripeX + 2, layout.originX)
            for appearance: TerminalAppearance in [.dark, .light] {
                let image = try XCTUnwrap(renderer.renderImage(frame: snapshot,
                    size: CGSize(width: 800, height: 300), scale: 2, appearance: appearance))
                let pixels = try SnapshotTests.pixels(image)
                let red = (appearance == .dark ? TerminalPalette.kansoDark : .kansoLight).ansi[1]
                for column in (stripeX - 1)...(stripeX + 2) {
                    let redCount = (0..<image.height).filter { row in
                        let offset = (row * image.width + column) * 4
                        return pixels[offset] == red.r && pixels[offset + 1] == red.g && pixels[offset + 2] == red.b
                    }.count
                    XCTAssertEqual(redCount, (stripeX..<(stripeX + 2)).contains(column) ? 3 * layout.metrics.cellHeight - 1 : 0)
                }
            }
        }
    }

    func testCopyUsesCoreTextAndNotifiesStatusBar() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        let actions = CommandBlockEditActions()
        view.editActions = actions
        var writes: [String] = []
        var notices = 0
        view.writeCopiedText = { writes.append($0); return true }
        view.onCopied = { notices += 1 }
        for part: FfiBlockText in [.command, .output, .all] { view.copyCommandBlock(2, part: part) }
        XCTAssertEqual(actions.copied, [.command, .output, .all])
        XCTAssertEqual(writes, ["command", "output", "command\noutput"])
        XCTAssertEqual(notices, 3)
        view.previousCommandBlock(nil)
        view.nextCommandBlock(nil)
        XCTAssertEqual(actions.jumps, [false, true])
    }

    func testBlockMenuValidationUsesAvailableText() throws {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        view.currentFrame = try frame()
        let command = NSMenuItem(title: "", action: #selector(TerminalView.copyBlockCommand(_:)), keyEquivalent: "")
        let output = NSMenuItem(title: "", action: #selector(TerminalView.copyBlockOutput(_:)), keyEquivalent: "")
        view.menuCommandBlock = 2
        XCTAssertTrue(view.validateMenuItem(command))
        XCTAssertTrue(view.validateMenuItem(output))
        view.menuCommandBlock = 3
        XCTAssertFalse(view.validateMenuItem(command))
        XCTAssertFalse(view.validateMenuItem(output))
    }
}

@MainActor
final class CommandBlockEditActions: TerminalEditActions {
    var hasSelection = false
    var copied: [FfiBlockText] = []
    var jumps: [Bool] = []
    var jumpTarget: UInt64? = 1
    func selectionText() -> String? { hasSelection ? "selected text" : nil }
    func paste(_ content: TerminalPasteContent) {}
    func selectAll() {}
    func commandBlockText(id: UInt64, part: FfiBlockText) -> String? {
        copied.append(part)
        switch part {
        case .command: return "command"
        case .output: return "output"
        case .all: return "command\noutput"
        }
    }
    func jumpCommandBlock(forward: Bool) -> UInt64? {
        jumps.append(forward)
        return jumpTarget.map { forward ? $0 + 1 : $0 }
    }
}
