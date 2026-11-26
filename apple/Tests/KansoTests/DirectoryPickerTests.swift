import AppKit
import CKansoFFI
import KansoBridge
@testable import Kanso
@testable import TerminalKit
import XCTest

@MainActor
final class DirectoryPickerTests: XCTestCase {
    private func frame(flags: FrameFlags = [], icon: UInt32 = 0xf07b) throws -> FrameSnapshot {
        let handle = FrameHandle.mockCommandBlocks(cols: 80, rows: 12)
        let header = try XCTUnwrap(UnsafeMutablePointer<KansoFrameHeader>(bitPattern: UInt(handle.headerAddress())))
        header.pointee.flags |= flags.rawValue
        header.pointee.cursor_row = 6
        let cursor = try XCTUnwrap(UnsafeMutablePointer(mutating: kanso_frame_line_meta(header, 6)))
        cursor.pointee.flags |= LineFlags.cursor.rawValue
        for row: UInt16 in [0, 6] {
            let cells = try XCTUnwrap(UnsafeMutablePointer(mutating: kanso_frame_row(header, row)))
            for col in 2..<10 {
                cells[col].flags |= CellFlags.chip.rawValue
                // 与真实终端一致：标签开头的空白格字符码是 0（不是 32），其后的空格是 32。
                cells[col].ch = col == 3 ? icon : (col == 2 ? 0 : 32)
            }
        }
        return try XCTUnwrap(FrameSnapshot(coreFrame: handle))
    }

    private func location(row: Int = 6, col: Int = 2, in view: TerminalView) throws -> NSPoint {
        let geometry = try XCTUnwrap(view.hitGeometry())
        return NSPoint(x: geometry.originX + (Double(col) + 0.5) * geometry.cellWidth,
                       y: view.bounds.height - geometry.originY - (Double(row) + 0.5) * geometry.cellHeight)
    }

