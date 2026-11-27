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

    func testFailureStripeStaysBetweenGutterAndText() throws {
        let snapshot = try frame()
        let renderer = try XCTUnwrap(TerminalRenderer())
        for mode: GutterMode in [.off, .both] {
            renderer.gutterMode = mode
            let layout = renderer.layout(scale: 2)
            let centerX = layout.gutterWidth + layout.paddingX / 2
            var boxSize = Int((9 * CGFloat(2)).rounded())
            if boxSize % 2 == 0 { boxSize += 1 }
            let boxHalf = boxSize / 2
            XCTAssertGreaterThan(centerX - boxHalf, layout.gutterWidth)
            XCTAssertLessThan(centerX + boxHalf, layout.originX)
            for appearance: TerminalAppearance in [.dark, .light] {
                let image = try XCTUnwrap(renderer.renderImage(frame: snapshot,
                    size: CGSize(width: 800, height: 300), scale: 2, appearance: appearance))
                let pixels = try SnapshotTests.pixels(image)
                let red = (appearance == .dark ? TerminalPalette.kansoDark : .kansoLight).ansi[1]
                let redCountInColumn: (Int) -> Int = { column in
                    (0..<image.height).filter { row in
                        let offset = (row * image.width + column) * 4
                        return pixels[offset] == red.r && pixels[offset + 1] == red.g && pixels[offset + 2] == red.b
                    }.count
                }
                // 失败的可折叠块在 gutter 与正文之间绘制红色 [-]/树形引导线，不侵入 gutter 或正文。
                XCTAssertGreaterThan(redCountInColumn(centerX), layout.metrics.cellHeight)
                XCTAssertEqual(redCountInColumn(centerX - boxHalf), boxSize)
                // 右框线与末行 └ 尾部在 centerX + boxHalf 列相接，共 boxSize + 1 像素。
                XCTAssertEqual(redCountInColumn(centerX + boxHalf), boxSize + 1)
                XCTAssertEqual(redCountInColumn(layout.gutterWidth + 1), 0)
                XCTAssertEqual(redCountInColumn(layout.originX - 1), 0)
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
