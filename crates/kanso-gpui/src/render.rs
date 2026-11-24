//! 核心帧快照 → 渲染数据。
//!
//! 输入是 `kanso-frame` 的 [`FrameView`]（只读，格式见 docs/frame-abi.md），输出是与图形库无关的
//! [`RenderGrid`]：背景矩形、文字片段、光标。这里不碰任何窗口 / 字体 API，因此可以在 CI 里测试；
//! GPUI 侧只负责把这些数据画出来。
//!
//! 排版策略（避免宽字符把后面的列挤歪）：
//! - 普通单宽度字符（每个单元格恰好一个码点）会合并成一个片段，画的时候按单元格宽度强制定位；
//! - 宽字符（CJK 等）与多码点字形簇各自单独成片段，从自己的列起点开始画；
//! - 空白单元格不进片段（背景由矩形负责）。

use kanso_frame::{
    CELL_BOLD, CELL_CHIP, CELL_DIM, CELL_HIDDEN, CELL_INVERSE, CELL_ITALIC, CELL_MATCH,
    CELL_MATCH_CURRENT, CELL_SELECTED, CELL_STRIKE, CELL_UNDERLINE_MASK, CELL_UNDERLINE_SHIFT,
    CELL_WIDE, CELL_WIDE_SPACER, CURSOR_BAR, CURSOR_BLOCK, CURSOR_UNDERLINE, Cell, Color,
    CommandBlock, DecodedColor, FRAME_ALT_SCREEN, FRAME_CURSOR_VISIBLE, FRAME_MOUSE_REPORTING,
    FrameView, LINE_BLANK, LINE_CURSOR, LINE_FIRST_PROMPT, LINE_PROMPT, LINE_WRAPPED,
};
use kanso_types::geometry::Rgb;

use crate::app::links::LineText;
use crate::contrast::ContrastGuard;
use crate::theme::{Palette, Rgb8};

/// 背景矩形：同一行内连续、同色的单元格合并成一块。默认背景色不产生矩形。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BgRect {
    /// 行。
    pub row: u16,
    /// 起始列。
    pub col: u16,
    /// 占几列。
    pub cols: u16,
    /// 颜色。
    pub color: Rgb8,
}

/// 文字片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
    /// 行。
    pub row: u16,
    /// 起始列。
    pub col: u16,
    /// 占几列（显示宽度）。
    pub cells: u16,
    /// 文本。`simple` 为真时每个字符恰好占一列。
    pub text: String,
    /// 是否为“每字符一列”的普通片段（可用等宽强制定位）。
    pub simple: bool,
    /// 前景色（已处理反色 / 暗淡 / 隐藏）。
    pub fg: Rgb8,
    /// 粗体。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
    /// 下划线样式（`UNDERLINE_*`，0 为无）。
    pub underline: u16,
    /// 下划线颜色，`None` 跟随前景。
    pub underline_color: Option<Rgb8>,
    /// 删除线。
    pub strike: bool,
}

/// 光标形状。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    /// 实心块（已经把格子的前景 / 背景换成光标色，画的时候无需额外处理）。
    Block,
    /// 下划线。
    Underline,
    /// 竖线。
    Bar,
    /// 空心块（窗口失焦时）。
    Hollow,
}

/// 需要叠加绘制的光标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorDraw {
    /// 行。
    pub row: u16,
    /// 列。
    pub col: u16,
    /// 占几列（光标落在宽字符上时为 2）。
    pub cells: u16,
    /// 形状。
    pub kind: CursorKind,
    /// 颜色。
    pub color: Rgb8,
}

/// 哪些提示符标签保留边框和目录粗体。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveChips {
    /// 没有命令块可对照（测试、尚未分块的会话）：全部按当前提示符画。
    All,
    /// 只有绝对行落在此闭区间内的标签保留边框和粗体。
    Only {
        /// 起始绝对行，含本行。
        start: u64,
        /// 结束绝对行，含本行。
        end: u64,
    },
}

impl LiveChips {
    /// 最新且尚未执行命令的块是当前待输入提示符；没有这种块时，可见标签都按历史提示符弱化。
    pub fn from_blocks(blocks: &[CommandBlock]) -> Self {
        if blocks.is_empty() {
            return Self::All;
        }
        match blocks
            .iter()
            .find(|b| b.latest && !b.running && b.command_line.is_none())
        {
            Some(b) => Self::Only {
                start: b.start_line,
                end: b.end_line,
            },
            None => Self::Only { start: 1, end: 0 },
        }
    }

    fn contains(self, abs: u64) -> bool {
        match self {
            Self::All => true,
            Self::Only { start, end } => start <= abs && abs <= end,
        }
    }
}

/// 这一绝对行是不是已经执行过的提示符（不含命令和输出）。
///
/// 当前待输入的块不算。命令和命令输出留在原字号、原颜色。
pub fn historical_prompt(blocks: &[CommandBlock], abs: u64) -> bool {
    if blocks.is_empty() {
        return false;
    }
    let live = blocks
        .iter()
        .find(|b| b.latest && !b.running && b.command_line.is_none());
    blocks.iter().any(|b| {
        if live.is_some_and(|live| live.id == b.id) {
            return false;
        }
        match b.command_line {
            Some(command) => b.start_line <= abs && abs < command,
            None => b.start_line <= abs && abs <= b.end_line,
        }
    })
}

/// 私有区里的 Powerline / Nerd Font 图标。历史提示符不画它们，避免依赖特殊字体。
pub fn is_special_symbol(ch: u32) -> bool {
    (0xE000..=0xF8FF).contains(&ch)
        || (0xF0000..=0xFFFFD).contains(&ch)
        || (0x100000..=0x10FFFD).contains(&ch)
}

