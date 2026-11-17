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
    CELL_BOLD, CELL_DIM, CELL_HIDDEN, CELL_INVERSE, CELL_ITALIC, CELL_MATCH, CELL_MATCH_CURRENT,
    CELL_SELECTED, CELL_STRIKE, CELL_UNDERLINE_MASK, CELL_UNDERLINE_SHIFT, CELL_WIDE,
    CELL_WIDE_SPACER, CURSOR_BAR, CURSOR_BLOCK, CURSOR_UNDERLINE, Cell, Color, DecodedColor,
    FRAME_ALT_SCREEN, FRAME_CURSOR_VISIBLE, FRAME_MOUSE_REPORTING, FrameView, LINE_BLANK,
    LINE_CURSOR, LINE_FIRST_PROMPT, LINE_PROMPT, LINE_WRAPPED,
};
use kanso_types::geometry::Rgb;

use crate::app::links::LineText;
use crate::contrast::ContrastGuard;
use crate::theme::{MATCH_BACKGROUND, MATCH_CURRENT_BACKGROUND, Palette, Rgb8};

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

/// 把单元格的颜色与属性解析成最终前景 / 背景（不含光标）。
fn resolve(
    cell: &Cell,
    underline_color_raw: u32,
    palette: &Palette,
    guard: Option<&mut ContrastGuard>,
) -> ResolvedCell {
    let mut fg = color_of(cell.fg, palette, palette.foreground);
    // 背景不会是语义色（语义着色只出现在前景）；遇到时按调色板解析即可。
    let mut bg = color_of(cell.bg, palette, palette.background);
    let flags = cell.flags;
    if flags & CELL_INVERSE != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags & CELL_DIM != 0 {
        // 暗淡：前景向背景靠 40%。
        fg = fg.mix(bg, 102);
    }
    if flags & CELL_SELECTED != 0 {
        bg = palette.selection_background;
    } else if flags & CELL_MATCH_CURRENT != 0 {
        bg = MATCH_CURRENT_BACKGROUND;
    } else if flags & CELL_MATCH != 0 {
        bg = MATCH_BACKGROUND;
    }
    // 最低对比度保护：只处理程序自己指定的索引色 / RGB 色；默认色与语义色已由主题保证，反色是有意的设计。
    if let Some(g) = guard
        && flags & (CELL_INVERSE | CELL_HIDDEN) == 0
        && matches!(
            Color::decode(cell.fg),
            DecodedColor::Indexed(_) | DecodedColor::Rgb(_)
        )
    {
        fg = g.adjusted(fg, bg);
    }
    if flags & CELL_HIDDEN != 0 {
        fg = bg;
    }
    let underline = (flags & CELL_UNDERLINE_MASK) >> CELL_UNDERLINE_SHIFT;
    let underline_color = (underline != 0 && underline_color_raw != 0)
        .then(|| color_of(underline_color_raw, palette, fg));
    ResolvedCell {
        style: SpanStyle {
            fg,
            bold: flags & CELL_BOLD != 0,
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
pub fn build_render_grid_with(
    view: &FrameView<'_>,
    palette: &Palette,
    mut guard: Option<&mut ContrastGuard>,
) -> RenderGrid {
    let header = view.header();
    let cols = header.cols;
    let rows = header.rows;
    let cursor_visible = header.flags & FRAME_CURSOR_VISIBLE != 0 && header.display_offset == 0;
    let (cursor_col, cursor_row) = (header.cursor_col, header.cursor_row);

    let mut bgs: Vec<BgRect> = Vec::new();
    let mut spans: Vec<TextSpan> = Vec::new();
    let mut cursor: Option<CursorDraw> = None;

    for row in 0..rows {
        let cells = view.row(row);
        // 当前正在累积的普通片段。
        let mut open: Option<TextSpan> = None;
        let mut col: u16 = 0;
        while usize::from(col) < cells.len() {
            let cell = &cells[usize::from(col)];
            if cell.flags & CELL_WIDE_SPACER != 0 {
                // 宽字符的后半：已由前半覆盖。
                col += 1;
                continue;
            }
            let extra = view.extra(cell);
            let grapheme = extra.and_then(|e| view.grapheme(e));
            let wide = cell.flags & CELL_WIDE != 0;
            let width: u16 = if wide { 2 } else { 1 };

            let mut resolved = resolve(
                cell,
                extra.map_or(0, |e| e.underline_color),
                palette,
                guard.as_deref_mut(),
            );
            let on_cursor = cursor_visible && row == cursor_row && col == cursor_col;
            if on_cursor {
                match header.cursor_shape {
                    CURSOR_BLOCK => {
                        resolved.bg = palette.cursor;
                        resolved.style.fg = palette.cursor_text;
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

            // 文字。
            let text: String = match grapheme {
                Some(g) => g.to_owned(),
                None => match char::from_u32(cell.ch) {
                    Some(c) if cell.ch != 0 && !c.is_control() => c.to_string(),
                    _ => " ".to_owned(),
                },
            };
            let blank = text == " " && resolved.style.underline == 0 && !resolved.style.strike;
            let simple = !wide && text.chars().count() == 1;

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
    out.truncate(out.trim_end().len());
    out
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
                    color: pal().ansi[4]
                },
                BgRect {
                    row: 0,
                    col: 3,
                    cols: 1,
                    color: pal().ansi[2]
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
        let hidden = g.spans.iter().find(|s| s.text == "h").unwrap();
        assert_eq!(hidden.fg, p.background);
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
    fn contrast_guard_only_touches_program_colors() {
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
