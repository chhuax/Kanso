import AppKit
import Carbon.HIToolbox
import KansoBridge
@testable import TerminalKit
import XCTest
@testable import Kanso

/// 快捷键（ui-shell.md §7）：组合键编解码、改绑规则、菜单生效、终端按键拦截（terminal.md §8.2 / §9）、
/// 录制交互与设置页。全部用内存设置后端与自建菜单，不碰真实存储、全局菜单与系统剪贴板。
@MainActor
final class KeybindingTests: XCTestCase {
    // MARK: 夹具

    private struct Fixture {
        let settings: SettingsStore
        let backend: MemorySettingsBackend
        let menu: NSMenu
        let controller: KeybindingController
        let copied: CopyLog
    }

    final class CopyLog {
        var count = 0
    }

    private func fixture() -> Fixture {
        let backend = MemorySettingsBackend()
        let settings = SettingsStore.attached(to: backend)
        let menu = AppDelegate.makeMainMenu(
            appearance: AppearanceController(settings: settings, apply: { _ in }),
            gutter: GutterModeController(settings: settings)
        )
        let copied = CopyLog()
        let controller = KeybindingController(settings: settings, menuProvider: { menu }, copySelection: { _ in copied.count += 1 })
        controller.applyToMenu()
        return Fixture(settings: settings, backend: backend, menu: menu, controller: controller, copied: copied)
    }

    private func item(_ command: KeyCommand, in menu: NSMenu) -> NSMenuItem? {
        for item in menu.items {
            if item.identifier == command.menuIdentifier { return item }
            if let found = item.submenu.flatMap({ self.item(command, in: $0) }) { return found }
        }
        return nil
    }

    private func allItems(_ menu: NSMenu) -> [NSMenuItem] {
        menu.items.flatMap { [$0] + ($0.submenu.map(allItems) ?? []) }
    }

    private func event(_ chars: String, base: String? = nil, flags: NSEvent.ModifierFlags, code: UInt16 = 0) -> NSEvent {
        InputFixtures.key(.keyDown, code, characters: chars, base: base, flags: flags)
    }

    // MARK: 组合键

    func testChordStorageRoundTripAndDisplay() throws {
        let samples: [(KeyChord, String, String)] = [
            (KeyChord(key: "n", modifiers: .command), "cmd+n", "⌘N"),
            (KeyChord(key: "\\", modifiers: [.command, .shift]), "shift+cmd+\\", "⇧⌘\\"),
            (KeyChord(key: "left", modifiers: [.command, .option]), "alt+cmd+left", "⌥⌘←"),
            (KeyChord(key: "insert", modifiers: .control), "ctrl+insert", "⌃Ins"),
            (KeyChord(key: "+", modifiers: .command), "cmd++", "⌘+"),
            (KeyChord(key: "f5", modifiers: []), "f5", "F5"),
        ]
        for (chord, storage, display) in samples {
            XCTAssertEqual(chord.storage, storage)
            XCTAssertEqual(chord.display, display)
            XCTAssertEqual(KeyChord(storage: storage), chord, storage)
        }
        XCTAssertNil(KeyChord(storage: ""))
        XCTAssertNil(KeyChord(storage: "cmd+"))
        XCTAssertNil(KeyChord(storage: "cmd+Shift+x"))
        XCTAssertNil(KeyChord(storage: "cmd+toolongname"))

        XCTAssertEqual(
            KeyChord(keyEquivalent: "\\", modifierMask: .command),
            KeyChord(key: "\\", modifiers: .command)
        )
        let right = String(Character(UnicodeScalar(UInt32(NSRightArrowFunctionKey))!))
        XCTAssertEqual(KeyChord(keyEquivalent: right, modifierMask: [.command, .option])?.display, "⌥⌘→")
        XCTAssertNil(KeyChord(keyEquivalent: "", modifierMask: .command))
    }