/// 一行连续的提示符标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChipRun {
    /// 视口行。
    pub row: u16,
    /// 起始列。
    pub col: u16,
    /// 宽度（单元格）。
    pub cols: u16,
    /// 当前待输入提示符：画边框，并保留目录粗体。
    pub emphasized: bool,
}

/// 一行的元数据（时间戳栏、分隔线、命令块命中用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineInfo {
    /// 绝对行号。
    pub abs_line: u64,
    /// 首次写出时的 Unix 毫秒；0 表示没有。
    pub timestamp_ms: i64,
    /// 从未写过内容的行。
    pub blank: bool,
    /// 光标所在行。
    pub cursor: bool,
    /// OSC 133 A 所在行（分隔线画在其上方）。
    pub prompt: bool,
    /// 会话第一个提示符（上方不画分隔线）。
    pub first_prompt: bool,
    /// 软换行到下一行。
    pub wrapped: bool,
}

/// 一帧的渲染数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderGrid {
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
    /// 背景矩形。
    pub bgs: Vec<BgRect>,
    /// 每个提示符标签。
    pub chips: Vec<ChipRun>,
    /// 文字片段。
    pub spans: Vec<TextSpan>,
    /// 光标（不可见或回滚查看历史时为 `None`）。
    pub cursor: Option<CursorDraw>,
    /// 光标所在单元格（即使光标被隐藏也给出，输入法候选框定位用）。
    pub cursor_cell: (u16, u16),
    /// 向上滚动的行数，0 表示在底部。
    pub display_offset: u32,
    /// 回滚缓冲区行数。
    pub scrollback_lines: u32,
    /// 程序是否开启了鼠标上报。
    pub mouse_reporting: bool,
    /// 每个视口行的元数据，长度等于 `rows`。
    pub lines: Vec<LineInfo>,
    /// 显示行号 = 绝对行号 - 本值 + 1（`clear` 后前移）。
    pub line_number_base: u64,
    /// 视口第 0 行的绝对行号。
    pub first_abs_line: u64,
    /// 是否在备用屏幕（全屏程序）：此时不画栏、分隔线与块装饰。
    pub alt_screen: bool,
}

/// 同一片段内必须一致的样式。
#[derive(Clone, Copy, PartialEq, Eq)]
struct SpanStyle {
    fg: Rgb8,
    bold: bool,
    italic: bool,
    underline: u16,
    underline_color: Option<Rgb8>,
    strike: bool,
}

struct ResolvedCell {
    style: SpanStyle,
    bg: Rgb8,
}

fn color_of(raw: u32, palette: &Palette, default: Rgb8) -> Rgb8 {
    match Color::decode(raw) {
        DecodedColor::Default => default,
        DecodedColor::Indexed(i) => palette.indexed(i),
        DecodedColor::Rgb(Rgb { r, g, b }) => Rgb8::new(r, g, b),
        DecodedColor::Semantic(c) => palette.semantic_color(c),
    }
}

/// 粗体前景的 ANSI 0..=7 提亮为 8..=15（bold-as-bright，xterm / Xshell 惯例）。
///
/// 只作用于显示解析：不改帧里的索引，原始网格、选区与复制文本都不受影响。
/// 只对调色板索引生效——RGB、语义色、默认色以及已是亮色的 8..=15 都原样返回。
/// `ls` 的 `01;32`（粗体绿）等 LS_COLORS 正是靠这条规则显示成亮色。
fn brighten_bold_foreground(raw: u32, palette: &Palette, default: Rgb8, bold: bool) -> Rgb8 {
    if bold && let DecodedColor::Indexed(i @ 0..=7) = Color::decode(raw) {
        palette.indexed(i + 8)
    } else {
        color_of(raw, palette, default)
    }
}

/// 按显示优先级合成单元格的前景和背景。
fn resolve(
    cell: &Cell,
    underline_color_raw: u32,
    palette: &Palette,
    guard: Option<&mut ContrastGuard>,
    block_cursor: bool,
    keep_chip_bold: bool,
    quiet: bool,
) -> ResolvedCell {
    let flags = cell.flags;
    let bold = flags & CELL_BOLD != 0;
    let mut fg = brighten_bold_foreground(cell.fg, palette, palette.foreground, bold);
    // 背景不会是语义色（语义着色只出现在前景）；遇到时按调色板解析即可。
    let mut bg = color_of(cell.bg, palette, palette.background);
    if flags & CELL_INVERSE != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags & CELL_SELECTED != 0 {
        bg = palette.selection_background;
    } else if flags & CELL_MATCH_CURRENT != 0 {
        bg = bg.mixed(palette.cursor, 0.7);
    } else if flags & CELL_MATCH != 0 {
        bg = bg.mixed(palette.cursor, 0.35);
    }
    // 先按可见底色保护对比度，再应用 SGR 2，保持弱化文本的层级。
    if block_cursor {
        fg = palette.cursor_text;
        bg = palette.cursor;
    } else if let Some(g) = guard {
        let shown = if flags & CELL_CHIP != 0 && bg == palette.background {
            palette.decorations.chip_background
        } else {
            bg
        };
        fg = g.adjusted(fg, shown);
    }
    if flags & CELL_DIM != 0 {
        fg = fg.mixed(bg, 0.4);
    }
    // 历史提示符统一收成次要灰（与时间戳栏同档），避免彩色字混背景后发脏；
    // 处于选区或搜索高亮的单元格保留对比度保护后的前景色。
    let highlighted = flags & (CELL_SELECTED | CELL_MATCH | CELL_MATCH_CURRENT) != 0;
    if quiet && !block_cursor && !highlighted {
        fg = palette.decorations.gutter_text;
    }
    let underline = (flags & CELL_UNDERLINE_MASK) >> CELL_UNDERLINE_SHIFT;
    let underline_color = (underline != 0 && underline_color_raw != 0)
        .then(|| color_of(underline_color_raw, palette, fg));
    ResolvedCell {
        style: SpanStyle {
            fg,
            // 字形粗细沿用上游的视觉弱化规则；颜色提亮用上面的原始 bold（终端语义）。
            bold: !quiet && flags & CELL_BOLD != 0 && (keep_chip_bold || flags & CELL_CHIP == 0),
            italic: flags & CELL_ITALIC != 0,
            underline,
            underline_color,
            strike: flags & CELL_STRIKE != 0,
        },
        bg,
    }
}

