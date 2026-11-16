import AppKit
import KansoBridge
import XCTest
@testable import TerminalKit

@MainActor
final class CommandBlockTests: XCTestCase {
    private func frame() throws -> FrameSnapshot {
        try XCTUnwrap(FrameSnapshot(coreFrame: FrameHandle.mockCommandBlocks(cols: 80, rows: 12)))
    }

    func testHoverButtonsAndMouseEventPassthrough() throws {
        let snapshot = try frame()
        let overlay = CommandBlockOverlayView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        let container = NSView(frame: overlay.frame)
        container.addSubview(overlay)
        let geometry = TerminalHitGeometry(originX: 100, originY: 4, cellWidth: 8, cellHeight: 18, cols: 80, rows: 12)
        overlay.update(snapshot: snapshot, geometry: geometry, palette: .kansoDark, hoveredID: 2, selecting: false)
        XCTAssertEqual(overlay.hoveredBlock?.id, 2)
        XCTAssertEqual(overlay.buttons.filter { !$0.isHidden }.count, 3)
        XCTAssertNil(overlay.hitTest(NSPoint(x: 200, y: 80)))
        let button = overlay.buttons[0]
        let buttonCenter = overlay.convert(NSPoint(x: button.frame.midX, y: button.frame.midY), to: container)
        XCTAssertTrue(overlay.hitTest(buttonCenter) === button)
        var copied: (UInt64, FfiBlockText)?
        overlay.onCopy = { copied = ($0, $1) }
        button.performClick(nil)
        XCTAssertEqual(copied?.0, 2)
        XCTAssertEqual(copied?.1, .command)
        overlay.update(snapshot: snapshot, geometry: geometry, palette: .kansoDark, hoveredID: 3, selecting: false)
        XCTAssertEqual(overlay.buttons.filter { !$0.isHidden }.map(\.tag), [2])
        overlay.update(snapshot: snapshot, geometry: geometry, palette: .kansoDark, hoveredID: 2, selecting: true)
        XCTAssertNil(overlay.hoveredBlock)
        XCTAssertTrue(overlay.buttons.allSatisfy(\.isHidden))
        overlay.update(snapshot: snapshot, geometry: geometry, palette: .kansoDark, hoveredID: nil, selecting: false)
        XCTAssertNil(overlay.hoveredBlock)
    }

    func testMouseReportingAndAlternateScreenSuppressHover() {
        XCTAssertTrue(CommandBlockOverlayView.showsHover(flags: [], selecting: false))
        XCTAssertFalse(CommandBlockOverlayView.showsHover(flags: .mouseReporting, selecting: false))
        XCTAssertFalse(CommandBlockOverlayView.showsHover(flags: .altScreen, selecting: false))
        XCTAssertFalse(CommandBlockOverlayView.showsHover(flags: [], selecting: true))
    }

    func testStatusOnlyUsesEmptyRightEnd() throws {
        let snapshot = try frame()
        XCTAssertTrue(CommandBlockOverlayView.statusFits(snapshot, row: 0, fromColumn: 60))
        XCTAssertFalse(CommandBlockOverlayView.statusFits(snapshot, row: 0, fromColumn: 2))
        let dense = try XCTUnwrap(FrameSnapshot.mockDense(cols: 80, rows: 4, seq: 0))
        XCTAssertFalse(CommandBlockOverlayView.statusFits(dense, row: 0, fromColumn: 60))
    }

    func testFailureStripeStaysBetweenGutterAndText() throws {
        let snapshot = try frame()
        let renderer = try XCTUnwrap(TerminalRenderer())
        for mode: GutterMode in [.off, .both] {
            renderer.gutterMode = mode
            let layout = renderer.layout(scale: 2)
            let stripeX = layout.originX - layout.padding / 2
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
    func selectionText() -> String? { nil }
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
    func jumpCommandBlock(forward: Bool) { jumps.append(forward) }
}