    func testChordFromEventNormalizesShiftedSymbolsAndLetters() throws {
        // ⌘⇧G：字符是大写 G。
        XCTAssertEqual(KeyChord(event: event("G", base: "G", flags: [.command, .shift])),
                       KeyChord(key: "g", modifiers: [.command, .shift]))
        // ⌘⇧\：charactersIgnoringModifiers 带 Shift 效果给出 "|"，要还原成基础字符。
        XCTAssertEqual(KeyChord(event: event("|", base: "|", flags: [.command, .shift])),
                       KeyChord(key: "\\", modifiers: [.command, .shift]))
        // Ctrl+C 的 charactersIgnoringModifiers 是字母本身。
        XCTAssertEqual(KeyChord(event: event("\u{03}", base: "c", flags: .control)),
                       KeyChord(key: "c", modifiers: .control))
        // 方向键：功能键字符映射成名字；capsLock / fn 等噪声位被忽略。
        let left = String(UnicodeScalar(UInt32(NSLeftArrowFunctionKey))!)
        XCTAssertEqual(KeyChord(event: event(left, flags: [.command, .option, .function, .capsLock])),
                       KeyChord(key: "left", modifiers: [.command, .option]))
    }

    func testBindingValidity() {
        XCTAssertTrue(KeyChord(key: "c", modifiers: .control).isValidBinding)
        XCTAssertTrue(KeyChord(key: "x", modifiers: .option).isValidBinding)
        XCTAssertFalse(KeyChord(key: "c", modifiers: []).isValidBinding, "普通字符不能绑定：会让输入文字变成快捷键")
        XCTAssertFalse(KeyChord(key: "c", modifiers: .shift).isValidBinding)
        XCTAssertFalse(KeyChord(key: "space", modifiers: .shift).isValidBinding)
        XCTAssertTrue(KeyChord(key: "insert", modifiers: .shift).isValidBinding, "Shift+Insert 是常见的粘贴键")
        XCTAssertFalse(KeyChord(key: "left", modifiers: []).isValidBinding, "不带修饰键的方向键属于终端程序")
        XCTAssertTrue(KeyChord(key: "f5", modifiers: .shift).isValidBinding)
    }

    func testPlainCtrlLetterDefinition() {
        XCTAssertTrue(KeyChord(key: "c", modifiers: .control).isPlainCtrlLetter)
        XCTAssertFalse(KeyChord(key: "c", modifiers: [.control, .shift]).isPlainCtrlLetter)
        XCTAssertFalse(KeyChord(key: "c", modifiers: .option).isPlainCtrlLetter)
        XCTAssertFalse(KeyChord(key: "1", modifiers: .control).isPlainCtrlLetter)
    }

    // MARK: 命令表

    func testCommandTableCoversExistingMenuCommandsWithDefaultKeys() {
        let f = fixture()
        // 表里每个命令在主菜单里都有对应菜单项，且初始按键等于默认值（ui-shell.md §7）。
        for command in KeyCommand.allCases {
            let menuItem = item(command, in: f.menu)
            XCTAssertNotNil(menuItem, "\(command) 没有菜单项")
            XCTAssertEqual(menuItem?.action, command.action, "\(command)")
            XCTAssertEqual(menuItem?.keyEquivalent, command.defaultChord?.menuKeyEquivalent ?? "", "\(command)")
            XCTAssertEqual(menuItem?.keyEquivalentModifierMask.intersection([.command, .option, .shift, .control]),
                           command.defaultChord?.modifiers.flags ?? [], "\(command)")
        }
        // 规格表中已实现的默认键。
        let expect: [KeyCommand: String] = [
            .newSession: "⌘N", .newLocalShell: "⌘T", .closeSession: "⌘W", .find: "⌘F", .findNext: "⌘G",
            .clearBuffer: "⌘K", .previousSession: "⌘[", .nextSession: "⌘]", .splitRight: "⌘\\", .splitDown: "⇧⌘\\",
            .previousSplit: "⌥⌘[", .nextSplit: "⌥⌘]", .toggleTabBar: "⌥⌘←", .copy: "⌘C", .paste: "⌘V", .selectAll: "⌘A",
            .commandPalette: "⇧⌘P",
        ]
        for (command, display) in expect { XCTAssertEqual(command.defaultChord?.display, display, "\(command)") }
        XCTAssertNil(KeyCommand.commandSuggestions.defaultChord)
        XCTAssertNil(KeyCommand.clearCommandHistory.defaultChord)
        // 默认键两两不冲突。
        let chords = KeyCommand.allCases.compactMap(\.defaultChord)
        XCTAssertEqual(Set(chords).count, chords.count)
    }