/// 把一帧转成渲染数据（不做对比度保护）。
pub fn build_render_grid(view: &FrameView<'_>, palette: &Palette) -> RenderGrid {
    build_render_grid_with(view, palette, None)
}

/// 把一帧转成渲染数据；`guard` 不为空时对程序指定的前景色做最低对比度保护。
/// 不区分历史提示符，标签全部按当前待输入的样式处理。
pub fn build_render_grid_with(
    view: &FrameView<'_>,
    palette: &Palette,
    guard: Option<&mut ContrastGuard>,
) -> RenderGrid {
    build_render_grid_for(view, palette, guard, LiveChips::All, &[])
}

/// 同 [`build_render_grid_with`]，并按命令块区分当前提示符和历史提示符。
pub fn build_render_grid_for(
    view: &FrameView<'_>,
    palette: &Palette,
    mut guard: Option<&mut ContrastGuard>,
    live: LiveChips,
    blocks: &[CommandBlock],
) -> RenderGrid {
    let header = view.header();
    let cols = header.cols;
    let rows = header.rows;
    let cursor_visible = header.flags & FRAME_CURSOR_VISIBLE != 0 && header.display_offset == 0;
    let (cursor_col, cursor_row) = (header.cursor_col, header.cursor_row);

    let mut bgs: Vec<BgRect> = Vec::new();
    let mut chips = Vec::new();
    let mut spans: Vec<TextSpan> = Vec::new();
    let mut cursor: Option<CursorDraw> = None;

    for row in 0..rows {
        let cells = view.row(row);
        let abs = view.line_meta(row).map(|m| m.abs_line);
        let emphasized = abs.is_some_and(|abs| live.contains(abs));
        let quiet = abs.is_some_and(|abs| historical_prompt(blocks, abs));
        // 当前正在累积的普通片段。
        let mut open: Option<TextSpan> = None;
        let mut col: u16 = 0;
        while usize::from(col) < cells.len() {
            let cell = &cells[usize::from(col)];
            if cell.flags & CELL_CHIP != 0
                && (col == 0 || cells[usize::from(col - 1)].flags & CELL_CHIP == 0)
            {
                let length = cells[usize::from(col)..]
                    .iter()
                    .take_while(|c| c.flags & CELL_CHIP != 0)
                    .count();
                chips.push(ChipRun {
                    row,
                    col,
                    cols: length as u16,
                    emphasized,
                });
            }
            if cell.flags & CELL_WIDE_SPACER != 0 {
                // 宽字符的后半：已由前半覆盖。
                col += 1;
                continue;
            }
            let extra = view.extra(cell);
            let grapheme = extra.and_then(|e| view.grapheme(e));
            let wide = cells.get(usize::from(col) + 1).is_some_and(|next| {
                cell.flags & CELL_WIDE != 0
                    || (cell.ch > 0x7f && next.flags & CELL_WIDE_SPACER != 0)
            });
            let width: u16 = if wide { 2 } else { 1 };

            let separator = grapheme.is_none() && cell.ch == 0xe0b0;
            let on_cursor = cursor_visible && row == cursor_row && col == cursor_col;
            let resolved = resolve(
                cell,
                extra.map_or(0, |e| e.underline_color),
                palette,
                if separator {
                    None
                } else {
                    guard.as_deref_mut()
                },
                on_cursor && header.cursor_shape == CURSOR_BLOCK,
                emphasized,
                quiet,
            );
            if on_cursor {
                match header.cursor_shape {
                    CURSOR_BLOCK => {
                        cursor = Some(CursorDraw {
                            row,
                            col,
                            cells: width,
                            kind: CursorKind::Block,
                            color: palette.cursor,
                        });
                    }
                    shape => {
                        cursor = Some(CursorDraw {
                            row,
                            col,
                            cells: width,
                            kind: match shape {
                                CURSOR_UNDERLINE => CursorKind::Underline,
                                CURSOR_BAR => CursorKind::Bar,
                                _ => CursorKind::Hollow,
                            },
                            color: palette.cursor,
                        });
                    }
                }
            }

            // 背景：与前一块相邻且同色就合并。
            if resolved.bg != palette.background {
                match bgs.last_mut() {
                    Some(last)
                        if last.row == row
                            && last.col + last.cols == col
                            && last.color == resolved.bg =>
                    {
                        last.cols += width;
                    }
                    _ => bgs.push(BgRect {
                        row,
                        col,
                        cols: width,
                        color: resolved.bg,
                    }),
                }
            }

            // 文字。历史提示符里的私有区图标不进片段，避免依赖 Nerd Font。
            let text: String = if quiet && is_special_symbol(cell.ch) {
                " ".to_owned()
            } else {
                match (cell.flags & CELL_HIDDEN != 0, grapheme) {
                    (true, _) => " ".to_owned(),
                    (false, Some(g)) => g.to_owned(),
                    (false, None) => match char::from_u32(cell.ch) {
                        Some(c) if cell.ch != 0 && !c.is_control() => c.to_string(),
                        _ => " ".to_owned(),
                    },
                }
            };
            let blank = text == " " && resolved.style.underline == 0 && !resolved.style.strike;
            let simple = !wide
                && text.chars().count() == 1
                && !matches!(text.as_str(), "\u{e0b0}" | "\u{e0a0}" | "\u{f07b}");

            if blank {
                spans.extend(open.take());
            } else if !simple {
                spans.extend(open.take());
                spans.push(new_span(row, col, width, text, false, resolved.style));
            } else {
                match open.as_mut() {
                    Some(o) if o.col + o.cells == col && same_style(o, &resolved.style) => {
                        o.text.push_str(&text);
                        o.cells += 1;
                    }
                    _ => {
                        spans.extend(open.take());
                        open = Some(new_span(row, col, 1, text, true, resolved.style));
                    }
                }
            }
            col += width;
        }
        spans.extend(open.take());
    }

    RenderGrid {
        cols,
        rows,
        bgs,
        chips,
        spans,
        cursor,
        cursor_cell: (cursor_col, cursor_row),
        display_offset: header.display_offset,
        scrollback_lines: header.scrollback_lines,
        mouse_reporting: header.flags & FRAME_MOUSE_REPORTING != 0,
        lines: (0..rows)
            .map(|r| {
                view.line_meta(r)
                    .map_or_else(LineInfo::default, |m| LineInfo {
                        abs_line: m.abs_line,
                        timestamp_ms: m.timestamp_ms,
                        blank: m.flags & LINE_BLANK != 0,
                        cursor: m.flags & LINE_CURSOR != 0,
                        prompt: m.flags & LINE_PROMPT != 0,
                        first_prompt: m.flags & LINE_FIRST_PROMPT != 0,
                        wrapped: m.flags & LINE_WRAPPED != 0,
                    })
            })
            .collect(),
        line_number_base: header.line_number_base,
        first_abs_line: header.first_abs_line,
        alt_screen: header.flags & FRAME_ALT_SCREEN != 0,
    }
}

