//! 将共享的终端配色目录提供给 Swift 设置与渲染器。

use std::path::Path;

/// 文件主题与已解析的 24 位 sRGB 色值。
#[derive(uniffi::Record)]
pub struct FfiTerminalTheme {
    /// 持久化选择使用的稳定标识。
    pub id: String,
    /// 主题文件给出的显示名称。
    pub name: String,
    /// 默认文字色。
    pub foreground: u32,
    /// 默认底色。
    pub background: u32,
    /// 光标色。
    pub cursor: u32,
    /// 块状光标上的文字色。
    pub cursor_text: u32,
    /// 选区底色。
    pub selection_background: u32,
    /// 标准与高亮 ANSI 色，依次共 16 项。
    pub ansi: Vec<u32>,
    /// 按帧内类别编号排列的 13 个语义色。
    pub semantic: Vec<u32>,
}

/// 扫描主题目录，按显示名返回有效的用户主题。
#[uniffi::export]
pub fn load_terminal_themes(directory: String) -> Vec<FfiTerminalTheme> {
    kanso_core::terminal_themes::load_terminal_themes(Path::new(&directory))
        .into_iter()
        .map(|t| FfiTerminalTheme {
            id: t.id,
            name: t.name,
            foreground: t.foreground,
            background: t.background,
            cursor: t.cursor,
            cursor_text: t.cursor_text,
            selection_background: t.selection_background,
            ansi: t.ansi.to_vec(),
            semantic: t.semantic.to_vec(),
        })
        .collect()
}
