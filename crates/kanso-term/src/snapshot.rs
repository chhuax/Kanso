//! 帧快照：把网格 + 侧表写成 `kanso_frame` 的连续缓冲区。

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use kanso_frame::{
    CELL_BOLD, CELL_DIM, CELL_HIDDEN, CELL_INVERSE, CELL_ITALIC, CELL_MATCH, CELL_MATCH_CURRENT,
    CELL_SELECTED, CELL_STRIKE, CELL_UNDERLINE_SHIFT, CELL_WIDE, CELL_WIDE_SPACER, CURSOR_BAR,
    CURSOR_BLOCK, CURSOR_HOLLOW_BLOCK, CURSOR_UNDERLINE, Cell, CellExtra, Color, FRAME_ALT_SCREEN,
    FRAME_CURSOR_BLINK, FRAME_CURSOR_VISIBLE, FRAME_FOCUSED, FRAME_MOUSE_REPORTING, Frame,
    FrameBuilder, LINE_BLANK, LINE_CURSOR, LINE_DIRTY, LINE_FIRST_PROMPT, LINE_PROMPT,
    LINE_WRAPPED, UNDERLINE_CURLY, UNDERLINE_DASHED, UNDERLINE_DOTTED, UNDERLINE_DOUBLE,
    UNDERLINE_SINGLE,
};
use kanso_types::geometry::{AbsLine, Rgb, TermColor};
use vte::ansi::{Color as VColor, CursorShape};

use crate::handler::Core;
use crate::table::{ROW_FIRST, ROW_PROMPT, ROW_WRITTEN};
use crate::types::{CellRange, Overlays};

/// alacritty 的颜色 → 帧里的颜色编码（不预解析成像素色，architecture §5、§11）。
fn encode_color(c: VColor) -> u32 {
    match c {
        VColor::Spec(v) => Color::encode(TermColor::Rgb(Rgb {
            r: v.r,
            g: v.g,
            b: v.b,
        })),
        VColor::Indexed(i) => Color::encode(TermColor::Indexed(i)),
        VColor::Named(n) => {
            let i = n as usize;
            match i {
                0..=15 => Color::encode(TermColor::Indexed(i as u8)),
                // Dim* 变体对应 0..=7 的暗色，单元格里几乎不会出现；真正的“暗”由 DIM 标志表达。
                259..=266 => Color::encode(TermColor::Indexed((i - 259) as u8)),
                // Foreground / Background / Cursor / BrightForeground / DimForeground：默认色。
                _ => Color::encode(TermColor::Default),
            }
        }
    }
}

/// 一个区间在某一行上覆盖的列范围。
fn spans(ranges: &[CellRange], abs: u64, cols: u16) -> Vec<(u16, u16)> {
    ranges
        .iter()
        .filter(|r| r.start.line.0 <= abs && abs <= r.end.line.0)
        .map(|r| {
            let s = if abs == r.start.line.0 {
                r.start.col
            } else {
                0
            };
            let e = if abs == r.end.line.0 {
                r.end.col
            } else {
                cols.saturating_sub(1)
            };
            (s, e)
        })
        .collect()
}

fn hit(spans: &[(u16, u16)], col: u16) -> bool {
    spans.iter().any(|&(s, e)| s <= col && col <= e)
}