    func testCommandBlockJumpDefaultAndRebinding() {
        let fixture = fixture()
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let actions = CommandBlockEditActions()
        view.editActions = actions
        let up = String(UnicodeScalar(UInt32(NSUpArrowFunctionKey))!)
        let down = String(UnicodeScalar(UInt32(NSDownArrowFunctionKey))!)
        XCTAssertTrue(fixture.controller.interceptTerminalKey(event(up, flags: .command), in: view))
        XCTAssertTrue(fixture.controller.interceptTerminalKey(event(down, flags: .command), in: view))
        XCTAssertEqual(actions.jumps, [false, true])
        XCTAssertEqual(fixture.controller.assign(KeyChord(key: "u", modifiers: [.command, .shift]), to: .previousCommandBlock), .assigned)
        XCTAssertNil(fixture.controller.match(event(up, flags: .command)))
        XCTAssertTrue(fixture.controller.interceptTerminalKey(event("U", flags: [.command, .shift]), in: view))
        XCTAssertEqual(actions.jumps, [false, true, false])
        XCTAssertEqual(item(.previousCommandBlock, in: fixture.menu)?.keyEquivalent, "u")
    }

    func testDefaultChordsDoNotBindPlainCtrlOrAltLetters() {
        // terminal.md §9：普通 Ctrl/Alt+字母永远不拦截——默认表里一个也没有。
        for command in KeyCommand.allCases {
            guard let chord = command.defaultChord else { continue }
            XCTAssertFalse(chord.isPlainCtrlLetter, "\(command)")
            XCTAssertFalse(chord.modifiers == .option && chord.isLetter, "\(command)")
        }
    }

