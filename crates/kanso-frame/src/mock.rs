//! 确定性的 mock 帧生产者。
//!
//! 在真实 core 就绪前，让渲染器能验证全部显示情形：提示符与分隔线、三种颜色编码、
//! 文字样式、宽字符、字形簇、超链接、选区与搜索高亮、空白行、光标。
//! 同样的参数必然产出同样的字节；`seq` 每加 1，内容整体上移一行（便于肉眼看到滚动动画）。

use crate::builder::{Frame, FrameBuilder};
use crate::color::Color;
use crate::layout::*;
use kanso_types::geometry::{Rgb, TermColor};

/// 时间戳基准（Unix 毫秒），固定值以保证输出确定。
const BASE_TIME_MS: i64 = 1_700_000_000_000;
/// 脚本循环周期（行数）。
const SCRIPT_LEN: u64 = 14;

#[derive(Clone, Copy)]
struct Style {
    fg: u32,
    bg: u32,
    flags: u16,
}

const PLAIN: Style = Style {
    fg: 0,
    bg: 0,
    flags: 0,
};

fn idx(i: u8) -> u32 {
    Color::encode(TermColor::Indexed(i))
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    Color::encode(TermColor::Rgb(Rgb { r, g, b }))
}

/// mock 专用的宽度判断：只覆盖 CJK 统一表意文字、全角标点与全角形式。
fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0x4E00..=0x9FFF | 0xFF00..=0xFF60)
}

/// 单行写入游标。
struct Row<'a> {
    b: &'a mut FrameBuilder,
    row: u16,
    col: u16,
    cols: u16,
}

impl Row<'_> {
    fn text(&mut self, s: &str, st: Style) -> &mut Self {
        for c in s.chars() {
            let wide = is_wide(c);
            let w = if wide { 2 } else { 1 };
            // 宽字符放不下时整体丢弃，不能只写前半。
            if u32::from(self.col) + w > u32::from(self.cols) {
                break;
            }
            let (lead, spacer) = if wide {
                (CELL_WIDE, CELL_WIDE_SPACER)
            } else {
                (0, 0)
            };
            self.cell(c as u32, st, lead, 0);
            if wide {
                self.cell(0, st, spacer, 0);
            }
        }
        self
    }

    fn cell(&mut self, ch: u32, st: Style, extra_flags: u16, extra: u16) {
        self.b.set_cell(
            self.row,
            self.col,
            Cell {
                ch,
                fg: st.fg,
                bg: st.bg,
                flags: st.flags | extra_flags,
                extra,
            },
        );
        self.col = self.col.saturating_add(1);
    }

    /// 写一个带字形簇文本的单元格；池或 extras 满时退化为只写首码点。
    fn grapheme(&mut self, text: &str, st: Style, hyperlink_id: u32, underline_color: u32) {
        let first = text.chars().next().map_or(0, |c| c as u32);
        let extra = self
            .b
            .push_grapheme(text)
            .ok()
            .and_then(|(off, len)| {
                self.b
                    .push_extra(CellExtra {
                        grapheme_offset: off,
                        grapheme_len: if text.chars().count() > 1 { len } else { 0 },
                        hyperlink_id,
                        underline_color,
                    })
                    .ok()
            })
            .unwrap_or(0);
        self.cell(first, st, 0, extra);
    }
}

fn prompt(r: &mut Row, cmd: &str) {
    let green = Style {
        fg: idx(2),
        bg: 0,
        flags: CELL_BOLD,
    };
    r.text("~/kanso $ ", green).text(cmd, PLAIN);
}

