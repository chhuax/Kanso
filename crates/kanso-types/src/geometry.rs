//! 终端网格几何与颜色。

use serde::{Deserialize, Serialize};

/// 终端网格尺寸（以单元格计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridSize {
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
}

/// 会话内累计的绝对行号。
///
/// 回滚缓冲丢弃旧行时它照常增长，不复用、不回绕（terminal.md §2）。用户执行 `clear` 后
/// “显示行号从 1 重新开始”只是显示层的偏移，不改变 `AbsLine`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbsLine(pub u64);

/// 以绝对行号定位的单元格坐标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellPoint {
    /// 绝对行号。
    pub line: AbsLine,
    /// 列号（从 0 起）。
    pub col: u16,
}

/// 24 位 RGB 颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb {
    /// 红。
    pub r: u8,
    /// 绿。
    pub g: u8,
    /// 蓝。
    pub b: u8,
}

/// 终端单元格的颜色。
///
/// 颜色按“索引或 RGB”原样保存，不预先解析成最终像素色：切换主题时只需更换调色板，
/// 不必重写已有内容（architecture §5、§11）。`Default` 的含义由所在位置决定
/// （作前景是默认前景色，作背景是默认背景色）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TermColor {
    /// 默认色，具体取值取决于它出现在前景还是背景。
    Default,
    /// 调色板索引（0..=255）。
    Indexed(u8),
    /// 直接指定的 RGB。
    Rgb(Rgb),
}

/// 光标落在单元格的哪一半。
///
/// 选区端点需要半个单元格的精度：从单元格右半开始拖动与从左半开始，选中的字符不同
/// （terminal.md §8.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CellSide {
    /// 单元格左半。
    Left,
    /// 单元格右半。
    Right,
}

/// 视口内的坐标（相对当前可见区域，不是绝对行号）。
///
/// 鼠标、滚轮事件用它定位；core 自己把它换算成 [`CellPoint`]（绝对行号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewportPoint {
    /// 列号（从 0 起）。
    pub col: u16,
    /// 视口内的行号（从 0 起，0 为可见区域第一行）。
    pub row: u16,
    /// 落在单元格的左半还是右半。
    pub side: CellSide,
}
