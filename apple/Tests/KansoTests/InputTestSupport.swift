import AppKit
import Carbon.HIToolbox
import KansoBridge
@testable import TerminalKit
import XCTest

/// 记录型 sink：按到达顺序保存所有输出，供断言。
@MainActor
final class RecordingSink: TerminalInputSink {
    enum Event: Equatable {
        case key(TerminalKeyEvent)
        case text(String)
        case mouse(TerminalMouseEvent)
        case scroll(lines: Int32, mods: KeyModifiers)
        case focus(Bool)
    }

    var events: [Event] = []

    func key(_ event: TerminalKeyEvent) { events.append(.key(event)) }
    func textInput(_ text: String) { events.append(.text(text)) }
    func mouse(_ event: TerminalMouseEvent) { events.append(.mouse(event)) }
    func scroll(lines: Int32, at: ViewportPointValue, mods: KeyModifiers) {
        events.append(.scroll(lines: lines, mods: mods))
    }
    func focus(_ focused: Bool) { events.append(.focus(focused)) }

    var keys: [TerminalKeyEvent] { events.compactMap { if case .key(let k) = $0 { k } else { nil } } }
    var texts: [String] { events.compactMap { if case .text(let t) = $0 { t } else { nil } } }
    var mice: [TerminalMouseEvent] { events.compactMap { if case .mouse(let m) = $0 { m } else { nil } } }
}

/// 假输入法：每个事件交给脚本，脚本按真实输入法的回调顺序调用视图的 `NSTextInputClient` 方法，
/// 返回值即 `handleEvent` 的"是否已处理"。
@MainActor
final class FakeInputMethod: TerminalInputMethod {
    var script: (NSEvent) -> Bool = { _ in false }
    var handledEvents: [NSEvent] = []
    var discardCount = 0

    func handleEvent(_ event: NSEvent) -> Bool {
        handledEvents.append(event)
        return script(event)
    }

    func discardMarkedText() { discardCount += 1 }
}

/// macOS 虚拟按键码（`kVK_*`），在测试里集中命名。
enum VK {
    static let a = UInt16(kVK_ANSI_A)
    static let b = UInt16(kVK_ANSI_B)
    static let c = UInt16(kVK_ANSI_C)
    static let i = UInt16(kVK_ANSI_I)
    static let n = UInt16(kVK_ANSI_N)
    static let semicolon = UInt16(kVK_ANSI_Semicolon)
    static let space = UInt16(kVK_Space)
    static let `return` = UInt16(kVK_Return)
    static let delete = UInt16(kVK_Delete)
    static let escape = UInt16(kVK_Escape)
    static let leftArrow = UInt16(kVK_LeftArrow)
    static let f5 = UInt16(kVK_F5)
}

@MainActor
enum InputFixtures {
    /// 构造键盘事件。`characters` 是带修饰键后的结果，`base` 是不带修饰键的字符。
    static func key(
        _ type: NSEvent.EventType = .keyDown, _ code: UInt16, characters: String, base: String? = nil,
        flags: NSEvent.ModifierFlags = [], isRepeat: Bool = false
    ) -> NSEvent {
        NSEvent.keyEvent(
            with: type, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
            characters: characters, charactersIgnoringModifiers: base ?? characters, isARepeat: isRepeat, keyCode: code
        )!
    }

    static func flags(_ flags: NSEvent.ModifierFlags) -> NSEvent {
        NSEvent.keyEvent(
            with: .flagsChanged, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
            characters: "", charactersIgnoringModifiers: "", isARepeat: false, keyCode: UInt16(kVK_Shift)
        )!
    }

    /// 设备相关的左 / 右 Option 位（NX_DEVICELALTKEYMASK / NX_DEVICERALTKEYMASK）。
    static let leftOption = NSEvent.ModifierFlags(rawValue: NSEvent.ModifierFlags.option.rawValue | 0x20)
    static let rightOption = NSEvent.ModifierFlags(rawValue: NSEvent.ModifierFlags.option.rawValue | 0x40)

    /// 一个放进无边框窗口的视图（窗口不显示）。窗口 frame 即内容区，便于独立推算屏幕坐标。
    static func makeView(size: NSSize = NSSize(width: 800, height: 400)) throws -> (TerminalView, NSWindow) {
        let window = NSWindow(contentRect: NSRect(origin: NSPoint(x: 100, y: 200), size: size),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        let view = TerminalView(frame: NSRect(origin: .zero, size: size))
        _ = try XCTUnwrap(view.renderer, "没有可用的 Metal 设备")
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        return (view, window)
    }

    static func mouse(
        _ type: NSEvent.EventType, at point: NSPoint, clickCount: Int = 1, flags: NSEvent.ModifierFlags = []
    ) -> NSEvent {
        NSEvent.mouseEvent(
            with: type, location: point, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
            eventNumber: 0, clickCount: clickCount, pressure: 1
        )!
    }
}