impl Core {
    /// 生成一帧。调用方负责 `seq` 递增与清脏标记。
    pub fn build_frame(
        &mut self,
        reuse: Option<Frame>,
        overlays: &Overlays<'_>,
        seq: u64,
    ) -> Frame {
        let cols = self.cols as u16;
        let rows = self.rows as u16;
        let mut b = match reuse {
            Some(f) => FrameBuilder::reuse(f, cols, rows),
            None => FrameBuilder::new(cols, rows),
        };
        self.links.begin_frame();

        let alt = self.alt;
        let grid = self.term.grid();
        let offset = grid.display_offset();
        let hist = grid.history_size();
        let first_abs_line = self.table.screen_top_abs() - offset as u64;
        let semantic = if self.semantic_enabled && !alt {
            self.semantic.frame(grid, first_abs_line)
        } else {
            Vec::new()
        };

        let mode = *self.term.mode();
        let style = self.term.cursor_style();
        let cur = grid.cursor.point;
        let cursor_row = cur.line.0 + offset as i32;
        let cursor_in_view = cursor_row >= 0 && (cursor_row as usize) < self.rows;
        let mut flags = 0;
        if alt {
            flags |= FRAME_ALT_SCREEN;
        }
        if mode.contains(TermMode::SHOW_CURSOR)
            && style.shape != CursorShape::Hidden
            && cursor_in_view
        {
            flags |= FRAME_CURSOR_VISIBLE;
        }
        if style.blinking {
            flags |= FRAME_CURSOR_BLINK;
        }
        if mode.intersects(TermMode::MOUSE_MODE) {
            flags |= FRAME_MOUSE_REPORTING;
        }
        if self.focused {
            flags |= FRAME_FOCUSED;
        }
        // 失焦时块状光标画成空心块；下划线、竖线保持原样（与 alacritty 的约定一致）。
        let shape = match style.shape {
            CursorShape::Block | CursorShape::Hidden if self.focused => CURSOR_BLOCK,
            CursorShape::Block | CursorShape::Hidden => CURSOR_HOLLOW_BLOCK,
            CursorShape::Underline => CURSOR_UNDERLINE,
            CursorShape::Beam => CURSOR_BAR,
            CursorShape::HollowBlock => CURSOR_HOLLOW_BLOCK,
        };
        b.set_seq(seq);
        b.set_flags(flags);
        b.set_line_numbers(first_abs_line, self.line_number_base);
        // 全屏没有回滚，滚动条与偏移都是 0。
        b.set_scroll(
            if alt { 0 } else { offset as u32 },
            if alt { 0 } else { hist as u32 },
        );
        b.set_cursor(
            cur.column.0 as u16,
            if cursor_in_view { cursor_row as u16 } else { 0 },
            shape,
        );

        let last_col = Column(self.cols - 1);
        let sel_on = !overlays.selection.is_empty();
        let match_on = !overlays.matches.is_empty() || overlays.current_match.is_some();
        for r in 0..rows {
            let abs = first_abs_line + u64::from(r);
            let row = &grid[Line(i32::from(r) - offset as i32)];
            let prompt = self.blocks.prompt_columns(AbsLine(abs), cols);

            // ---- 行元数据
            let mut lf = LINE_DIRTY;
            let mut ts = 0;
            if alt {
                lf |= LINE_BLANK;
            } else {
                match self.table.get(abs) {
                    Some(m) => {
                        if m.flags & ROW_WRITTEN != 0 {
                            ts = m.ts_ms;
                        } else {
                            lf |= LINE_BLANK;
                        }
                        if m.flags & ROW_PROMPT != 0 {
                            lf |= LINE_PROMPT;
                        }
                        if m.flags & ROW_FIRST != 0 {
                            lf |= LINE_FIRST_PROMPT;
                        }
                    }
                    None => lf |= LINE_BLANK,
                }
            }
            if row[last_col].flags.contains(Flags::WRAPLINE) {
                lf |= LINE_WRAPPED;
            }
            if cursor_in_view && cursor_row as u16 == r {
                lf |= LINE_CURSOR;
            }
            if let Some(m) = b.line_meta_mut(r) {
                m.abs_line = abs;
                m.timestamp_ms = ts;
                m.flags = lf;
            }

            // ---- 单元格
            let sel = if sel_on {
                spans(overlays.selection, abs, cols)
            } else {
                Vec::new()
            };
            let mut mat = if match_on {
                spans(overlays.matches, abs, cols)
            } else {
                Vec::new()
            };
            let curm = overlays
                .current_match
                .map(|c| spans(&[c], abs, cols))
                .unwrap_or_default();
            mat.extend(curm.iter().copied());

            for c in 0..cols {
                let cell = &row[Column(usize::from(c))];
                let f = cell.flags;
                let mut ch = cell.c as u32;
                if cell.c == ' '
                    || cell.c == '\t'
                    || f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    ch = 0;
                }
                let mut cf = 0u16;
                for (from, to) in [
                    (Flags::BOLD, CELL_BOLD),
                    (Flags::DIM, CELL_DIM),
                    (Flags::ITALIC, CELL_ITALIC),
                    (Flags::STRIKEOUT, CELL_STRIKE),
                    (Flags::INVERSE, CELL_INVERSE),
                    (Flags::HIDDEN, CELL_HIDDEN),
                    (Flags::WIDE_CHAR, CELL_WIDE),
                    (Flags::WIDE_CHAR_SPACER, CELL_WIDE_SPACER),
                ] {
                    if f.contains(from) {
                        cf |= to;
                    }
                }
                let ul = if f.contains(Flags::UNDERLINE) {
                    UNDERLINE_SINGLE
                } else if f.contains(Flags::DOUBLE_UNDERLINE) {
                    UNDERLINE_DOUBLE
                } else if f.contains(Flags::UNDERCURL) {
                    UNDERLINE_CURLY
                } else if f.contains(Flags::DOTTED_UNDERLINE) {
                    UNDERLINE_DOTTED
                } else if f.contains(Flags::DASHED_UNDERLINE) {
                    UNDERLINE_DASHED
                } else {
                    0
                };
                cf |= ul << CELL_UNDERLINE_SHIFT;
                if !sel.is_empty() && hit(&sel, c) {
                    cf |= CELL_SELECTED;
                }
                if !mat.is_empty() && hit(&mat, c) {
                    cf |= CELL_MATCH;
                }
                if !curm.is_empty() && hit(&curm, c) {
                    cf |= CELL_MATCH_CURRENT;
                }
                let mut fg = encode_color(cell.fg);
                if fg == 0
                    && !prompt.as_ref().is_some_and(|range| range.contains(&c))
                    && !f.intersects(Flags::INVERSE | Flags::HIDDEN)
                    && let Some(&class) =
                        semantic.get(usize::from(r) * usize::from(cols) + usize::from(c))
                    && class != u8::MAX
                {
                    fg = Color::semantic(class);
                }
                let bg = encode_color(cell.bg);

                let mut extra_idx = 0u16;
                if cell.extra.is_some() {
                    let zw = cell.zerowidth().filter(|z| !z.is_empty());
                    let link = cell.hyperlink();
                    let ulc = cell.underline_color();
                    if zw.is_some() || link.is_some() || ulc.is_some() {
                        let (g_off, g_len) = match zw {
                            Some(z) => {
                                let mut s = String::with_capacity(4 + z.len() * 3);
                                s.push(cell.c);
                                s.extend(z.iter());
                                b.push_grapheme(&s).unwrap_or((0, 0))
                            }
                            None => (0, 0),
                        };
                        let hyperlink_id = link.as_ref().map_or(0, |l| self.links.intern(l));
                        let underline_color = ulc.map_or(0, encode_color);
                        // 超过 CellExtra 上限（65535）时丢弃附加信息，字符本身仍然显示。
                        extra_idx = b
                            .push_extra(CellExtra {
                                grapheme_offset: g_off,
                                grapheme_len: g_len,
                                hyperlink_id,
                                underline_color,
                            })
                            .unwrap_or(0);
                    }
                }
                if ch == 0 && fg == 0 && bg == 0 && cf == 0 && extra_idx == 0 {
                    continue;
                }
                b.set_cell(
                    r,
                    c,
                    Cell {
                        ch,
                        fg,
                        bg,
                        flags: cf,
                        extra: extra_idx,
                    },
                );
            }
        }
        let mut frame = b.finish();
        frame.command_blocks = self.visible_blocks(first_abs_line, rows);
        frame
    }
}