/// 每行按列排列的字符（宽字符的占位格为 `'\0'`），带软换行标志；链接识别用。
pub fn line_texts(view: &FrameView<'_>) -> Vec<LineText> {
    (0..view.header().rows)
        .map(|r| LineText {
            chars: view
                .row(r)
                .iter()
                .map(|cell| {
                    if cell.flags & CELL_WIDE_SPACER != 0 {
                        return '\0';
                    }
                    let first = view
                        .extra(cell)
                        .and_then(|e| view.grapheme(e))
                        .and_then(|g| g.chars().next());
                    match first.or_else(|| char::from_u32(cell.ch)) {
                        Some(c) if cell.ch != 0 && !c.is_control() => c,
                        _ => ' ',
                    }
                })
                .collect(),
            wrapped: view
                .line_meta(r)
                .is_some_and(|m| m.flags & LINE_WRAPPED != 0),
        })
        .collect()
}

/// 单元格上的 OSC 8 超链接 id（没有为 `None`）。
pub fn hyperlink_at(view: &FrameView<'_>, row: u16, col: u16) -> Option<u32> {
    let cell = view.row(row).get(usize::from(col))?;
    view.extra(cell)
        .map(|e| e.hyperlink_id)
        .filter(|&id| id != 0)
}

fn new_span(row: u16, col: u16, cells: u16, text: String, simple: bool, s: SpanStyle) -> TextSpan {
    TextSpan {
        row,
        col,
        cells,
        text,
        simple,
        fg: s.fg,
        bold: s.bold,
        italic: s.italic,
        underline: s.underline,
        underline_color: s.underline_color,
        strike: s.strike,
    }
}

fn same_style(span: &TextSpan, s: &SpanStyle) -> bool {
    span.fg == s.fg
        && span.bold == s.bold
        && span.italic == s.italic
        && span.underline == s.underline
        && span.underline_color == s.underline_color
        && span.strike == s.strike
}

/// 取一行的纯文本（宽字符占位格跳过，空单元格当空格），去掉行尾空白。自检与测试用。
pub fn row_text(view: &FrameView<'_>, row: u16) -> String {
    let mut out = String::new();
    append_row_text(view, row, &mut out);
    out.truncate(out.trim_end().len());
    out
}

fn append_row_text(view: &FrameView<'_>, row: u16, out: &mut String) {
    for cell in view.row(row) {
        if cell.flags & CELL_WIDE_SPACER != 0 {
            continue;
        }
        match view.extra(cell).and_then(|e| view.grapheme(e)) {
            Some(g) => out.push_str(g),
            None => out.push(match char::from_u32(cell.ch) {
                Some(c) if cell.ch != 0 && !c.is_control() => c,
                _ => ' ',
            }),
        }
    }
}

/// 自检要区分 shell 的命令回显和执行输出，按逻辑行计数，不能把窄分屏中的软折行算作换行。
pub fn logical_screen_text(view: &FrameView<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for row in 0..view.header().rows {
        append_row_text(view, row, &mut line);
        let wrapped = view
            .line_meta(row)
            .is_some_and(|m| m.flags & LINE_WRAPPED != 0);
        if !wrapped || row + 1 == view.header().rows {
            line.truncate(line.trim_end().len());
            lines.push(std::mem::take(&mut line));
        }
    }
    lines
}

