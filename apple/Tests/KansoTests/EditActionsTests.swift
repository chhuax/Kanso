import AppKit
import CKansoFFI
import KansoBridge
import XCTest
@testable import TerminalKit
@testable import Kanso

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

    private func frame(flags: FrameFlags = []) throws -> FrameSnapshot {
        let handle = FrameHandle.mockCommandBlocks(cols: 80, rows: 12)
        let header = try XCTUnwrap(UnsafeMutablePointer<KansoFrameHeader>(bitPattern: UInt(handle.headerAddress())))
        header.pointee.flags |= flags.rawValue
        return try XCTUnwrap(FrameSnapshot(coreFrame: handle))
    }

    func testCopyOnSelectCopiesOnlyWhenEnabledAndSelected() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        let actions = FakeEditActions()
        view.inputSink = sink
        view.editActions = actions
        var copied: [String] = []
        var notices = 0
        view.writeCopiedText = { copied.append($0); return true }
        view.onCopied = { notices += 1 }
        let g = try XCTUnwrap(view.hitGeometry())
        func at(col: Double, row: Double) -> NSPoint {
            NSPoint(x: g.originX + (col + 0.2) * g.cellWidth, y: view.bounds.height - (g.originY + (row + 0.5) * g.cellHeight))
        }

        // 默认关闭：有选区也不自动复制。
        XCTAssertFalse(view.copyOnSelect)
        actions.hasSelection = true
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1)))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: at(col: 5, row: 1)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 5, row: 1)))
        XCTAssertTrue(copied.isEmpty)

        // 开启后：完成选择自动复制，并触发状态栏提示。
        view.copyOnSelect = true
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1)))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: at(col: 5, row: 1)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 5, row: 1)))
        XCTAssertEqual(copied, ["selected"])
        XCTAssertEqual(notices, 1)

        // 单击无选区：即使选中了命令块也不触发"选中即复制"。
        actions.hasSelection = false
        view.selectedCommandBlock = 42
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 1, row: 1)))
        XCTAssertEqual(copied.count, 1)

        // 鼠标上报开启且未按 Shift：交给程序，不自动复制。
        actions.hasSelection = true
        view.currentFrame = try frame(flags: .mouseReporting)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1)))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: at(col: 5, row: 1)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 5, row: 1)))
        XCTAssertEqual(copied.count, 1)

        // 鼠标上报开启但按下时按住 Shift 绕过：松开时自动复制（中途松开 Shift 仍有效）。
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: at(col: 1, row: 1), flags: .shift))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: at(col: 5, row: 1)))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: at(col: 5, row: 1)))
        XCTAssertEqual(copied, ["selected", "selected"])
        XCTAssertEqual(notices, 2)
    }

    /// 1.0.7 把选中即复制存在 display.copyOnSelect：没有 mouse 分区时迁移一次并写回，之后以 mouse 为准。
    func testCopyOnSelectMigratesFromDisplaySection() {
        let backend = MemorySettingsBackend()
        backend.values["display"] = #"{"v":1,"copyOnSelect":true}"#
        XCTAssertTrue(SettingsStore.attached(to: backend).mouse.copyOnSelect)
        XCTAssertNotNil(backend.values["mouse"])

        // 写回后再改 mouse，旧字段不会把值翻回去。
        let settings = SettingsStore.attached(to: backend)
        settings.updateMouse { $0.copyOnSelect = false }
        XCTAssertFalse(SettingsStore.attached(to: backend).mouse.copyOnSelect)

        // 没有旧字段：保持默认，也不凭空写 mouse 分区。
        let fresh = MemorySettingsBackend()
        fresh.values["display"] = #"{"v":1}"#
        XCTAssertFalse(SettingsStore.attached(to: fresh).mouse.copyOnSelect)
        XCTAssertNil(fresh.values["mouse"])
    }

    func testCopyOnSelectSettingsAndEditMenuItem() {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))

        XCTAssertFalse(settings.mouse.copyOnSelect)
        TerminalSettingsApplier.apply(settings, to: view)
        XCTAssertFalse(view.copyOnSelect)

        settings.updateMouse { $0.copyOnSelect = true }
        TerminalSettingsApplier.apply(settings, to: view)
        XCTAssertTrue(view.copyOnSelect)

        let reloaded = SettingsStore.attached(to: backend)
        XCTAssertTrue(reloaded.mouse.copyOnSelect)

        let main = AppDelegate.makeMainMenu(
            appearance: AppearanceController(settings: settings, apply: { _ in }),
            gutter: GutterModeController(settings: settings)
        )
        let items = main.items.flatMap { $0.submenu?.items ?? [] }
        XCTAssertTrue(items.contains { $0.action == #selector(MainWindowController.kansoCopyOnSelect(_:)) })
    }

    func testRightClickCopyOrPasteCopiesSelectionElsePastes() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let scratch = NSPasteboard(name: NSPasteboard.Name("com.chhuax.kanso.tests.\(UUID().uuidString)"))
        defer { scratch.releaseGlobally() }
        scratch.clearContents()
        scratch.setString("clip", forType: .string)
        view.pasteboard = scratch
        let sink = RecordingSink()
        let actions = FakeEditActions()
        view.inputSink = sink
        view.editActions = actions
        var copied: [String] = []
        view.writeCopiedText = { copied.append($0); return true }
        var menuRequested = 0
        view.contextMenuProvider = { menuRequested += 1; return NSMenu() }
        view.rightClickShowsMenu = false
        let point = NSPoint(x: 300, y: 200)

        actions.hasSelection = true
        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: point))
        view.rightMouseDragged(with: InputFixtures.mouse(.rightMouseDragged, at: point))
        view.rightMouseUp(with: InputFixtures.mouse(.rightMouseUp, at: point))
        XCTAssertEqual(copied, ["selected"])
        XCTAssertTrue(actions.pasted.isEmpty)
        // 复制后选区被清掉，下一次右键才会粘贴（不再手动清选区，验证的就是这一点）。
        XCTAssertFalse(actions.hasSelection)

        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: point))
        view.rightMouseUp(with: InputFixtures.mouse(.rightMouseUp, at: point))
        XCTAssertEqual(actions.pasted, [.text("clip")])
        XCTAssertEqual(copied.count, 1)

        // 不弹菜单、不预选词，也不给 core 发任何鼠标事件（含孤立的 drag / up）。
        XCTAssertEqual(menuRequested, 0)
        XCTAssertTrue(actions.selectedWordPoints.isEmpty)
        XCTAssertTrue(sink.mice.isEmpty)
    }

    func testMiddleClickPasteOrNoAction() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let scratch = NSPasteboard(name: NSPasteboard.Name("com.chhuax.kanso.tests.\(UUID().uuidString)"))
        defer { scratch.releaseGlobally() }
        scratch.clearContents()
        scratch.setString("clip", forType: .string)
        view.pasteboard = scratch
        let sink = RecordingSink()
        let actions = FakeEditActions()
        view.inputSink = sink
        view.editActions = actions
        let point = NSPoint(x: 300, y: 200)

        func click() {
            view.otherMouseDown(with: Self.middleEvent(.otherMouseDown, at: point))
            view.otherMouseDragged(with: Self.middleEvent(.otherMouseDragged, at: point))
            view.otherMouseUp(with: Self.middleEvent(.otherMouseUp, at: point))
        }

        XCTAssertTrue(view.middleClickPastes)
        click()
        XCTAssertEqual(actions.pasted, [.text("clip")])

        view.middleClickPastes = false
        click()
        XCTAssertEqual(actions.pasted.count, 1)
        XCTAssertTrue(sink.mice.isEmpty)
    }

    func testMouseReportingHandsClicksToProgramUnlessShift() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let scratch = NSPasteboard(name: NSPasteboard.Name("com.chhuax.kanso.tests.\(UUID().uuidString)"))
        defer { scratch.releaseGlobally() }
        scratch.clearContents()
        scratch.setString("clip", forType: .string)
        view.pasteboard = scratch
        let sink = RecordingSink()
        let actions = FakeEditActions()
        view.inputSink = sink
        view.editActions = actions
        view.rightClickShowsMenu = false
        view.currentFrame = try frame(flags: .mouseReporting)
        let point = NSPoint(x: 300, y: 200)

        // 未按 Shift：右键和中键都交给程序，终端不复制也不粘贴。
        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: point))
        view.rightMouseUp(with: InputFixtures.mouse(.rightMouseUp, at: point))
        view.otherMouseDown(with: Self.middleEvent(.otherMouseDown, at: point))
        view.otherMouseUp(with: Self.middleEvent(.otherMouseUp, at: point))
        XCTAssertEqual(sink.mice.map(\.button), [.right, .right, .middle, .middle])
        XCTAssertTrue(actions.pasted.isEmpty)

        // 按住 Shift：由终端处理，设置重新生效，且不再发给程序。
        sink.events.removeAll()
        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: point, flags: .shift))
        view.otherMouseDown(with: Self.middleEvent(.otherMouseDown, at: point, flags: .shift))
        XCTAssertEqual(actions.pasted, [.text("clip"), .text("clip")])
        XCTAssertTrue(sink.mice.isEmpty)
    }

    func testMouseSettingsDecodingAndRoundTrip() throws {
        func decode(_ json: String) throws -> MouseSettings {
            try XCTUnwrap(SettingsCodec.decode(MouseSettings.self, from: json))
        }
        XCTAssertEqual(try decode("{}"), MouseSettings())
        XCTAssertEqual(try decode(#"{"copyOnSelect":true}"#).middleClick, .paste)
        XCTAssertEqual(try decode(#"{"copyOnSelect":true}"#).rightClick, .menu)
        let unknown = try decode(#"{"middleClick":"scroll","rightClick":7,"copyOnSelect":"yes"}"#)
        XCTAssertEqual(unknown, MouseSettings())

        var custom = MouseSettings()
        custom.middleClick = .nothing
        custom.rightClick = .copyOrPaste
        custom.copyOnSelect = true
        let text = SettingsCodec.encode(custom)
        XCTAssertEqual(text, #"{"copyOnSelect":true,"middleClick":"none","rightClick":"copyOrPaste","v":1}"#)
        XCTAssertEqual(try decode(text), custom)
    }

    func testRightClickShowsMenuEditMenuItemToggles() throws {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        let main = AppDelegate.makeMainMenu(
            appearance: AppearanceController(settings: settings, apply: { _ in }),
            gutter: GutterModeController(settings: settings)
        )
        let items = main.items.flatMap { $0.submenu?.items ?? [] }
        XCTAssertTrue(items.contains { $0.action == #selector(MainWindowController.kansoRightClickShowsMenu(_:)) })
        XCTAssertEqual(settings.mouse.rightClick, .menu)

        settings.updateMouse { $0.rightClick = .copyOrPaste }
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        TerminalSettingsApplier.apply(settings, to: view)
        XCTAssertFalse(view.rightClickShowsMenu)
        XCTAssertTrue(view.middleClickPastes)

        settings.updateMouse { $0.middleClick = .nothing }
        TerminalSettingsApplier.apply(settings, to: view)
        XCTAssertFalse(view.middleClickPastes)
        XCTAssertEqual(SettingsStore.attached(to: backend).mouse.rightClick, .copyOrPaste)
    }

    /// `NSEvent.mouseEvent` 造不出 buttonNumber == 2 的事件，这里借 CGEvent 构造中键事件。
    private static func middleEvent(_ type: NSEvent.EventType, at point: NSPoint, flags: NSEvent.ModifierFlags = []) -> NSEvent {
        let cgType: CGEventType = type == .otherMouseDown ? .otherMouseDown : type == .otherMouseUp ? .otherMouseUp : .otherMouseDragged
        let cg = CGEvent(mouseEventSource: nil, mouseType: cgType, mouseCursorPosition: point, mouseButton: .center)!
        cg.flags = CGEventFlags(rawValue: UInt64(flags.rawValue))
        return NSEvent(cgEvent: cg)!
    }

    func testContextMenuHasIconsShortcutsAndBlockAnchor() throws {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        let keybindings = KeybindingController(settings: settings, menuProvider: { nil })
        _ = keybindings.assign(KeyChord(key: "d", modifiers: .command), to: .splitRight)
        keybindings.clear(.find)

        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        XCTAssertTrue(view.responds(to: #selector(NSText.copy(_:))))
        let menu = TerminalHost.makeContextMenu(view: view, session: 99, keybindings: keybindings)
        let nonSeparators = menu.items.filter { !$0.isSeparatorItem }
        let actions = nonSeparators.map(\.action)
        XCTAssertEqual(actions, [
            #selector(TerminalView.copy(_:)),
            #selector(TerminalView.paste(_:)),
            #selector(TerminalView.selectAll(_:)),
            #selector(MainWindowController.kansoFind(_:)),
            #selector(MainWindowController.kansoClearBuffer(_:)),
            #selector(MainWindowController.kansoSplitRight(_:)),
            #selector(MainWindowController.kansoSplitDown(_:)),
            #selector(MainWindowController.kansoLocateWorkingDirectory(_:)),
            #selector(MainWindowController.kansoCloseSession(_:)),
        ])
        for item in nonSeparators {
            XCTAssertNotNil(item.image, "右键菜单项应带图标：\(item.title)")
        }
        let findItem = try XCTUnwrap(nonSeparators.first { $0.action == #selector(MainWindowController.kansoFind(_:)) })
        XCTAssertEqual(findItem.keyEquivalent, "", "用户清空快捷键后右键菜单不应回落默认键")
        let splitRight = try XCTUnwrap(nonSeparators.first { $0.action == #selector(MainWindowController.kansoSplitRight(_:)) })
        XCTAssertEqual(splitRight.keyEquivalent, "d")
        XCTAssertTrue(splitRight.keyEquivalentModifierMask.contains(.command))
        let locate = try XCTUnwrap(nonSeparators.first { $0.action == #selector(MainWindowController.kansoLocateWorkingDirectory(_:)) })
        XCTAssertEqual(locate.representedObject as? UInt64, 99)

        // 命令块条目插在关闭会话分隔线之前，保证关闭会话始终在最底端。
        view.currentFrame = try frame()
        let g = try XCTUnwrap(view.hitGeometry())
        let blockPoint = NSPoint(x: g.originX + g.cellWidth, y: view.bounds.height - g.originY - 1.5 * g.cellHeight)
        view.addCommandBlockMenuItems(to: menu, event: InputFixtures.mouse(.rightMouseDown, at: blockPoint))
        let afterBlockActions = menu.items.filter { !$0.isSeparatorItem }.map(\.action)
        XCTAssertEqual(afterBlockActions.suffix(3), [
            #selector(TerminalView.copyBlockCommand(_:)),
            #selector(TerminalView.copyBlockOutput(_:)),
            #selector(MainWindowController.kansoCloseSession(_:)),
        ])

        // 鼠标上报开启且未按 Shift 时右键透传给程序，不弹菜单。
        let sink = RecordingSink()
        view.inputSink = sink
        view.currentFrame = try frame(flags: .mouseReporting)
        var menuRequested = 0
        view.contextMenuProvider = { menuRequested += 1; return menu }
        view.rightMouseDown(with: InputFixtures.mouse(.rightMouseDown, at: NSPoint(x: 100, y: 100)))
        XCTAssertEqual(menuRequested, 0)
        XCTAssertEqual(sink.mice.last?.button, .right)
    }
}

@MainActor
private final class FakeEditActions: TerminalEditActions {
    var hasSelection = false
    var selectAllCount = 0
    var selectedWordPoints: [ViewportPointValue] = []
    var pasted: [TerminalPasteContent] = []

    func selectionText() -> String? { hasSelection ? "selected" : nil }
    func paste(_ content: TerminalPasteContent) { pasted.append(content) }
    func selectAll() { selectAllCount += 1 }
    func selectWord(at point: ViewportPointValue) { selectedWordPoints.append(point) }
    func clearSelection() { hasSelection = false }
}
