//! 帧缓冲区的布局结构与常量。
//!
//! 所有结构 `#[repr(C)]` 且无隐式填充（用显式 `_reserved` 字段占位），整数为本机字节序
//! （支持 aarch64 与 x86_64，均为小端）。改动任何字段的位置、尺寸或含义都必须升 [`FRAME_VERSION`]，
//! 并同步 `kanso_frame.h` 与 `docs/frame-abi.md`。

use bytemuck::{Pod, Zeroable};
use core::mem::{align_of, offset_of, size_of};

/// `FrameHeader::magic`：本机字节序下读出来是 ASCII "KFRM"。
pub const FRAME_MAGIC: u32 = 0x4D52_464B;
/// 当前布局版本。
pub const FRAME_VERSION: u16 = 1;

/// 全屏程序期间：时间戳栏留空、不画分隔线、不做语义着色。
pub const FRAME_ALT_SCREEN: u32 = 1 << 0;
/// 光标可见（光标不在视口内时 core 必须清掉此位）。
pub const FRAME_CURSOR_VISIBLE: u32 = 1 << 1;
/// 光标闪烁。
pub const FRAME_CURSOR_BLINK: u32 = 1 << 2;
/// 程序开启了鼠标上报；UI 据此决定点击交给程序还是按住 Shift 绕过（terminal.md §8.1）。
pub const FRAME_MOUSE_REPORTING: u32 = 1 << 3;
/// 终端当前拥有键盘焦点。
pub const FRAME_FOCUSED: u32 = 1 << 4;

/// 光标形状：块。
pub const CURSOR_BLOCK: u8 = 0;
/// 光标形状：下划线。
pub const CURSOR_UNDERLINE: u8 = 1;
/// 光标形状：竖线。
pub const CURSOR_BAR: u8 = 2;
/// 光标形状：空心块（失焦）。
pub const CURSOR_HOLLOW_BLOCK: u8 = 3;

/// 本行软换行到下一行。
pub const LINE_WRAPPED: u32 = 1 << 0;
/// OSC 133 A 所在行；分隔线画在其上方，同一行只标一次（terminal.md §4）。
pub const LINE_PROMPT: u32 = 1 << 1;
/// 光标所在行，时间戳栏高亮。
pub const LINE_CURSOR: u32 = 1 << 2;
/// 从未写过的行，不显示时间和行号。
pub const LINE_BLANK: u32 = 1 << 3;
/// 自上一帧以来有变化，渲染器可只重传这些行。
pub const LINE_DIRTY: u32 = 1 << 4;
/// 会话第一个提示符，上方不画分隔线（local-shell.md §5.2）。
pub const LINE_FIRST_PROMPT: u32 = 1 << 5;

/// 粗体。
pub const CELL_BOLD: u16 = 1 << 0;
/// 暗淡。
pub const CELL_DIM: u16 = 1 << 1;
/// 斜体。
pub const CELL_ITALIC: u16 = 1 << 2;
/// 下划线样式在 flags 中的起始位。
pub const CELL_UNDERLINE_SHIFT: u16 = 3;
/// 下划线样式掩码（bits 3..=5），取值见 `UNDERLINE_*`。
pub const CELL_UNDERLINE_MASK: u16 = 0b111 << CELL_UNDERLINE_SHIFT;
/// 删除线。
pub const CELL_STRIKE: u16 = 1 << 6;
/// 反色。
pub const CELL_INVERSE: u16 = 1 << 7;
/// 隐藏（不绘制字形）。
pub const CELL_HIDDEN: u16 = 1 << 8;
/// 宽字符前半。
pub const CELL_WIDE: u16 = 1 << 9;
/// 宽字符后半占位格（`ch` 为 0）。
pub const CELL_WIDE_SPACER: u16 = 1 << 10;
/// 属于选区；由 core 在生成快照时直接标注，渲染器不再计算区间。
pub const CELL_SELECTED: u16 = 1 << 11;
/// 搜索匹配。
pub const CELL_MATCH: u16 = 1 << 12;
/// 当前匹配项（同时也带 [`CELL_MATCH`]）。
pub const CELL_MATCH_CURRENT: u16 = 1 << 13;
/// 闪烁文字。
pub const CELL_BLINK: u16 = 1 << 14;
/// 提示符上下文标签。
pub const CELL_CHIP: u16 = 1 << 15;

