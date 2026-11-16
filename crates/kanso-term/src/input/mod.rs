//! 输入编码：把 UI 交来的键盘、鼠标、滚轮、粘贴事件，翻译成发往 PTY / SSH 通道的字节
//! （architecture.md §5、terminal.md §8、§9）。
//!
//! 全部是 sans-IO 纯函数：只看事件和 [`TermModes`](crate::TermModes)，不持有状态、不读时钟。
//! 调用方（会话层）负责在调用前处理好「不该发给 shell 的输入」：IME 组字期间的按键、
//! 文件传输期间的输入、会话已结束的锁定（terminal.md §1、§13、§16）。
//!
//! 这里编码出来的字节**才算用户输入**；终端自己的回应走 `Terminal::take_replies`，
//! 不经过本模块（terminal.md §5.2）。
//!
//! 一手出处：xterm「XTerm Control Sequences」、kitty keyboard protocol 规范文档、ECMA-48。
//! 逐项映射表与取舍见 `docs/input-encoding.md`。

mod key;
mod mouse;
mod paste;

#[cfg(test)]
mod tests;

pub use key::encode_key;
pub use mouse::{ScrollAction, encode_mouse, encode_scroll};
pub use paste::{ShellFlavor, encode_paste, quote_paths};

/// 运行平台。影响 Alt / Option 的解释和 Option+方向键的单词跳转（terminal.md §9）。
///
/// 由 UI 层按编译目标传入，而不是在这里用 `cfg!` 判断：这样两种平台的行为都能在同一台机器上测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// macOS：Option 默认产生特殊字符，只有设置为 Meta 的一侧才发 `ESC` 前缀。
    Mac,
    /// Windows / Linux：Alt 一律是 Meta。
    Other,
}

/// macOS 上「Option 作为 Meta 键」的设置（terminal.md §9）。非 mac 平台忽略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsMeta {
    /// 关闭：Option 产生系统组合字符（默认，保持 macOS 输入特殊字符的习惯）。
    #[default]
    Off,
    /// 仅左 Option 作为 Meta。
    Left,
    /// 仅右 Option 作为 Meta。
    Right,
    /// 两侧都作为 Meta。
    Both,
}

/// 按键编码选项。由 UI 从设置里组装，随每次调用传入（不缓存，设置变化立即生效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyOptions {
    /// 运行平台。
    pub platform: Platform,
    /// Option 作为 Meta 的设置，仅 [`Platform::Mac`] 使用。
    pub option_as_meta: OptionAsMeta,
}
