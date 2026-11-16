import AppKit
import KansoBridge
@testable import TerminalKit
import XCTest

/// IME 序列测试：不依赖真实输入法，由假输入法按真实输入法的回调顺序驱动 `TerminalView`。
/// 每个用例标注对应 terminal.md §16 的场景编号。
@MainActor
final class InputMethodTests: XCTestCase {
    private var view: TerminalView!
    private var window: NSWindow!
    private var sink: RecordingSink!
    private var ime: FakeInputMethod!

    override func setUpWithError() throws {
        (view, window) = try InputFixtures.makeView()
        sink = RecordingSink()
        ime = FakeInputMethod()
        view.inputSink = sink
        view.inputMethod = ime
    }

    private func down(_ code: UInt16, _ chars: String, base: String? = nil, flags: NSEvent.ModifierFlags = [],
                      isRepeat: Bool = false) {
        view.keyDown(with: InputFixtures.key(.keyDown, code, characters: chars, base: base, flags: flags, isRepeat: isRepeat))
    }

    private func up(_ code: UInt16, _ chars: String, flags: NSEvent.ModifierFlags = []) {
        view.keyUp(with: InputFixtures.key(.keyUp, code, characters: chars, flags: flags))
    }

    /// 没有组字的"输入法英文模式"：直接把按键字符作为 insertText 回调。
    private func installEnglishModeIME() {
        ime.script = { [unowned self] event in
            view.insertText(event.characters ?? "", replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }
    }

    // MARK: 基本输入

    func testPlainEnglishTypingEmitsKeyWithText() {
        installEnglishModeIME()
        down(VK.a, "a")
        XCTAssertEqual(sink.events, [.key(TerminalKeyEvent(key: .character("a"), text: "a"))])
    }

    func testCtrlCAndCtrlABypassInputMethod() {
        // 输入法脚本若被调用，会把 Ctrl+A 翻成 Emacs 编辑命令 moveToBeginningOfLine:；Ctrl 组合必须绕开它。
        ime.script = { [unowned self] _ in
            view.doCommand(by: #selector(NSResponder.moveToBeginningOfLine(_:)))
            return true
        }
        down(VK.c, "\u{03}", base: "c", flags: .control)
        down(VK.a, "\u{01}", base: "a", flags: .control)
        XCTAssertTrue(ime.handledEvents.isEmpty)
        XCTAssertEqual(sink.keys, [
            TerminalKeyEvent(key: .character("c"), mods: .ctrl),
            TerminalKeyEvent(key: .character("a"), mods: .ctrl),
        ])
        XCTAssertTrue(sink.texts.isEmpty)
    }

    func testOptionLetterGoesThroughTextWhenMetaOff() {
        // Option+a 产生 å：按键事件带 text，由 core 按"Option 作为 Meta"设置决定发 ESC a 还是 å。
        installEnglishModeIME()
        down(VK.a, "å", base: "a", flags: InputFixtures.leftOption)
        XCTAssertEqual(sink.keys, [TerminalKeyEvent(key: .character("a"), mods: .altLeft, text: "å")])
    }

    func testLeftAndRightOptionAreDistinguished() {
        installEnglishModeIME()
        down(VK.a, "å", base: "a", flags: InputFixtures.leftOption)
        down(VK.a, "å", base: "a", flags: InputFixtures.rightOption)
        XCTAssertEqual(sink.keys.map(\.mods), [.altLeft, .altRight])
    }

    func testCommandKeyIsNotSent() {
        installEnglishModeIME()
        down(VK.c, "c", flags: .command)
        up(VK.c, "c", flags: .command)
        XCTAssertTrue(sink.events.isEmpty)
        XCTAssertTrue(ime.handledEvents.isEmpty)
    }

    func testKeyUpEmitsReleaseAndRepeatIsMarked() {
        installEnglishModeIME()
        down(VK.a, "a")
        down(VK.a, "a", isRepeat: true)
        up(VK.a, "a")
        XCTAssertEqual(sink.keys.map(\.kind), [.press, .repeat, .release])
    }

    func testDoCommandBuildsKeyFromOriginalEventNotSelector() {
        // 回车：输入法回调 insertNewline:，键由原始事件决定；故意回调一个与键无关的 selector 来证明不按名称推断。
        ime.script = { [unowned self] _ in
            view.doCommand(by: #selector(NSResponder.moveToEndOfLine(_:)))
            return true
        }
        down(VK.return, "\r")
        XCTAssertEqual(sink.keys, [TerminalKeyEvent(key: .enter)])
    }

    func testKeysTheInputMethodIgnoresAreSentAsKeys() {
        ime.script = { _ in false }
        down(VK.f5, "\u{F708}")
        down(VK.leftArrow, "\u{F702}", flags: .option)
        XCTAssertEqual(sink.keys, [
            TerminalKeyEvent(key: .function(5)),
            TerminalKeyEvent(key: .arrowLeft, mods: .altLeft),
        ])
    }

    // MARK: 场景 1：快速输入第二个键不能丢

    func testScenario1_FastTypingInEnglishModeKeepsEveryKey() {
        installEnglishModeIME()
        down(VK.a, "a")
        down(VK.b, "b")
        XCTAssertEqual(sink.keys.map(\.key), [.character("a"), .character("b")])
    }

    func testScenario1_LateCallbackForPreviousKeyDoesNotSwallowNextKey() {
        // 第一个键：输入法声称已处理但回调延后；第二个键处理期间才补上第一个键的 insertText。
        // 两个字符各自只出现一次，不能互相吞掉。
        var firstDone = false
        ime.script = { [unowned self] event in
            if event.characters == "a" { return true }
            if !firstDone {
                firstDone = true
                view.insertText("a", replacementRange: NSRange(location: NSNotFound, length: 0))
            }
            view.insertText("b", replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }
        down(VK.a, "a")
        down(VK.b, "b")
        let sent = sink.events.map { e -> String in
            switch e {
            case .key(let k): k.text ?? "?"
            case .text(let t): t
            default: "?"
            }
        }
        XCTAssertEqual(sent.sorted(), ["a", "b"])
    }

    // MARK: 场景 2：Shift 后的第一个全角标点不能丢

    func testScenario2_ShiftFlagsChangedDoesNotResetStateAndPunctuationIsSent() {
        var flagsSeen = 0
        ime.script = { [unowned self] event in
            if event.type == .flagsChanged { flagsSeen += 1; return false }
            // 中文输入法把 Shift+; 的半角 ":" 转成全角 "："。
            view.insertText("：", replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }
        view.flagsChanged(with: InputFixtures.flags(.shift))
        down(VK.semicolon, ":", base: ";", flags: .shift)
        view.flagsChanged(with: InputFixtures.flags([]))
        XCTAssertEqual(flagsSeen, 2, "flagsChanged 必须转给输入法")
        XCTAssertEqual(sink.events, [.text("：")])
    }

    // MARK: 场景 3：空格确认词组不能重复上屏

    func testScenario3_SpaceConfirmingCompositionCommitsOnceAndSpaceIsNotSent() {
        ime.script = { [unowned self] event in
            switch event.keyCode {
            case VK.n: view.setMarkedText("n", selectedRange: NSRange(location: 1, length: 0), replacementRange: .init(location: NSNotFound, length: 0))
            case VK.i: view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: .init(location: NSNotFound, length: 0))
            case VK.space: view.insertText("你", replacementRange: .init(location: NSNotFound, length: 0))
            default: return false
            }
            return true
        }
        down(VK.n, "n"); down(VK.i, "i"); down(VK.space, " ")
        up(VK.space, " ")
        XCTAssertEqual(sink.events, [.text("你")])
        XCTAssertFalse(view.hasMarkedText())
    }

    // MARK: 场景 4：两次组字先后完成都要上屏

    func testScenario4_TwoQuickIdenticalCompositionsBothCommit() {
        ime.script = { [unowned self] event in
            let none = NSRange(location: NSNotFound, length: 0)
            switch event.keyCode {
            case VK.n: view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
            case VK.space: view.insertText("你", replacementRange: none)
            default: return false
            }
            return true
        }
        down(VK.n, "n"); down(VK.space, " ")
        down(VK.n, "n"); down(VK.space, " ")
        XCTAssertEqual(sink.texts, ["你", "你"], "不能用内容去重：两次相同的上屏都必须发送")
    }

    // MARK: 场景 5：已上屏内容不重复发送

    func testScenario5_UnmarkTextAfterInsertTextSendsNothingTwice() {
        let none = NSRange(location: NSNotFound, length: 0)
        view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
        XCTAssertTrue(sink.events.isEmpty, "setMarkedText 只更新显示")
        XCTAssertTrue(view.hasMarkedText())
        view.insertText("你", replacementRange: none)
        // insertText 已清空组字，紧接着的 unmarkText 不能再发一遍。
        view.unmarkText()
        XCTAssertEqual(sink.events, [.text("你")])
        // 组字已结束后再调 unmarkText 也不发送。
        view.unmarkText()
        XCTAssertEqual(sink.events, [.text("你")])
    }

    /// 只靠 unmarkText 提交的输入法（例如组字中途切走焦点）：未上屏的组字必须作为文字发出，不能丢。
    func testUnmarkTextCommitsPendingMarkedText() {
        let none = NSRange(location: NSNotFound, length: 0)
        view.setMarkedText("nihao", selectedRange: NSRange(location: 5, length: 0), replacementRange: none)
        XCTAssertTrue(sink.events.isEmpty)
        view.unmarkText()
        XCTAssertEqual(sink.events, [.text("nihao")])
        XCTAssertFalse(view.hasMarkedText())
    }

    // MARK: 场景 6：组字期间回车 / 退格 / Esc 归输入法

    func testScenario6_ReturnBackspaceEscapeDuringCompositionAreNotSentToShell() {
        let none = NSRange(location: NSNotFound, length: 0)
        view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)

        // 退格：输入法缩短组字。
        ime.script = { [unowned self] _ in
            view.setMarkedText("n", selectedRange: NSRange(location: 1, length: 0), replacementRange: none)
            return true
        }
        down(VK.delete, "\u{7F}")
        XCTAssertTrue(sink.events.isEmpty)

        // Esc：输入法取消组字。
        ime.script = { [unowned self] _ in
            view.setMarkedText("", selectedRange: NSRange(location: 0, length: 0), replacementRange: none)
            return true
        }
        down(VK.escape, "\u{1B}")
        XCTAssertTrue(sink.events.isEmpty)
        XCTAssertFalse(view.hasMarkedText())

        // 回车：输入法把原始字母上屏，回车本身不发。
        view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
        ime.script = { [unowned self] _ in
            view.insertText("ni", replacementRange: none)
            return true
        }
        down(VK.return, "\r")
        XCTAssertEqual(sink.events, [.text("ni")])
    }

    func testScenario6_KeysIgnoredOrCommandedByInputMethodDuringCompositionAreDropped() {
        let none = NSRange(location: NSNotFound, length: 0)
        view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
        // 输入法不处理，或回调 doCommand：组字期间都不能漏给 shell。
        ime.script = { _ in false }
        down(VK.return, "\r")
        ime.script = { [unowned self] _ in
            view.doCommand(by: #selector(NSResponder.deleteBackward(_:)))
            return true
        }
        down(VK.delete, "\u{7F}")
        XCTAssertTrue(sink.events.isEmpty)
        // 组字期间的松开也不发 release（没有对应的 press）。
        up(VK.return, "\r")
        XCTAssertTrue(sink.events.isEmpty)
    }

    func testResigningFirstResponderDiscardsMarkedTextWithoutSending() {
        let none = NSRange(location: NSNotFound, length: 0)
        XCTAssertTrue(window.makeFirstResponder(view))
        view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
        XCTAssertTrue(window.makeFirstResponder(nil))
        XCTAssertFalse(view.hasMarkedText())
        XCTAssertEqual(ime.discardCount, 1)
        XCTAssertTrue(sink.texts.isEmpty)
    }

    // MARK: NSTextInputClient 其余语义

    func testClientQueries() {
        XCTAssertEqual(view.markedRange().location, NSNotFound)
        XCTAssertEqual(view.selectedRange(), NSRange(location: 0, length: 0))
        view.setMarkedText(NSAttributedString(string: "你好"), selectedRange: NSRange(location: 1, length: 1),
                           replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(view.markedRange(), NSRange(location: 0, length: 2))
        XCTAssertEqual(view.selectedRange(), NSRange(location: 2, length: 0))
        XCTAssertNil(view.attributedSubstring(forProposedRange: NSRange(location: 0, length: 1), actualRange: nil))
        XCTAssertTrue(view.validAttributesForMarkedText().isEmpty)
        XCTAssertEqual(view.characterIndex(for: .zero), NSNotFound)
    }
}