    func testGoToTabItemsAreNotRebindable() {
        let f = fixture()
        let tabItems = allItems(f.menu).filter { $0.action == #selector(MainWindowController.kansoSelectTab(_:)) }
        XCTAssertEqual(tabItems.count, 9)
        for n in 1...9 {
            XCTAssertEqual(f.controller.assign(KeyChord(key: String(n), modifiers: .command), to: .newSession), .reserved)
        }
        XCTAssertEqual(tabItems.map(\.keyEquivalent), (1...9).map(String.init))
    }

    // MARK: 改绑、冲突、清空、恢复默认

    func testRebindMakesOldDefaultKeyInvalidInMenu() {
        let f = fixture()
        let copy = item(.copy, in: f.menu)!
        XCTAssertEqual(copy.keyEquivalent, "c")
        XCTAssertTrue(copy.keyEquivalentModifierMask.contains(.command))

        let chord = KeyChord(key: "c", modifiers: [.control, .shift])
        XCTAssertEqual(f.controller.assign(chord, to: .copy), .assigned)
        XCTAssertEqual(copy.keyEquivalent, "c")
        XCTAssertEqual(copy.keyEquivalentModifierMask.intersection([.command, .control, .shift, .option]), [.control, .shift])
        // 原默认键 ⌘C 不再属于任何菜单项（不能新旧两组键同时生效）。
        let stillCmdC = allItems(f.menu).filter { $0.keyEquivalent == "c" && $0.keyEquivalentModifierMask.contains(.command) }
        XCTAssertTrue(stillCmdC.isEmpty)
        XCTAssertNil(f.controller.match(event("c", flags: .command)))
        XCTAssertEqual(f.controller.match(event("C", base: "C", flags: [.control, .shift]))?.command, .copy)
        // 改绑写进设置并跨重启保留。
        XCTAssertEqual(f.backend.values["keybindings"].map { $0.contains("ctrl+shift+c") }, true)
        let reloaded = KeybindingController(settings: .attached(to: f.backend), menuProvider: { nil })
        XCTAssertEqual(reloaded.chord(for: .copy), chord)
    }

    func testRebindBackToDefaultRemovesOverride() {
        let f = fixture()
        // 不用 ⌘J：它现在是"在 Filer 中显示工作目录"的默认键。
        _ = f.controller.assign(KeyChord(key: "u", modifiers: .command), to: .find)
        XCTAssertNotNil(f.settings.keybindings.overrides["find"])
        _ = f.controller.assign(KeyChord(key: "f", modifiers: .command), to: .find)
        XCTAssertNil(f.settings.keybindings.overrides["find"])
    }

    func testClearLeavesNoKeyAndDefaultStopsWorking() {
        let f = fixture()
        f.controller.clear(.clearBuffer)
        XCTAssertNil(f.controller.chord(for: .clearBuffer))
        let menuItem = item(.clearBuffer, in: f.menu)!
        XCTAssertEqual(menuItem.keyEquivalent, "")
        XCTAssertTrue(menuItem.keyEquivalentModifierMask.isEmpty)
        XCTAssertNil(f.controller.match(event("k", flags: .command)))
        XCTAssertEqual(f.settings.keybindings.overrides["clearBuffer"], "", "清空存为空串，区别于\"用默认值\"")
        // 清空后原默认键可以绑给别的命令而不冲突。
        XCTAssertEqual(f.controller.assign(KeyChord(key: "k", modifiers: .command), to: .find), .assigned)
    }

    func testConflictIsReportedWithoutChangingAnythingThenReplaceClearsTheOther() {
        let f = fixture()
        let g = KeyChord(key: "g", modifiers: .command)
        XCTAssertEqual(f.controller.assign(g, to: .find), .conflict(.findNext))
        XCTAssertEqual(f.controller.chord(for: .find), KeyChord(key: "f", modifiers: .command), "冲突时不修改")
        XCTAssertEqual(f.controller.chord(for: .findNext), g)

        XCTAssertEqual(f.controller.assignReplacingConflict(g, to: .find), .assigned)
        XCTAssertEqual(f.controller.chord(for: .find), g)
        XCTAssertNil(f.controller.chord(for: .findNext), "被替换的命令不再绑定任何键")
        XCTAssertEqual(item(.findNext, in: f.menu)?.keyEquivalent, "")
        XCTAssertEqual(item(.find, in: f.menu)?.keyEquivalent, "g")
        // 没有任何两个菜单项同键。
        let keys = allItems(f.menu).filter { !$0.keyEquivalent.isEmpty }
            .map { "\($0.keyEquivalent)|\($0.keyEquivalentModifierMask.rawValue)" }
        XCTAssertEqual(Set(keys).count, keys.count)
    }

    func testAssignToSameChordIsNotAConflict() {
        let f = fixture()
        XCTAssertEqual(f.controller.assign(KeyChord(key: "f", modifiers: .command), to: .find), .assigned)
    }

    func testReservedAndInvalidChordsAreRejected() {
        let f = fixture()
        XCTAssertEqual(f.controller.assign(KeyChord(key: "q", modifiers: .command), to: .find), .reserved)
        XCTAssertEqual(f.controller.assign(KeyChord(key: "h", modifiers: .command), to: .find), .reserved)
        XCTAssertEqual(f.controller.assign(KeyChord(key: "x", modifiers: []), to: .find), .invalid)
        XCTAssertEqual(f.controller.assign(KeyChord(key: "x", modifiers: .shift), to: .find), .invalid)
        XCTAssertEqual(f.settings.keybindings, KeybindingSettings())
    }

    func testRestoreDefaultsRestoresWholeTableAndMenu() {
        let f = fixture()
        _ = f.controller.assign(KeyChord(key: "c", modifiers: [.control, .shift]), to: .copy)
        f.controller.clear(.paste)
        _ = f.controller.assignReplacingConflict(KeyChord(key: "g", modifiers: .command), to: .find)
        f.controller.restoreDefaults()
        for command in KeyCommand.allCases {
            XCTAssertEqual(f.controller.chord(for: command), command.defaultChord, "\(command)")
            XCTAssertEqual(item(command, in: f.menu)?.keyEquivalent, command.defaultChord?.menuKeyEquivalent ?? "", "\(command)")
        }
        XCTAssertTrue(f.settings.keybindings.overrides.isEmpty)
    }

    func testResetToDefaultsFromHelpMenuRestoresKeybindingsToo() throws {
        let f = fixture()
        _ = f.controller.assign(KeyChord(key: "c", modifiers: [.control, .shift]), to: .copy)
        try f.settings.resetToDefaults()
        XCTAssertEqual(item(.copy, in: f.menu)?.keyEquivalent, "c")
        XCTAssertTrue(item(.copy, in: f.menu)!.keyEquivalentModifierMask.contains(.command))
    }

    func testUnparsableStoredChordFallsBackToDefault() {
        let backend = MemorySettingsBackend()
        backend.values["keybindings"] = #"{"v":1,"bindings":{"find":"garbage++","copy":"ctrl+shift+c"}}"#
        let controller = KeybindingController(settings: .attached(to: backend), menuProvider: { nil })
        XCTAssertEqual(controller.chord(for: .find), KeyCommand.find.defaultChord)
        XCTAssertEqual(controller.chord(for: .copy), KeyChord(key: "c", modifiers: [.control, .shift]))
    }

    // MARK: 终端按键拦截（terminal.md §8.2 / §9）

    private final class FakeActions: TerminalEditActions {
        var hasSelection = false
        func selectionText() -> String? { hasSelection ? "x" : nil }
        func paste(_ content: TerminalPasteContent) {}
        func selectAll() {}
    }

    func testPlainCtrlAndAltLettersAreNeverIntercepted() {
        let f = fixture()
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        view.editActions = FakeActions()
        // 默认绑定下，Ctrl+C / Ctrl+A / Alt+B 一律不拦截。
        for (char, base, flags) in [("\u{03}", "c", NSEvent.ModifierFlags.control), ("\u{01}", "a", .control),
                                    ("∫", "b", .option), ("\u{0B}", "k", .control)] {
            XCTAssertFalse(f.controller.interceptTerminalKey(event(char, base: base, flags: flags), in: view), base)
        }
        // 用户绑定过的组合会被拦截（菜单没有认领时不能漏给 shell）。
        _ = f.controller.assign(KeyChord(key: "j", modifiers: .control), to: .find)
        XCTAssertTrue(f.controller.interceptTerminalKey(event("\n", base: "j", flags: .control), in: view))
        XCTAssertFalse(f.controller.interceptTerminalKey(event("\u{0B}", base: "k", flags: .control), in: view), "其他 Ctrl+字母仍然不拦截")
    }

    /// 普通 Ctrl+字母设为复制键：有选区时复制并吞掉，没有选区时照常发给程序（Ctrl+C 仍能中断）。
    func testPlainCtrlLetterCopyBindingCopiesOnlyWithSelection() {
        let f = fixture()
        XCTAssertEqual(f.controller.assign(KeyChord(key: "c", modifiers: .control), to: .copy), .assigned)
        // 不写进菜单：菜单会在窗口最前面拦截，没有选区时就无法照常发给程序。
        let copyItem = item(.copy, in: f.menu)!
        XCTAssertEqual(copyItem.keyEquivalent, "")
        XCTAssertTrue(copyItem.keyEquivalentModifierMask.isEmpty)

        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let actions = FakeActions()
        view.editActions = actions
        let ctrlC = event("\u{03}", base: "c", flags: .control)

        actions.hasSelection = false
        XCTAssertFalse(f.controller.interceptTerminalKey(ctrlC, in: view), "没有选区：照常发给程序")
        XCTAssertEqual(f.copied.count, 0)

        actions.hasSelection = true
        XCTAssertTrue(f.controller.interceptTerminalKey(ctrlC, in: view), "有选区：复制并吞掉")
        XCTAssertEqual(f.copied.count, 1)
    }

    /// 复制键绑定为其他组合（非普通 Ctrl+字母）：没有选区时也要吞掉，不能变成多余的控制字符。
    func testOtherCopyBindingIsSwallowedEvenWithoutSelection() {
        let f = fixture()
        _ = f.controller.assign(KeyChord(key: "c", modifiers: [.control, .shift]), to: .copy)
        let view = TerminalView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let actions = FakeActions()
        view.editActions = actions
        let chord = event("C", base: "C", flags: [.control, .shift])
        actions.hasSelection = false
        XCTAssertTrue(f.controller.interceptTerminalKey(chord, in: view))
        XCTAssertEqual(f.copied.count, 0, "拦截点只吞掉按键；复制本身由菜单项（有选区时可用）执行")
        // 菜单项保留了这个组合。
        XCTAssertEqual(item(.copy, in: f.menu)?.keyEquivalentModifierMask.intersection([.control, .shift]), [.control, .shift])
    }

    /// 视图的 keyDown 先问拦截点：被拦截的键不再发给 shell；没被拦截的 Ctrl+字母照常发出。
    func testTerminalViewKeyDownHonorsInterceptor() throws {
        let (view, _) = try InputFixtures.makeView()
        let sink = RecordingSink()
        view.inputSink = sink
        var intercepted = 0
        view.keyInterceptor = { e in
            intercepted += 1
            return e.charactersIgnoringModifiers == "c"
        }
        view.keyDown(with: event("\u{03}", base: "c", flags: .control, code: VK.c))
        XCTAssertTrue(sink.keys.isEmpty, "被拦截的 Ctrl+C 不发给 shell")
        view.keyDown(with: event("\u{01}", base: "a", flags: .control, code: VK.a))
        XCTAssertEqual(sink.keys.count, 1, "未被拦截的 Ctrl+A 照常发出")
        XCTAssertEqual(intercepted, 2)
    }

    // MARK: 录制

    func testRecorderCapturesChordsEscClearsAndCancels() throws {
        let field = KeyChordField()
        var events: [KeyChordField.Event] = []
        field.onEvent = { events.append($0) }

        field.beginRecording()
        XCTAssertTrue(field.isRecording)
        field.handle(event("g", flags: [.command, .shift]))
        XCTAssertEqual(events, [.chord(KeyChord(key: "g", modifiers: [.command, .shift]))])
        XCTAssertFalse(field.isRecording)

        field.beginRecording()
        field.handle(InputFixtures.key(.keyDown, VK.escape, characters: "\u{1b}", flags: []))
        XCTAssertEqual(events.last, .cancel)
        XCTAssertFalse(field.isRecording)

        field.beginRecording()
        field.handle(InputFixtures.key(.keyDown, VK.delete, characters: "\u{7f}", flags: []))
        XCTAssertEqual(events.last, .clear)

        // ⌘⌫ 带修饰键，是普通组合不是"清空"。
        field.beginRecording()
        field.handle(InputFixtures.key(.keyDown, VK.delete, characters: "\u{7f}", flags: .command))
        XCTAssertEqual(events.last, .chord(KeyChord(key: "\u{7f}", modifiers: .command)))
    }

    /// 录制期间 performKeyEquivalent 必须消费按键：否则 ⌘N 会真的新建会话、⌘W 会关窗口。
    func testRecorderSwallowsKeyEquivalentsOnlyWhileRecording() {
        let field = KeyChordField()
        var events: [KeyChordField.Event] = []
        field.onEvent = { events.append($0) }
        let cmdN = event("n", flags: .command)
        XCTAssertFalse(field.performKeyEquivalent(with: cmdN), "未录制时不截获")
        field.beginRecording()
        XCTAssertTrue(field.performKeyEquivalent(with: cmdN))
        XCTAssertEqual(events, [.chord(KeyChord(key: "n", modifiers: .command))])
    }

    func testRecorderCancelsWhenLosingFocus() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 100), styleMask: [.titled],
                              backing: .buffered, defer: false)
        let field = KeyChordField()
        window.contentView = field
        field.beginRecording()
        XCTAssertTrue(field.isRecording)
        XCTAssertTrue(field.resignFirstResponder())
        XCTAssertFalse(field.isRecording)
    }

    // MARK: 设置页

    private func makePage(
        answer: Bool = false
    ) -> (KeyboardSettingsViewController, Fixture, SettingsWindowTests.StubPrompter) {
        let f = fixture()
        let prompter = SettingsWindowTests.StubPrompter()
        prompter.answer = answer
        let page = KeyboardSettingsViewController(keybindings: f.controller, prompter: prompter)
        _ = page.view
        return (page, f, prompter)
    }

    func testPageListsEveryCommandWithItsChordPlusFixedTabRow() {
        let (page, _, _) = makePage()
        XCTAssertEqual(Set(page.fields.keys), Set(KeyCommand.allCases))
        XCTAssertEqual(page.fields[.copy]?.chord?.display, "⌘C")
        XCTAssertEqual(page.fields[.splitDown]?.chord?.display, "⇧⌘\\")
        XCTAssertEqual(page.fixedTabsField.fixedText, "⌘1–⌘9")
        XCTAssertFalse(page.fixedTabsField.isEnabled)
        page.fixedTabsField.beginRecording()
        XCTAssertFalse(page.fixedTabsField.isRecording, "⌘1–⌘9 不可录制")
    }

    func testPageRecordsClearsAndRestores() async {
        let (page, f, _) = makePage()
        let field = page.fields[.newLocalShell]!
        field.beginRecording()
        field.handle(event("y", flags: .command))
        await page.pendingTask?.value
        XCTAssertEqual(f.controller.chord(for: .newLocalShell), KeyChord(key: "y", modifiers: .command))
        XCTAssertEqual(field.chord?.display, "⌘Y")
        XCTAssertEqual(item(.newLocalShell, in: f.menu)?.keyEquivalent, "y")

        field.beginRecording()
        field.handle(InputFixtures.key(.keyDown, VK.delete, characters: "\u{7f}", flags: []))
        await page.pendingTask?.value
        XCTAssertNil(f.controller.chord(for: .newLocalShell))
        XCTAssertNil(field.chord)

        page.restoreButton.performClick(nil)
        XCTAssertEqual(f.controller.chord(for: .newLocalShell), KeyCommand.newLocalShell.defaultChord)
        XCTAssertEqual(field.chord?.display, "⌘T")
    }

    func testPageConflictAsksAndReplaceOrCancel() async {
        // 取消：什么都不变。
        let (page, f, prompter) = makePage(answer: false)
        let field = page.fields[.find]!
        field.beginRecording()
        field.handle(event("g", flags: .command))
        await page.pendingTask?.value
        XCTAssertEqual(prompter.asked.count, 1)
        XCTAssertEqual(prompter.asked.first?.existing, .findNext)
        XCTAssertEqual(prompter.asked.first?.new, .find)
        XCTAssertEqual(f.controller.chord(for: .find), KeyCommand.find.defaultChord)
        XCTAssertEqual(f.controller.chord(for: .findNext), KeyCommand.findNext.defaultChord)
        XCTAssertEqual(field.chord?.display, "⌘F", "取消后录制框回到当前值")
        XCTAssertFalse(field.isRecording)

        // 替换：原命令清空。
        prompter.answer = true
        field.beginRecording()
        field.handle(event("g", flags: .command))
        await page.pendingTask?.value
        XCTAssertEqual(f.controller.chord(for: .find)?.display, "⌘G")
        XCTAssertNil(f.controller.chord(for: .findNext))
        XCTAssertNil(page.fields[.findNext]?.chord)
    }

    func testPageShowsMessageForRejectedChords() async {
        let (page, f, prompter) = makePage()
        let field = page.fields[.find]!
        field.beginRecording()
        field.handle(event("q", flags: .command))
        await page.pendingTask?.value
        XCTAssertNotEqual(page.messageText, "")
        XCTAssertTrue(prompter.asked.isEmpty)
        XCTAssertEqual(f.controller.chord(for: .find), KeyCommand.find.defaultChord)

        field.beginRecording()
        field.handle(event("x", flags: []))
        await page.pendingTask?.value
        XCTAssertEqual(f.controller.chord(for: .find), KeyCommand.find.defaultChord)
    }

    func testResettingSettingsRefreshesOpenPage() async throws {
        let (page, f, _) = makePage()
        _ = f.controller.assign(KeyChord(key: "y", modifiers: .command), to: .find)
        XCTAssertEqual(page.fields[.find]?.chord?.display, "⌘Y")
        try f.settings.resetToDefaults()
        XCTAssertEqual(page.fields[.find]?.chord?.display, "⌘F")
    }
}