/// 整屏文本，每行一项。
pub fn screen_text(view: &FrameView<'_>) -> Vec<String> {
    (0..view.header().rows).map(|r| row_text(view, r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_frame::{CURSOR_HOLLOW_BLOCK, CellExtra, Frame, FrameBuilder, UNDERLINE_SINGLE};
    use kanso_types::geometry::TermColor;

    fn pal() -> Palette {
        Palette::kanso_dark()
    }

    fn plain(ch: char) -> Cell {
        Cell {
            ch: ch as u32,
            fg: 0,
            bg: 0,
            flags: 0,
            extra: 0,
        }
    }

    fn put(b: &mut FrameBuilder, row: u16, col: u16, text: &str) {
        for (i, c) in text.chars().enumerate() {
            b.set_cell(row, col + i as u16, plain(c));
        }
    }

    fn frame(b: FrameBuilder) -> Frame {
        b.finish()
    }

    #[test]
    fn plain_text_merges_into_one_span_and_skips_blanks() {
        let mut b = FrameBuilder::new(20, 2);
        put(&mut b, 0, 0, "ls -l");
        put(&mut b, 1, 3, "ab");
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        let texts: Vec<_> = g
            .spans
            .iter()
            .map(|s| (s.row, s.col, s.cells, s.text.as_str()))
            .collect();
        // 第 0 行：空格把 "ls" 与 "-l" 分成两段；第 1 行从第 3 列开始。
        assert_eq!(
            texts,
            vec![(0, 0, 2, "ls"), (0, 3, 2, "-l"), (1, 3, 2, "ab")]
        );
        assert!(g.bgs.is_empty());
        assert!(g.spans.iter().all(|s| s.simple));
    }

    #[test]
    fn bold_ansi_foreground_brightens_but_rgb_and_bright_stay() {
        let p = pal();
        // 粗体 + ANSI 0..=7 → 提亮到 8..=15（ls 的 01;32 等）。
        assert_eq!(
            brighten_bold_foreground(Color::encode(TermColor::Indexed(2)), &p, p.foreground, true),
            p.ansi[10]
        );
        assert_eq!(
            brighten_bold_foreground(Color::encode(TermColor::Indexed(4)), &p, p.foreground, true),
            p.ansi[12]
        );
        // 非粗体保持原色。
        assert_eq!(
            brighten_bold_foreground(
                Color::encode(TermColor::Indexed(2)),
                &p,
                p.foreground,
                false
            ),
            p.ansi[2]
        );
        // 粗体但已是亮色 8..=15：保持原色，不二次提亮。
        assert_eq!(
            brighten_bold_foreground(
                Color::encode(TermColor::Indexed(10)),
                &p,
                p.foreground,
                true
            ),
            p.ansi[10]
        );
        // 粗体 RGB、语义色、默认色都不参与提亮。
        let rgb = Color::encode(TermColor::Rgb(Rgb { r: 1, g: 2, b: 3 }));
        assert_eq!(
            brighten_bold_foreground(rgb, &p, p.foreground, true),
            Rgb8::new(1, 2, 3)
        );
        assert_eq!(
            brighten_bold_foreground(Color::semantic(0), &p, p.foreground, true),
            p.semantic_color(0)
        );
        assert_eq!(
            brighten_bold_foreground(0, &p, p.foreground, true),
            p.foreground
        );
    }

    #[test]
    fn bold_ansi_reaches_render_grid_as_bright_color() {
        let mut b = FrameBuilder::new(3, 1);
        b.set_cell(
            0,
            0,
            Cell {
                fg: Color::encode(TermColor::Indexed(2)),
                flags: CELL_BOLD,
                ..plain('a')
            },
        );
        b.set_cell(
            0,
            1,
            Cell {
                fg: Color::encode(TermColor::Indexed(2)),
                ..plain('b')
            },
        );
        let f = frame(b);
        let p = pal();
        let g = build_render_grid(&f.view().unwrap(), &p);
        assert_eq!(g.spans[0].fg, p.ansi[10], "粗体绿应显示为亮绿");
        assert_eq!(g.spans[1].fg, p.ansi[2], "非粗体绿应保持暗绿");
    }

    #[test]
    fn style_change_splits_span_and_colors_resolve() {
        let mut b = FrameBuilder::new(10, 1);
        let red = Color::encode(TermColor::Indexed(1));
        let rgb = Color::encode(TermColor::Rgb(Rgb { r: 1, g: 2, b: 3 }));
        b.set_cell(
            0,
            0,
            Cell {
                fg: red,
                ..plain('a')
            },
        );
        b.set_cell(
            0,
            1,
            Cell {
                fg: red,
                ..plain('b')
            },
        );
        b.set_cell(
            0,
            2,
            Cell {
                fg: rgb,
                flags: CELL_BOLD,
                ..plain('c')
            },
        );
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!(g.spans.len(), 2);
        assert_eq!(g.spans[0].text, "ab");
        assert_eq!(g.spans[0].fg, pal().ansi[1]);
        assert_eq!(g.spans[1].fg, Rgb8::new(1, 2, 3));
        assert!(g.spans[1].bold);
    }

    #[test]
    fn background_rects_merge_and_skip_default() {
        let mut b = FrameBuilder::new(10, 1);
        let blue = Color::encode(TermColor::Indexed(4));
        let green = Color::encode(TermColor::Indexed(2));
        for c in 0..3 {
            b.set_cell(
                0,
                c,
                Cell {
                    bg: blue,
                    ..plain(' ')
                },
            );
        }
        b.set_cell(
            0,
            3,
            Cell {
                bg: green,
                ..plain(' ')
            },
        );
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!(
            g.bgs,
            vec![
                BgRect {
                    row: 0,
                    col: 0,
                    cols: 3,
                    color: pal().ansi[4],
                },
                BgRect {
                    row: 0,
                    col: 3,
                    cols: 1,
                    color: pal().ansi[2],
                },
            ]
        );
        assert!(g.spans.is_empty(), "纯空格背景不应产生文字片段");
    }

    #[test]
    fn inverse_swaps_default_colors() {
        let mut b = FrameBuilder::new(4, 1);
        b.set_cell(
            0,
            0,
            Cell {
                flags: CELL_INVERSE,
                ..plain('x')
            },
        );
        let f = frame(b);
        let p = pal();
        let g = build_render_grid(&f.view().unwrap(), &p);
        assert_eq!(g.spans[0].fg, p.background);
        assert_eq!(g.bgs[0].color, p.foreground);
    }

    #[test]
    fn dim_hidden_and_selected() {
        let mut b = FrameBuilder::new(4, 1);
        b.set_cell(
            0,
            0,
            Cell {
                flags: CELL_DIM,
                ..plain('d')
            },
        );
        b.set_cell(
            0,
            1,
            Cell {
                flags: CELL_HIDDEN,
                ..plain('h')
            },
        );
        b.set_cell(
            0,
            2,
            Cell {
                flags: CELL_SELECTED,
                ..plain('s')
            },
        );
        let f = frame(b);
        let p = pal();
        let g = build_render_grid(&f.view().unwrap(), &p);
        let dim = g.spans.iter().find(|s| s.text == "d").unwrap();
        assert_ne!(dim.fg, p.foreground);
        assert!(g.spans.iter().all(|s| !s.text.contains('h')));
        assert_eq!(g.bgs[0].color, p.selection_background);
        assert_eq!(g.bgs[0].col, 2);
    }

    #[test]
    fn underline_and_strike_keep_blank_cells() {
        let mut b = FrameBuilder::new(4, 1);
        b.set_cell(
            0,
            0,
            Cell {
                flags: UNDERLINE_SINGLE << CELL_UNDERLINE_SHIFT,
                ..plain(' ')
            },
        );
        b.set_cell(
            0,
            1,
            Cell {
                flags: CELL_STRIKE,
                ..plain('x')
            },
        );
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!(g.spans.len(), 2, "样式不同的相邻片段不合并");
        assert_eq!(g.spans[0].underline, UNDERLINE_SINGLE);
        assert!(g.spans[1].strike);
    }

    #[test]
    fn wide_char_is_own_span_and_spacer_skipped() {
        let mut b = FrameBuilder::new(8, 1);
        put(&mut b, 0, 0, "a");
        b.set_cell(
            0,
            1,
            Cell {
                flags: CELL_WIDE,
                ..plain('你')
            },
        );
        b.set_cell(
            0,
            2,
            Cell {
                flags: CELL_WIDE_SPACER,
                ..plain('\0')
            },
        );
        put(&mut b, 0, 3, "b");
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        let v: Vec<_> = g
            .spans
            .iter()
            .map(|s| (s.col, s.cells, s.text.as_str(), s.simple))
            .collect();
        assert_eq!(
            v,
            vec![(0, 1, "a", true), (1, 2, "你", false), (3, 1, "b", true)]
        );
        assert_eq!(row_text(&f.view().unwrap(), 0), "a你b");
    }

    #[test]
    fn grapheme_cluster_uses_extra_text() {
        let mut b = FrameBuilder::new(4, 1);
        let (off, len) = b.push_grapheme("e\u{301}").unwrap();
        let extra = b
            .push_extra(CellExtra {
                grapheme_offset: off,
                grapheme_len: len,
                hyperlink_id: 0,
                underline_color: 0,
            })
            .unwrap();
        b.set_cell(
            0,
            0,
            Cell {
                extra,
                ..plain('e')
            },
        );
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!(g.spans.len(), 1);
        assert_eq!(g.spans[0].text, "e\u{301}");
        assert!(!g.spans[0].simple);
    }

    #[test]
    fn block_cursor_recolors_cell_and_is_reported() {
        let mut b = FrameBuilder::new(6, 2);
        put(&mut b, 1, 2, "x");
        b.set_flags(FRAME_CURSOR_VISIBLE);
        b.set_cursor(2, 1, CURSOR_BLOCK);
        let f = frame(b);
        let p = pal();
        let g = build_render_grid(&f.view().unwrap(), &p);
        let c = g.cursor.clone().unwrap();
        assert_eq!((c.row, c.col, c.kind), (1, 2, CursorKind::Block));
        assert_eq!(g.bgs[0].color, p.cursor);
        assert_eq!(g.spans[0].fg, p.cursor_text);
        assert_eq!(g.cursor_cell, (2, 1));
    }

    #[test]
    fn bar_and_hollow_cursor_do_not_recolor() {
        for (shape, kind) in [
            (CURSOR_BAR, CursorKind::Bar),
            (CURSOR_HOLLOW_BLOCK, CursorKind::Hollow),
        ] {
            let mut b = FrameBuilder::new(4, 1);
            b.set_flags(FRAME_CURSOR_VISIBLE);
            b.set_cursor(1, 0, shape);
            let f = frame(b);
            let g = build_render_grid(&f.view().unwrap(), &pal());
            assert_eq!(g.cursor.unwrap().kind, kind);
            assert!(g.bgs.is_empty());
        }
    }

    #[test]
    fn hidden_cursor_or_scrollback_has_no_cursor_but_keeps_cell() {
        let mut b = FrameBuilder::new(4, 1);
        b.set_cursor(3, 0, CURSOR_BLOCK);
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert!(g.cursor.is_none());
        assert_eq!(g.cursor_cell, (3, 0));

        let mut b = FrameBuilder::new(4, 1);
        b.set_flags(FRAME_CURSOR_VISIBLE);
        b.set_scroll(5, 100);
        let f = frame(b);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert!(g.cursor.is_none());
        assert_eq!((g.display_offset, g.scrollback_lines), (5, 100));
    }

    #[test]
    fn semantic_color_maps_through_palette() {
        let mut b = FrameBuilder::new(4, 1);
        b.set_cell(
            0,
            0,
            Cell {
                fg: Color::semantic(10),
                ..plain('E')
            },
        );
        let f = frame(b);
        let p = pal();
        let g = build_render_grid(&f.view().unwrap(), &p);
        assert_eq!(g.spans[0].fg, p.semantic[10]);
    }

    #[test]
    fn mouse_reporting_flag_is_forwarded() {
        let mut b = FrameBuilder::new(2, 1);
        b.set_flags(FRAME_MOUSE_REPORTING);
        let f = frame(b);
        assert!(build_render_grid(&f.view().unwrap(), &pal()).mouse_reporting);
    }

    #[test]
    fn mock_frame_renders_without_panicking() {
        let f = kanso_frame::mock::mock_frame(80, 24, 1);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!((g.cols, g.rows), (80, 24));
        assert!(!g.spans.is_empty());
        let dense = kanso_frame::mock::mock_frame_dense(120, 40, 2);
        let g = build_render_grid(&dense.view().unwrap(), &pal());
        assert!(g.spans.len() >= 40);
    }

    #[test]
    fn line_info_comes_from_frame_metadata() {
        let f = kanso_frame::mock::mock_command_blocks_frame(40, 8);
        let g = build_render_grid(&f.view().unwrap(), &pal());
        assert_eq!(g.lines.len(), 8);
        assert!(g.lines[0].prompt && g.lines[0].first_prompt);
        assert!(g.lines[3].prompt && !g.lines[3].first_prompt);
        assert!(!g.lines[1].prompt);
        assert_eq!(g.lines[2].abs_line, 2);
        assert!(g.lines[2].timestamp_ms > 0);
        assert!(!g.alt_screen);
        assert_eq!(g.first_abs_line, 0);
    }

    #[test]
    fn terminal_preset_search_colors_match_mac() {
        for (palette, expected) in [
            (Palette::kanso_dark(), [0x26414D, 0x467D95]),
            (Palette::kanso_light(), [0xB2C8D2, 0x6996AB]),
        ] {
            let mut b = FrameBuilder::new(3, 1);
            for (col, flags) in [
                CELL_MATCH,
                CELL_MATCH_CURRENT,
                CELL_SELECTED | CELL_MATCH_CURRENT,
            ]
            .into_iter()
            .enumerate()
            {
                b.set_cell(
                    0,
                    col as u16,
                    Cell {
                        flags,
                        ..plain('x')
                    },
                );
            }
            let f = frame(b);
            let g = build_render_grid(&f.view().unwrap(), &palette);
            assert_eq!(
                g.bgs.iter().map(|r| r.color.to_u32()).collect::<Vec<_>>(),
                [
                    expected[0],
                    expected[1],
                    palette.selection_background.to_u32()
                ]
            );
        }
    }

    #[test]
    fn dim_uses_final_colors_and_chip_runs_follow_cells() {
        let p = Palette::kanso_dark();
        let mut guard = ContrastGuard::new(4.5);
        let mut b = FrameBuilder::new(4, 1);
        b.set_cell(
            0,
            0,
            Cell {
                flags: CELL_CHIP | CELL_DIM,
                fg: Color::encode(TermColor::Indexed(0)),
                ..plain('x')
            },
        );
        b.set_cell(
            0,
            1,
            Cell {
                flags: CELL_CHIP,
                ..plain(' ')
            },
        );
        b.set_cell(
            0,
            3,
            Cell {
                flags: CELL_DIM,
                ..plain('y')
            },
        );
        b.set_flags(FRAME_CURSOR_VISIBLE);
        b.set_cursor(3, 0, CURSOR_BLOCK);
        let f = frame(b);
        let g = build_render_grid_with(&f.view().unwrap(), &p, Some(&mut guard));
        assert_eq!(
            g.chips,
            vec![ChipRun {
                row: 0,
                col: 0,
                cols: 2,
                emphasized: true,
            }]
        );
        let fg = guard.adjusted(p.ansi[0], p.decorations.chip_background);
        assert_eq!(g.spans[0].fg, fg.mixed(p.background, 0.4));
        assert_eq!(g.spans[1].fg, p.cursor_text.mixed(p.cursor, 0.4));
    }

    #[test]
    fn historical_chips_drop_bold_while_the_idle_prompt_keeps_it() {
        let mut b = FrameBuilder::new(2, 2);
        b.set_cell(
            0,
            0,
            Cell {
                flags: CELL_CHIP | CELL_BOLD,
                ..plain('a')
            },
        );
        b.set_cell(
            1,
            0,
            Cell {
                flags: CELL_CHIP | CELL_BOLD,
                ..plain('b')
            },
        );
        b.line_meta_mut(0).unwrap().abs_line = 1;
        b.line_meta_mut(1).unwrap().abs_line = 4;
        let f = frame(b);
        let g = build_render_grid_for(
            &f.view().unwrap(),
            &Palette::kanso_dark(),
            None,
            LiveChips::Only { start: 4, end: 4 },
            &[],
        );
        assert!(!g.chips[0].emphasized);
        assert!(g.chips[1].emphasized);
        assert!(!g.spans.iter().find(|s| s.row == 0).unwrap().bold);
        assert!(g.spans.iter().find(|s| s.text == "b").unwrap().bold);
    }

    #[test]
    fn historical_prompt_drops_special_symbols_and_shrinks() {
        let mut b = FrameBuilder::new(4, 2);
        // 提示符行：目录字 + 私有区图标 + 选中的分支字。命令在下一行。
        b.set_cell(0, 0, Cell { ..plain('~') });
        b.set_cell(
            0,
            1,
            Cell {
                ch: 0xF07B,
                fg: Color::encode(TermColor::Indexed(10)),
                ..plain('\0')
            },
        );
        b.set_cell(
            0,
            2,
            Cell {
                flags: CELL_SELECTED,
                ..plain('m')
            },
        );
        b.set_cell(1, 0, Cell { ..plain('l') });
        b.line_meta_mut(0).unwrap().abs_line = 3;
        b.line_meta_mut(1).unwrap().abs_line = 4;
        let block = CommandBlock {
            id: 1,
            latest: false,
            start_line: 3,
            end_line: 6,
            command_line: Some(4),
            running: false,
            exit_code: Some(0),
            duration: None,
            can_copy_command: true,
            can_copy_output: true,
        };
        let f = frame(b);
        let dark = Palette::kanso_dark();
        let g = build_render_grid_for(
            &f.view().unwrap(),
            &dark,
            None,
            LiveChips::Only { start: 1, end: 0 },
            &[block],
        );
        let prompt = g.spans.iter().find(|s| s.text == "~").unwrap();
        assert!(!prompt.bold);
        assert_eq!(prompt.fg, dark.decorations.gutter_text);
        let selected = g.spans.iter().find(|s| s.text == "m").unwrap();
        assert_eq!(selected.fg, dark.foreground);
        assert_eq!(
            g.bgs,
            vec![BgRect {
                row: 0,
                col: 2,
                cols: 1,
                color: dark.selection_background,
            }]
        );
        let command = g.spans.iter().find(|s| s.text == "l").unwrap();
        assert_eq!(command.fg, dark.foreground);
    }

    #[test]
    fn only_the_idle_latest_block_keeps_full_chips() {
        fn block(latest: bool, running: bool, command: Option<u64>, start: u64) -> CommandBlock {
            CommandBlock {
                id: start,
                latest,
                start_line: start,
                end_line: start,
                command_line: command,
                running,
                exit_code: None,
                duration: None,
                can_copy_command: command.is_some(),
                can_copy_output: command.is_some(),
            }
        }
        assert_eq!(LiveChips::from_blocks(&[]), LiveChips::All);
        assert_eq!(
            LiveChips::from_blocks(&[block(true, false, None, 8)]),
            LiveChips::Only { start: 8, end: 8 }
        );
        // 命令已经开始，或还在跑：没有待输入提示符。
        assert_eq!(
            LiveChips::from_blocks(&[block(true, true, Some(2), 1)]),
            LiveChips::Only { start: 1, end: 0 }
        );
        assert_eq!(
            LiveChips::from_blocks(
                &[block(false, false, Some(1), 1), block(true, false, None, 6),]
            ),
            LiveChips::Only { start: 6, end: 6 }
        );
    }

    #[test]
    fn contrast_guard_adjusts_low_contrast_and_keeps_compliant_text() {
        let light = Palette::kanso_light();
        let white = Color::encode(TermColor::Indexed(15));
        let mut b = FrameBuilder::new(4, 1);
        // 程序指定的亮白（在浅色背景上几乎不可见）与默认前景。
        b.set_cell(
            0,
            0,
            Cell {
                fg: white,
                ..plain('a')
            },
        );
        b.set_cell(0, 2, plain('b'));
        let f = frame(b);
        let off = build_render_grid(&f.view().unwrap(), &light);
        let mut guard = ContrastGuard::new(4.5);
        let on = build_render_grid_with(&f.view().unwrap(), &light, Some(&mut guard));
        let fg_of = |g: &RenderGrid, col| g.spans.iter().find(|s| s.col == col).unwrap().fg;
        assert_eq!(fg_of(&off, 0), light.ansi[15]);
        assert!(crate::contrast::ratio(fg_of(&on, 0), light.background) >= 4.5);
        // 默认前景不动。
        assert_eq!(fg_of(&on, 2), light.foreground);
    }

    #[test]
    fn link_helpers_read_text_and_hyperlinks() {
        let mut b = FrameBuilder::new(6, 2);
        put(&mut b, 0, 0, "ab");
        let id = b
            .push_extra(CellExtra {
                grapheme_offset: 0,
                grapheme_len: 0,
                hyperlink_id: 7,
                underline_color: 0,
            })
            .unwrap();
        b.set_cell(
            0,
            3,
            Cell {
                extra: id,
                ..plain('c')
            },
        );
        let f = frame(b);
        let v = f.view().unwrap();
        let lines = line_texts(&v);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars.iter().collect::<String>(), "ab c  ");
        assert_eq!(hyperlink_at(&v, 0, 3), Some(7));
        assert_eq!(hyperlink_at(&v, 0, 0), None);
        assert_eq!(hyperlink_at(&v, 5, 0), None);
    }

    #[test]
    fn selftest_keeps_wrapped_echo_and_output_on_separate_logical_lines() {
        let mut b = FrameBuilder::new(8, 3);
        put(&mut b, 0, 0, "echo KT_");
        put(&mut b, 1, 0, "TAB");
        put(&mut b, 2, 0, "KT_TAB");
        b.line_meta_mut(0).unwrap().flags |= LINE_WRAPPED;
        let f = frame(b);
        assert_eq!(
            logical_screen_text(&f.view().unwrap()),
            ["echo KT_TAB", "KT_TAB"]
        );
    }

    #[test]
    fn screen_text_trims_trailing_blanks() {
        let mut b = FrameBuilder::new(10, 2);
        put(&mut b, 0, 0, "hi");
        let f = frame(b);
        assert_eq!(
            screen_text(&f.view().unwrap()),
            vec!["hi".to_owned(), String::new()]
        );
    }
}