/// 无下划线。
pub const UNDERLINE_NONE: u16 = 0;
/// 单线。
pub const UNDERLINE_SINGLE: u16 = 1;
/// 双线。
pub const UNDERLINE_DOUBLE: u16 = 2;
/// 波浪线。
pub const UNDERLINE_CURLY: u16 = 3;
/// 点线。
pub const UNDERLINE_DOTTED: u16 = 4;
/// 虚线。
pub const UNDERLINE_DASHED: u16 = 5;

/// 颜色标签：默认色。
pub const COLOR_TAG_DEFAULT: u32 = 0;
/// 颜色标签：调色板索引（低 8 位）。
pub const COLOR_TAG_INDEXED: u32 = 1;
/// 颜色标签：RGB（低 24 位 0xRRGGBB）。
pub const COLOR_TAG_RGB: u32 = 2;
/// 颜色标签：语义着色类别（低 8 位）。只出现在 fg 上，且只在程序没设前景色时由 core 填入。
pub const COLOR_TAG_SEMANTIC: u32 = 3;
/// 颜色标签所在的位移（高 8 位）。
pub const COLOR_TAG_SHIFT: u32 = 24;

/// 单帧单元格总数上限。
///
/// 让所有偏移与 `total_len` 在 `u32` 内可表示（上限下单元格段 256 MiB），
/// 使 `FrameBuilder::finish` 不需要处理溢出。
pub const MAX_CELLS: usize = 1 << 24;
/// 字符串池字节数上限，理由同 [`MAX_CELLS`]。
pub const MAX_POOL_LEN: usize = 1 << 24;
/// `CellExtra` 数量上限；`Cell::extra` 为 `u16` 且 0 表示无，所以最大索引是 `u16::MAX`。
pub const MAX_EXTRAS: usize = u16::MAX as usize;

/// 帧头，位于缓冲区起点。
///
/// 以后只允许在末尾追加字段（读取方按 `header_size` 兼容旧版本）。
#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
pub struct FrameHeader {
    /// 固定为 [`FRAME_MAGIC`]。
    pub magic: u32,
    /// 布局版本，见 [`FRAME_VERSION`]。
    pub version: u16,
    /// 写入方使用的帧头字节数。
    pub header_size: u16,
    /// 帧序号，同一终端单调递增。
    pub seq: u64,
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
    /// `FRAME_*` 标志位。
    pub flags: u32,
    /// 视口第 0 行的绝对行号（`kanso_types::AbsLine` 的值）。
    pub first_abs_line: u64,
    /// 显示行号 = abs_line − line_number_base + 1；`clear` 后移动 base 而不让 AbsLine 回退。
    pub line_number_base: u64,
    /// 用户向上滚动了多少行，0 表示在底部。
    pub display_offset: u32,
    /// 当前回滚总行数（滚动条用）。
    pub scrollback_lines: u32,
    /// 光标列（视口内坐标）。
    pub cursor_col: u16,
    /// 光标行（视口内坐标）。
    pub cursor_row: u16,
    /// 光标形状，取值见 `CURSOR_*`。
    pub cursor_shape: u8,
    /// 保留，写 0。
    pub _reserved0: [u8; 3],
    /// `LineMeta` 段偏移（字节，相对缓冲区起点，下同）。
    pub line_meta_offset: u32,
    /// `Cell` 段偏移，共 `rows * cols` 个，行优先。
    pub cells_offset: u32,
    /// `CellExtra` 段偏移。
    pub extras_offset: u32,
    /// `CellExtra` 个数。
    pub extras_count: u32,
    /// 字符串池偏移。
    pub pool_offset: u32,
    /// 字符串池有效字节数（不含末尾对齐填充）。
    pub pool_len: u32,
    /// 整个缓冲区字节数（含对齐填充）。
    pub total_len: u32,
    /// 保留，写 0。
    pub _reserved1: u32,
}

/// 行元数据，每个视口行一个。
#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
pub struct LineMeta {
    /// 该行的绝对行号。
    pub abs_line: u64,
    /// 该行首次写出时的 Unix 毫秒；0 表示无（例如从未写过的行）。
    pub timestamp_ms: i64,
    /// `LINE_*` 标志位。
    pub flags: u32,
    /// 保留，写 0。
    pub _reserved: u32,
}

