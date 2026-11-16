import AppKit
import Carbon.HIToolbox

// NSEvent 到 `TerminalKeyEvent` 的映射。只用 Apple 文档中的 API 与 `kVK_*` 常量
// （Carbon.HIToolbox 的 Events.h），不依赖任何第三方终端的实现。

extension KeyModifiers {
    /// 设备相关的左 / 右 Option 位：`NX_DEVICELALTKEYMASK`（0x20）、`NX_DEVICERALTKEYMASK`（0x40）。
    ///
    /// 出处：IOKit 的 `IOLLEvent.h`（`NX_DEVICELALTKEYMASK` / `NX_DEVICERALTKEYMASK`）。
    /// 这两位放在 `NSEvent.modifierFlags.rawValue` 的低 16 位（设备相关部分），
    /// AppKit 没有为它们提供公开的 `NSEvent.ModifierFlags` 成员，只能按位读取。
    static let deviceLeftAlt: UInt = 0x20
    static let deviceRightAlt: UInt = 0x40

    init(_ flags: NSEvent.ModifierFlags) {
        var mods: KeyModifiers = []
        if flags.contains(.shift) { mods.insert(.shift) }
        if flags.contains(.control) { mods.insert(.ctrl) }
        if flags.contains(.command) { mods.insert(.meta) }
        if flags.contains(.option) {
            let raw = flags.rawValue
            let left = raw & Self.deviceLeftAlt != 0
            let right = raw & Self.deviceRightAlt != 0
            // 合成事件（测试、辅助功能工具）可能只带 .option 而没有设备位：此时无法区分左右，
            // 按左 Option 处理，保证"Option 按下"这一事实不丢。
            if left || !right { mods.insert(.altLeft) }
            if right { mods.insert(.altRight) }
        }
        self = mods
    }
}

extension TerminalKeyEvent {
    /// 由 keyDown / keyUp 事件构造；无法映射（没有字符也不是已知功能键）时返回 nil。
    init?(event: NSEvent, kind: KeyEventKind) {
        guard event.type == .keyDown || event.type == .keyUp else { return nil }
        let mods = KeyModifiers(event.modifierFlags)

        var isTextKey = false
        let key: TerminalKey
        if let special = Self.specialKey(forKeyCode: Int(event.keyCode)) {
            key = special
            isTextKey = special == .space
        } else {
            // 不带修饰键时该键产生的字符：Ctrl/Alt 组合与 kitty 协议需要"物理键本身"而不是修饰后的结果。
            let base = event.characters(byApplyingModifiers: [])
                ?? event.charactersIgnoringModifiers ?? ""
            guard !base.isEmpty, !Self.isNonText(base) else { return nil }
            key = .character(base)
            isTextKey = true
        }

        var text: String?
        if isTextKey, let characters = event.characters, !characters.isEmpty, !Self.isNonText(characters) {
            text = characters
        }
        self.init(key: key, mods: mods, text: text, kind: kind)
    }

    /// 全是控制字符（Ctrl+C 产生 U+0003）或 AppKit 为功能键编的私有区字符（U+F700...U+F8FF）的字符串
    /// 不是"文本"：前者由 core 的按键编码器按 `key + mods` 生成，后者纯属 AppKit 内部约定。
    private static func isNonText(_ s: String) -> Bool {
        s.unicodeScalars.allSatisfy { $0.value < 0x20 || $0.value == 0x7F || (0xF700...0xF8FF).contains($0.value) }
    }

    /// 按键码 → 非字符键。按键码与布局无关，所以功能键不会受输入源影响。
    static func specialKey(forKeyCode code: Int) -> TerminalKey? {
        switch code {
        case kVK_Return, kVK_ANSI_KeypadEnter: return .enter
        case kVK_Tab: return .tab
        case kVK_Delete: return .backspace
        case kVK_Escape: return .escape
        case kVK_Space: return .space
        case kVK_UpArrow: return .arrowUp
        case kVK_DownArrow: return .arrowDown
        case kVK_LeftArrow: return .arrowLeft
        case kVK_RightArrow: return .arrowRight
        case kVK_Home: return .home
        case kVK_End: return .end
        case kVK_PageUp: return .pageUp
        case kVK_PageDown: return .pageDown
        // Mac 键盘没有 Insert，对应位置的键是 Help；外接 PC 键盘的 Insert 也上报为 kVK_Help。
        case kVK_Help: return .insert
        case kVK_ForwardDelete: return .delete
        case kVK_F1: return .function(1)
        case kVK_F2: return .function(2)
        case kVK_F3: return .function(3)
        case kVK_F4: return .function(4)
        case kVK_F5: return .function(5)
        case kVK_F6: return .function(6)
        case kVK_F7: return .function(7)
        case kVK_F8: return .function(8)
        case kVK_F9: return .function(9)
        case kVK_F10: return .function(10)
        case kVK_F11: return .function(11)
        case kVK_F12: return .function(12)
        case kVK_F13: return .function(13)
        case kVK_F14: return .function(14)
        case kVK_F15: return .function(15)
        case kVK_F16: return .function(16)
        case kVK_F17: return .function(17)
        case kVK_F18: return .function(18)
        case kVK_F19: return .function(19)
        case kVK_F20: return .function(20)
        default: return nil
        }
    }
}