/// 写脚本第 `k` 行，返回该行的 `LINE_*` 附加标志。
fn script_line(r: &mut Row, k: u64) -> u32 {
    match k {
        0 | 6 => {
            prompt(r, if k == 0 { "cargo build" } else { "ls -la" });
            LINE_PROMPT
        }
        1 => {
            r.text("indexed ", PLAIN);
            for i in 0..16u8 {
                let st = Style {
                    fg: idx(i),
                    bg: if i % 4 == 0 { idx(i + 8) } else { 0 },
                    flags: 0,
                };
                r.cell(u32::from(b'A' + i), st, 0, 0);
            }
            0
        }
        2 => {
            r.text("rgb ", PLAIN);
            for i in 0..16u8 {
                let st = Style {
                    fg: rgb(255 - i * 16, i * 16, 128),
                    bg: rgb(i * 8, 0, 255 - i * 8),
                    flags: 0,
                };
                r.cell(u32::from(b'#'), st, 0, 0);
            }
            0
        }
        3 => {
            for (word, class) in [
                ("error", 1u8),
                ("warning", 2),
                ("path/to/file", 3),
                ("42", 4),
            ] {
                let st = Style {
                    fg: Color::semantic(class),
                    bg: 0,
                    flags: 0,
                };
                r.text(word, st).text(" ", PLAIN);
            }
            0
        }
        4 => {
            for (word, flags) in [
                ("bold", CELL_BOLD),
                ("dim", CELL_DIM),
                ("italic", CELL_ITALIC),
                ("bold-italic", CELL_BOLD | CELL_ITALIC),
            ] {
                r.text(word, Style { flags, ..PLAIN }).text(" ", PLAIN);
            }
            0
        }
        5 => {
            for (word, u) in [
                ("single", UNDERLINE_SINGLE),
                ("double", UNDERLINE_DOUBLE),
                ("curly", UNDERLINE_CURLY),
                ("dotted", UNDERLINE_DOTTED),
                ("dashed", UNDERLINE_DASHED),
            ] {
                let flags = u << CELL_UNDERLINE_SHIFT;
                r.text(word, Style { flags, ..PLAIN }).text(" ", PLAIN);
            }
            // 带独立下划线颜色的单元格，走 CellExtra。
            let st = Style {
                flags: UNDERLINE_CURLY << CELL_UNDERLINE_SHIFT,
                ..PLAIN
            };
            r.grapheme("u", st, 0, rgb(255, 0, 0));
            0
        }
        7 => {
            for (word, flags) in [
                ("strike", CELL_STRIKE),
                ("inverse", CELL_INVERSE),
                ("hidden", CELL_HIDDEN),
                ("blink", CELL_BLINK),
            ] {
                r.text(word, Style { flags, ..PLAIN }).text(" ", PLAIN);
            }
            0
        }
        8 => {
            r.text("wide: 你好，世界 end", PLAIN);
            0
        }
        9 => {
            r.text("grapheme: ", PLAIN);
            r.grapheme("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", PLAIN, 0, 0);
            // 宽 emoji 占两格：后半是占位。
            r.cell(0, PLAIN, CELL_WIDE_SPACER, 0);
            r.text(" ", PLAIN);
            r.grapheme("e\u{0301}", PLAIN, 0, 0);
            0
        }
        10 => {
            r.text("link: ", PLAIN);
            let st = Style {
                fg: idx(4),
                bg: 0,
                flags: UNDERLINE_SINGLE << CELL_UNDERLINE_SHIFT,
            };
            for c in "kanso.app".chars() {
                r.grapheme(&c.to_string(), st, 7, 0);
            }
            0
        }
        11 => {
            r.text("sel ", PLAIN);
            r.text(
                "selected",
                Style {
                    flags: CELL_SELECTED,
                    ..PLAIN
                },
            );
            r.text(" m1 ", PLAIN);
            r.text(
                "match",
                Style {
                    flags: CELL_MATCH,
                    ..PLAIN
                },
            );
            r.text(" ", PLAIN);
            r.text(
                "current",
                Style {
                    flags: CELL_MATCH | CELL_MATCH_CURRENT,
                    ..PLAIN
                },
            );
            0
        }
        12 => {
            // 写满整行，模拟软换行。
            let cols = usize::from(r.cols);
            let s: String = "wrapped line ".chars().cycle().take(cols).collect();
            r.text(&s, PLAIN);
            LINE_WRAPPED
        }
        _ => {
            r.text("plain output line", PLAIN);
            0
        }
    }
}

/// 生成 `cols x rows` 的 mock 帧。任意尺寸（含 0）都不会 panic。
///
/// 视口第 `row` 行显示脚本第 `(seq + row) % 14` 行，绝对行号为 `seq + row`；
/// 绝对行号 0 的提示符带 `LINE_FIRST_PROMPT`。最后两行（行数大于 4 时）保持空白，
/// 光标在其上一行。
pub fn mock_frame(cols: u16, rows: u16, seq: u64) -> Frame {
    let mut b = FrameBuilder::new(cols, rows);
    b.set_seq(seq);
    b.set_line_numbers(seq, 0);
    b.set_scroll(0, 1000);

    let blank_from = if rows > 4 { rows - 2 } else { rows };
    let cursor_row = blank_from.saturating_sub(1).min(rows.saturating_sub(1));
    let mut flags = FRAME_FOCUSED;
    if rows > 0 && cols > 0 {
        flags |= FRAME_CURSOR_VISIBLE | FRAME_CURSOR_BLINK;
    }
    b.set_flags(flags);
    b.set_cursor(0, cursor_row, CURSOR_BLOCK);

    for row in 0..rows {
        let abs = seq + u64::from(row);
        let mut line_flags = LINE_DIRTY;
        let mut ts = 0i64;
        if row >= blank_from {
            line_flags |= LINE_BLANK;
        } else {
            ts = BASE_TIME_MS.saturating_add((abs as i64).saturating_mul(1000))
                + (abs % 1000) as i64;
            let mut r = Row {
                b: &mut b,
                row,
                col: 0,
                cols,
            };
            line_flags |= script_line(&mut r, abs % SCRIPT_LEN);
            if line_flags & LINE_PROMPT != 0 && abs == 0 {
                line_flags |= LINE_FIRST_PROMPT;
            }
            if row == cursor_row {
                line_flags |= LINE_CURSOR;
            }
        }
        if let Some(m) = b.line_meta_mut(row) {
            m.abs_line = abs;
            m.timestamp_ms = ts;
            m.flags = line_flags;
        }
    }
    b.finish()
}

