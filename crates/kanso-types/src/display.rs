//! 显示设置里需要下发给终端内核的选项（terminal.md §15）。

/// 默认光标形状：程序没有用 DECSCUSR 指定样式时使用（DECSCUSR 0 也回到它）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    /// 块状。
    #[default]
    Block,
    /// 下划线。
    Underline,
    /// 竖线。
    Bar,
}

/// 下发给所有终端会话的显示选项。
///
/// 字体、字号、配色等纯渲染选项不在这里，它们只存在于 UI 层；这里只放内核必须知道的：
/// 回滚上限决定内核保留多少行，默认光标样式决定帧里的光标形状与闪烁标志，语义着色决定帧里的类别标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalOptions {
    /// 回滚行数上限（不含屏幕）。调小时内核立即丢弃超出的最老的行。
    pub scrollback_lines: u32,
    /// 默认光标形状。
    pub cursor_shape: CursorShape,
    /// 默认光标是否闪烁。
    pub cursor_blink: bool,
    /// 是否为没有 ANSI 前景色的输出叠加语义颜色。
    pub semantic_coloring: bool,
}

impl TerminalOptions {
    /// 回滚行数的默认值（与会话创建时的初始值一致）。
    pub const DEFAULT_SCROLLBACK_LINES: u32 = 10_000;
}

impl Default for TerminalOptions {
    /// 回滚 10 000 行、块状、不闪烁。
    fn default() -> Self {
        TerminalOptions {
            scrollback_lines: Self::DEFAULT_SCROLLBACK_LINES,
            cursor_shape: CursorShape::Block,
            cursor_blink: false,
            semantic_coloring: true,
        }
    }
}