/// 单元格。
#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
pub struct Cell {
    /// Unicode 标量值；0 表示空单元格（宽字符的占位格也是 0）。
    pub ch: u32,
    /// 前景色，编码见 [`crate::Color`]。
    pub fg: u32,
    /// 背景色，编码见 [`crate::Color`]。
    pub bg: u32,
    /// `CELL_*` 标志位。
    pub flags: u16,
    /// 0 表示无附加信息；n 表示 `extras[n-1]`。
    pub extra: u16,
}

/// 单元格的附加信息：字形簇、超链接、下划线颜色。
#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy, Debug)]
pub struct CellExtra {
    /// 字形簇完整文本在字符串池内的偏移（相对池起点，UTF-8）。
    pub grapheme_offset: u32,
    /// 字形簇字节数；0 表示无，单元格只有 `ch` 一个码点。
    pub grapheme_len: u32,
    /// OSC 8 链接 id，0 表示无；URL 由控制面按 id 查询，不放进帧里。
    pub hyperlink_id: u32,
    /// 下划线颜色（同颜色编码）；0 表示跟随前景。
    pub underline_color: u32,
}

/// 帧头字节数。
pub const HEADER_SIZE: usize = size_of::<FrameHeader>();

// 尺寸与偏移是跨语言契约：C 头文件里有同样的断言，任何一侧变动都会编译失败。
const _: () = assert!(size_of::<FrameHeader>() == 88 && align_of::<FrameHeader>() == 8);
const _: () = assert!(size_of::<LineMeta>() == 24 && align_of::<LineMeta>() == 8);
const _: () = assert!(size_of::<Cell>() == 16 && align_of::<Cell>() == 4);
const _: () = assert!(size_of::<CellExtra>() == 16 && align_of::<CellExtra>() == 4);

const _: () = assert!(offset_of!(FrameHeader, magic) == 0);
const _: () = assert!(offset_of!(FrameHeader, version) == 4);
const _: () = assert!(offset_of!(FrameHeader, header_size) == 6);
const _: () = assert!(offset_of!(FrameHeader, seq) == 8);
const _: () = assert!(offset_of!(FrameHeader, cols) == 16);
const _: () = assert!(offset_of!(FrameHeader, rows) == 18);
const _: () = assert!(offset_of!(FrameHeader, flags) == 20);
const _: () = assert!(offset_of!(FrameHeader, first_abs_line) == 24);
const _: () = assert!(offset_of!(FrameHeader, line_number_base) == 32);
const _: () = assert!(offset_of!(FrameHeader, display_offset) == 40);
const _: () = assert!(offset_of!(FrameHeader, scrollback_lines) == 44);
const _: () = assert!(offset_of!(FrameHeader, cursor_col) == 48);
const _: () = assert!(offset_of!(FrameHeader, cursor_row) == 50);
const _: () = assert!(offset_of!(FrameHeader, cursor_shape) == 52);
const _: () = assert!(offset_of!(FrameHeader, line_meta_offset) == 56);
const _: () = assert!(offset_of!(FrameHeader, cells_offset) == 60);
const _: () = assert!(offset_of!(FrameHeader, extras_offset) == 64);
const _: () = assert!(offset_of!(FrameHeader, extras_count) == 68);
const _: () = assert!(offset_of!(FrameHeader, pool_offset) == 72);
const _: () = assert!(offset_of!(FrameHeader, pool_len) == 76);
const _: () = assert!(offset_of!(FrameHeader, total_len) == 80);

const _: () = assert!(offset_of!(LineMeta, abs_line) == 0);
const _: () = assert!(offset_of!(LineMeta, timestamp_ms) == 8);
const _: () = assert!(offset_of!(LineMeta, flags) == 16);

const _: () = assert!(offset_of!(Cell, ch) == 0);
const _: () = assert!(offset_of!(Cell, fg) == 4);
const _: () = assert!(offset_of!(Cell, bg) == 8);
const _: () = assert!(offset_of!(Cell, flags) == 12);
const _: () = assert!(offset_of!(Cell, extra) == 14);

const _: () = assert!(offset_of!(CellExtra, grapheme_offset) == 0);
const _: () = assert!(offset_of!(CellExtra, grapheme_len) == 4);
const _: () = assert!(offset_of!(CellExtra, hyperlink_id) == 8);
const _: () = assert!(offset_of!(CellExtra, underline_color) == 12);