    func testLatestDirectoryChipIncludesPaddingAndDoesNotSelectBlock() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        view.currentFrame = try frame()
        var anchors: [NSRect] = []
        view.onDirectoryChipClick = { anchors.append($0) }
        for col in [2, 3, 9] {
            let point = try location(col: col, in: view)
            view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: point))
            view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: point))
        }
        XCTAssertEqual(anchors.count, 3)
        XCTAssertEqual(anchors.first, anchors.last)
        XCTAssertEqual(anchors.first?.width, try XCTUnwrap(view.hitGeometry()).cellWidth * 8)
    }

    func testOldPromptMouseReportingAlternateScreenOtherChipDragAndMultipleClickAreIgnored() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        var shown = 0
        view.onDirectoryChipClick = { _ in shown += 1 }
        for (flags, icon, row) in [(FrameFlags(), UInt32(0xf07b), 0), (.mouseReporting, 0xf07b, 6),
                                   (.altScreen, 0xf07b, 6), (FrameFlags(), 0xe0a0, 6)] {
            view.currentFrame = try frame(flags: flags, icon: icon)
            let point = try location(row: row, in: view)
            view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: point))
            view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: point))
        }
        view.currentFrame = try frame()
        let point = try location(in: view)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: point))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: point))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: point))
        for count in [2, 3] {
            view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: point, clickCount: count))
            view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: point, clickCount: count))
        }
        XCTAssertEqual(shown, 0)
        let snapshot = try frame()
        var block = try XCTUnwrap(snapshot.commandBlocks.last)
        block.running = true
        XCTAssertNil(TerminalView.directoryChipColumns(row: 6, col: 3, frame: snapshot, block: block))
        block.running = false
        block.latest = false
        XCTAssertNil(TerminalView.directoryChipColumns(row: 6, col: 3, frame: snapshot, block: block))
    }

    func testDirectoryLoadingSortingFilteringHiddenEntriesAndParent() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        for name in ["zebra", "Alpha", ".hidden", "中文目录", "space '\""] {
            try FileManager.default.createDirectory(at: root.appendingPathComponent(name), withIntermediateDirectories: false)
        }
        try Data().write(to: root.appendingPathComponent("a-file"))
        var model = DirectoryPickerModel(directory: root.path)
        model.candidates = try await DirectoryPickerModel.load(directory: root.path)
        let sorted = ["Alpha", "space '\"", "zebra", "中文目录"].sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        XCTAssertEqual(model.entries.map(\.name), [".."] + sorted)
        XCTAssertEqual(model.entries.first?.path, root.deletingLastPathComponent().path)
        model.query = "ap"
        XCTAssertEqual(model.entries.map(\.name), ["..", "Alpha"])
        model.query = "."
        XCTAssertEqual(model.entries.map(\.name), ["..", ".hidden"])
        model.query = "hdn"
        XCTAssertEqual(model.entries.map(\.name), [".."])
        model.query = "中目"
        XCTAssertEqual(model.entries.map(\.name), ["..", "中文目录"])
    }

    func testKeyboardAcceptanceSendsExactBytesAndEscapeCancels() {
        let sink = RecordingSink()
        let controller = DirectoryPickerViewController(directory: "/tmp/base")
        _ = controller.view
        controller.model.candidates = CommandPaletteFuzzy.prepare([
            DirectoryPickerModel.Entry(name: "中文", path: "/tmp/a '\"\n中文\n")
        ], text: { $0.name })
        controller.reload()
        XCTAssertGreaterThan(controller.tableView.tableColumns[0].width, 300)
        controller.onAccept = { DirectoryPickerModel.changeDirectory(to: $0, through: sink) }
        var cancelled = false
        controller.onCancel = { cancelled = true }
        let editor = NSTextView()
        XCTAssertTrue(controller.control(controller.searchField.textField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
        XCTAssertEqual(controller.tableView.selectedRow, 1)
        XCTAssertTrue(controller.control(controller.searchField.textField, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        XCTAssertEqual(sink.texts, ["\u{1b}[9001~2f746d702f612027220ae4b8ade696870a\n"])
        XCTAssertTrue(controller.control(controller.searchField.textField, textView: editor, doCommandBy: #selector(NSResponder.moveUp(_:))))
        controller.acceptSelection()
        XCTAssertEqual(sink.texts.last, "\u{1b}[9001~2f746d70\n")
        XCTAssertTrue(controller.control(controller.searchField.textField, textView: editor, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertTrue(cancelled)
        XCTAssertTrue(sink.keys.isEmpty)
    }

    func testPanelFlipsAboveAndOutsideClickDismisses() {
        let parent = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 600), styleMask: [.borderless], backing: .buffered, defer: false)
        let panel = FloatingPanel(content: NSView(), size: DirectoryPickerViewController.size, flipsAbove: true)
        panel.position(below: NSRect(x: 40, y: 20, width: 120, height: 20), within: NSRect(x: 0, y: 0, width: 800, height: 600))
        XCTAssertEqual(panel.frame.minY, 46)
        var dismissed = false
        panel.onDismiss = { dismissed = true }
        panel.handleMouseDown(in: parent)
        XCTAssertTrue(dismissed)
    }

    func testDirectoryListKeepsEntriesBeyondTwoThousand() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        for index in 0..<2100 {
            try FileManager.default.createDirectory(at: root.appendingPathComponent(String(format: "folder-%04d", index)), withIntermediateDirectories: false)
        }
        var model = DirectoryPickerModel(directory: root.path)
        model.candidates = try await DirectoryPickerModel.load(directory: root.path)
        XCTAssertEqual(model.entries.count, 2101)
        model.query = "2099"
        XCTAssertEqual(model.entries.map(\.name), ["..", "folder-2099"])
    }

    func testPickerTextHasEnglishAndChineseTranslations() throws {
        for (language, expected) in [("en", ["Search directories…", ".. (Parent Directory)"]),
                                      ("zh-Hans", ["搜索目录…", ".. (上级目录)"])] {
            let path = try XCTUnwrap(Bundle(for: DirectoryPickerViewController.self).path(forResource: language, ofType: "lproj"))
            let bundle = try XCTUnwrap(Bundle(path: path))
            XCTAssertEqual(String(localized: "Search directories…", bundle: bundle), expected[0])
            XCTAssertEqual(String(localized: ".. (Parent Directory)", bundle: bundle), expected[1])
        }
    }
}