/// 命令块截图用帧：成功命令、失败命令、下一提示符，复用普通帧的网格写入器。
pub fn mock_command_blocks_frame(cols: u16, rows: u16) -> Frame {
    let mut builder = FrameBuilder::new(cols, rows);
    builder.set_flags(FRAME_FOCUSED);
    let lines = ["$ echo hello", "hello", "", "$ false", "", "", "$ "];
    for row in 0..rows {
        if let Some(text) = lines.get(usize::from(row)) {
            Row {
                b: &mut builder,
                row,
                col: 0,
                cols,
            }
            .text(text, PLAIN);
        }
        let meta = builder.line_meta_mut(row).expect("行号在网格内");
        meta.abs_line = u64::from(row);
        meta.timestamp_ms = BASE_TIME_MS + i64::from(row) * 1_000;
        meta.flags = if [0, 3, 6].contains(&row) {
            LINE_PROMPT
        } else {
            0
        };
        if row == 0 {
            meta.flags |= LINE_FIRST_PROMPT;
        }
    }
    let mut frame = builder.finish();
    frame.command_blocks = [(1, 0, 2, Some(0)), (2, 3, 5, Some(1)), (3, 6, 6, None)]
        .into_iter()
        .filter(|(_, start, _, _)| *start < u64::from(rows))
        .map(|(id, start, end, exit_code)| crate::CommandBlock {
            id,
            latest: id == 3,
            start_line: start,
            end_line: end.min(u64::from(rows - 1)),
            command_line: exit_code.map(|_| start),
            running: false,
            exit_code,
            duration: exit_code.map(|_| kanso_types::Message::CommandDurationMilliseconds {
                value: "320".into(),
            }),
            can_copy_command: exit_code.is_some(),
            can_copy_output: exit_code.is_some(),
            foldable: exit_code.is_some() && end > start,
            folded: false,
        })
        .collect();
    frame
}

/// 生成 `cols x rows` 的"满屏彩色文字"mock 帧，供渲染器性能测试使用。
///
/// 每个单元格都是可见 ASCII 字符，前景为随位置变化的 RGB 色，约 1/8 的单元格带索引色背景；
/// 所有行都带时间戳，无空白行。与 [`mock_frame`] 一样确定性，任意尺寸（含 0）都不会 panic。
pub fn mock_frame_dense(cols: u16, rows: u16, seq: u64) -> Frame {
    let mut b = FrameBuilder::new(cols, rows);
    b.set_seq(seq);
    b.set_line_numbers(seq, 0);
    b.set_scroll(0, 1000);
    let mut flags = FRAME_FOCUSED;
    if rows > 0 && cols > 0 {
        flags |= FRAME_CURSOR_VISIBLE;
    }
    b.set_flags(flags);
    let cursor_row = rows / 2;
    b.set_cursor(cols / 2, cursor_row, CURSOR_BLOCK);

    for row in 0..rows {
        let abs = seq + u64::from(row);
        let mut r = Row {
            b: &mut b,
            row,
            col: 0,
            cols,
        };
        for col in 0..cols {
            // 取 0x21..=0x7E 的可见字符，避免空格（空格不产生字形，会让测试偏轻）。
            let n = (u32::from(row) * 31 + u32::from(col) * 7 + seq as u32) % 94;
            let ch = 0x21 + n;
            let (cr, cg, cb) = (
                (u32::from(col) * 5 % 256) as u8,
                (u32::from(row) * 13 % 256) as u8,
                (255 - u32::from(col) * 3 % 256) as u8,
            );
            let st = Style {
                fg: rgb(cr, cg, cb),
                bg: if (u32::from(col) + u32::from(row)).is_multiple_of(8) {
                    idx((n % 16) as u8)
                } else {
                    0
                },
                flags: if n.is_multiple_of(11) { CELL_BOLD } else { 0 },
            };
            r.cell(ch, st, 0, 0);
        }
        let mut line_flags = LINE_DIRTY;
        if row == cursor_row {
            line_flags |= LINE_CURSOR;
        }
        if let Some(m) = b.line_meta_mut(row) {
            m.abs_line = abs;
            m.timestamp_ms = BASE_TIME_MS.saturating_add((abs as i64).saturating_mul(1000));
            m.flags = line_flags;
        }
    }
    b.finish()
}
