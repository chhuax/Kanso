//! UI 交给 core 的输入事件：键盘、鼠标、粘贴，以及与输入相关的显示设置。
//!
//! 这里只描述“用户做了什么”。把它翻译成终端字节序列（普通按键、xterm 修饰键、
//! kitty keyboard protocol、鼠标上报、bracketed paste 等）完全由 core 决定，
//! UI 不做任何转义（terminal.md §8、§9）。

use serde::{Deserialize, Serialize};

use crate::geometry::ViewportPoint;

/// 修饰键状态。
///
/// Alt 区分左右，是因为“Option 作为 Meta”可以只对一侧生效（terminal.md §9）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modifiers {
    /// Shift。
    pub shift: bool,
    /// Ctrl。
    pub ctrl: bool,
    /// 左 Alt / 左 Option。
    pub alt_left: bool,
    /// 右 Alt / 右 Option。
    pub alt_right: bool,
    /// Meta：macOS 的 ⌘，Windows 的 Win 键。
    pub meta: bool,
}

impl Modifiers {
    /// 没有任何修饰键。
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt_left: false,
        alt_right: false,
        meta: false,
    };

    /// 任意一侧 Alt 是否按下。
    pub const fn alt(&self) -> bool {
        self.alt_left || self.alt_right
    }
}

/// 物理键（按布局解释后的逻辑键）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    /// 字符键：该键在当前键盘布局下**不带修饰键**时产生的字符。
    /// 用于 Ctrl/Alt 组合和 kitty 协议；带修饰键后实际产生的文本在 [`KeyEvent::text`]。
    Character(String),
    /// 回车。
    Enter,
    /// Tab。
    Tab,
    /// 退格。
    Backspace,
    /// Esc。
    Escape,
    /// 空格。
    Space,
    /// 上箭头。
    ArrowUp,
    /// 下箭头。
    ArrowDown,
    /// 左箭头。
    ArrowLeft,
    /// 右箭头。
    ArrowRight,
    /// Home。
    Home,
    /// End。
    End,
    /// Page Up。
    PageUp,
    /// Page Down。
    PageDown,
    /// Insert。
    Insert,
    /// Delete（向前删除）。
    Delete,
    /// 功能键，取值 1..=24。
    F(u8),
}

/// 按键事件类型。kitty keyboard protocol 需要区分 Release。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyEventKind {
    /// 按下。
    Press,
    /// 按住不放产生的重复。
    Repeat,
    /// 松开。
    Release,
}

/// 一次按键。
///
/// IME 组字期间的按键**不发给 core**；IME 上屏的文字走 `Core::text_input`，
/// 不走 `KeyEvent`（terminal.md §16）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    /// 逻辑键。
    pub key: Key,
    /// 修饰键。
    pub mods: Modifiers,
    /// 系统按当前布局和修饰键产生的文本，例如 Option+e 的组合字符；非文本键为 `None`。
    pub text: Option<String>,
    /// 事件类型。
    pub kind: KeyEventKind,
}

/// 鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    /// 左键。
    Left,
    /// 中键。
    Middle,
    /// 右键。
    Right,
    /// 没有按键（单纯移动）。
    None,
}

/// 鼠标事件类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseEventKind {
    /// 按下。
    Down,
    /// 松开。
    Up,
    /// 按住按键移动。
    Drag,
    /// 未按键移动。
    Move,
}

/// 一次鼠标事件。
///
/// 选区由 core 处理：`click_count` 为 2 选词、3 选行；Option+拖动是矩形选择；
/// 按住 Shift 时，即使程序开启了鼠标上报也由终端自己处理（terminal.md §8.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseEvent {
    /// 事件类型。
    pub kind: MouseEventKind,
    /// 相关按键。
    pub button: MouseButton,
    /// 视口内位置。
    pub point: ViewportPoint,
    /// 修饰键。
    pub mods: Modifiers,
    /// 连击次数（1 起）。
    pub click_count: u8,
}

/// 粘贴内容，由 UI 读剪贴板后归类。
///
/// 转义规则、AI CLI 会话的特殊处理（如图片与文件路径）、是否使用 bracketed paste
/// 全部由 core 决定（terminal.md §8.3），UI 只负责读剪贴板并归类。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteContent {
    /// 文本。
    Text(String),
    /// 文件（本地绝对路径）。
    Files(Vec<String>),
    /// 剪贴板里只有图片、没有文本或文件。
    ImageOnly,
}

/// 时间戳列的显示模式（terminal.md §2）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimestampMode {
    /// 都不显示。
    Off,
    /// 只显示行号。
    LineNumbers,
    /// 只显示时间。
    Time,
    /// 行号和时间都显示（默认）。
    #[default]
    Both,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_alt_and_none() {
        assert!(!Modifiers::NONE.alt());
        assert_eq!(Modifiers::NONE, Modifiers::default());
        let m = Modifiers {
            alt_right: true,
            ..Modifiers::NONE
        };
        assert!(m.alt());
    }

    #[test]
    fn timestamp_mode_default_is_both() {
        assert_eq!(TimestampMode::default(), TimestampMode::Both);
    }
}
