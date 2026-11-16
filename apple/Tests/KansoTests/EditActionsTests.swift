import AppKit
import XCTest
@testable import TerminalKit

/// 复制 / 粘贴 / 全选的接线测试。
///
/// 剪贴板归类一律用临时命名的剪贴板，**不读写用户的系统剪贴板**（`NSPasteboard.general`）。
@MainActor
final class EditActionsTests: XCTestCase {
    /// 每个用例一个临时命名的剪贴板，用完释放。不用 setUp/tearDown：它们在非主 actor 上下文里执行，
    /// 而本类整体隔离在主 actor 上。
    private func withScratchPasteboard(_ body: (NSPasteboard) -> Void) {
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("com.chhuax.kanso.tests.\(UUID().uuidString)"))
        pasteboard.clearContents()
        defer { pasteboard.releaseGlobally() }
        body(pasteboard)
    }

    func testClassifyText() {
        withScratchPasteboard { pasteboard in
            pasteboard.setString("echo hi", forType: .string)
            XCTAssertEqual(TerminalView.classify(pasteboard), .text("echo hi"))
        }
    }

    /// Finder 复制的文件同时带文件名文本：必须按文件处理（terminal.md §8.3 第 1 条）。
    func testClassifyFilesWinOverText() {
        withScratchPasteboard { pasteboard in
            let url = URL(fileURLWithPath: "/tmp/with space.txt")
            pasteboard.writeObjects([url as NSURL])
            pasteboard.setString("with space.txt", forType: .string)
            XCTAssertEqual(TerminalView.classify(pasteboard), .files(["/tmp/with space.txt"]))
        }
    }

    func testClassifyImageOnly() {
        withScratchPasteboard { pasteboard in
            let image = NSImage(size: NSSize(width: 2, height: 2))
            image.lockFocus()
            NSColor.red.setFill()
            NSRect(x: 0, y: 0, width: 2, height: 2).fill()
            image.unlockFocus()
            pasteboard.writeObjects([image])
            XCTAssertEqual(TerminalView.classify(pasteboard), .imageOnly)
        }
    }

    func testClassifyEmpty() {
        withScratchPasteboard { pasteboard in
            XCTAssertNil(TerminalView.classify(pasteboard))
        }
    }

    func testMenuValidationFollowsSelection() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let actions = FakeEditActions()
        view.editActions = actions
        let copyItem = NSMenuItem(title: "Copy", action: #selector(TerminalView.copy(_:)), keyEquivalent: "c")
        let pasteItem = NSMenuItem(title: "Paste", action: #selector(TerminalView.paste(_:)), keyEquivalent: "v")

        actions.hasSelection = false
        XCTAssertFalse(view.validateMenuItem(copyItem), "没有选区时复制不可用")
        XCTAssertTrue(view.validateMenuItem(pasteItem))

        actions.hasSelection = true
        XCTAssertTrue(view.validateMenuItem(copyItem))

        view.editActions = nil
        XCTAssertFalse(view.validateMenuItem(pasteItem), "没有接上 core 时粘贴不可用")
    }

    func testSelectAllForwards() {
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let actions = FakeEditActions()
        view.editActions = actions
        view.selectAll(nil)
        XCTAssertEqual(actions.selectAllCount, 1)
    }
}

@MainActor
private final class FakeEditActions: TerminalEditActions {
    var hasSelection = false
    var selectAllCount = 0
    var pasted: [TerminalPasteContent] = []

    func selectionText() -> String? { hasSelection ? "selected" : nil }
    func paste(_ content: TerminalPasteContent) { pasted.append(content) }
    func selectAll() { selectAllCount += 1 }
}
