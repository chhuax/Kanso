//! kanso-term 的测试：terminal.md §18 中属于 T4a 的验收用例，外加侧表与转发的一致性检查。

use alacritty_terminal::event::Event;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use kanso_frame::{
    CELL_MATCH, CELL_MATCH_CURRENT, CELL_SELECTED, CELL_WIDE, CELL_WIDE_SPACER, CURSOR_BAR,
    CURSOR_BLOCK, CURSOR_HOLLOW_BLOCK, CURSOR_UNDERLINE, FRAME_ALT_SCREEN, FRAME_CURSOR_BLINK,
    FRAME_CURSOR_VISIBLE, FRAME_FOCUSED, FRAME_MOUSE_REPORTING, Frame, FrameView, LINE_BLANK,
    LINE_CURSOR, LINE_FIRST_PROMPT, LINE_PROMPT, LINE_WRAPPED,
};
use kanso_types::geometry::{AbsLine, CellPoint, GridSize};
use vte::ansi::Processor;

use crate::handler::{Dims, EventSink, NoSync, alacritty_config};
use crate::*;

fn term(cols: u16, rows: u16, scrollback: usize) -> Terminal {
    Terminal::new(TermConfig {
        size: GridSize { cols, rows },
        scrollback_lines: scrollback,
    })
}

fn feed(t: &mut Terminal, s: &str, now: i64) {
    t.feed(s.as_bytes(), now);
}

fn snap(t: &mut Terminal) -> Frame {
    t.snapshot(None)
}

#[test]
fn chip_markers_survive_sgr_and_chunk_boundaries() {
    let bytes = "x\x1b]777;kanso-chip;start\x07a\x1b[0m中b\x1b]777;kanso-chip;end\x07y".as_bytes();
    for split in 0..=bytes.len() {
        let mut terminal = term(20, 3, 20);
        terminal.feed(&bytes[..split], 0);
        terminal.feed(&bytes[split..], 0);
        let frame = snap(&mut terminal);
        let view = frame.view().unwrap();
        let flags: Vec<_> = view.row(0)[..6]
            .iter()
            .map(|cell| cell.flags & kanso_frame::CELL_CHIP != 0)
            .collect();
        assert_eq!(
            flags,
            [false, true, true, true, true, false],
            "split={split}"
        );
    }
}

#[test]
fn chip_ends_on_newline_or_next_prompt_and_ignores_unknown_commands() {
    for ending in ["\r\n", "\x1bE", "\x1b]133;A\x07"] {
        let mut terminal = term(20, 3, 20);
        feed(
            &mut terminal,
            &format!("\x1b]777;kanso-chip;start\x07a{ending}b"),
            0,
        );
        let frame = snap(&mut terminal);
        let view = frame.view().unwrap();
        for row in 0..3 {
            for cell in view.row(row) {
                if cell.ch == u32::from('a') {
                    assert_ne!(cell.flags & kanso_frame::CELL_CHIP, 0);
                } else {
                    assert_eq!(cell.flags & kanso_frame::CELL_CHIP, 0);
                }
            }
        }
    }
    let mut terminal = term(20, 3, 20);
    feed(
        &mut terminal,
        "\x1b]777;unknown;start\x07x\x1b]777;kanso-chip;start\x07a\x1b]777;kanso-chip;unknown\x07b\x1b]777;kanso-chip;end\x07y",
        0,
    );
    let frame = snap(&mut terminal);
    let view = frame.view().unwrap();
    assert_eq!(view.row(0)[0].flags & kanso_frame::CELL_CHIP, 0);
    assert_ne!(view.row(0)[2].flags & kanso_frame::CELL_CHIP, 0);
    assert_eq!(view.row(0)[3].flags & kanso_frame::CELL_CHIP, 0);
}

#[test]
fn chip_cells_keep_attributes_through_reflow_and_scrollback() {
    let mut terminal = term(20, 3, 20);
    feed(
        &mut terminal,
        "\x1b]777;kanso-chip;start\x07abcdefghijk\x1b]777;kanso-chip;end\x07\r\nplain\r\nnext\r\nlast",
        0,
    );
    terminal.resize(GridSize { cols: 6, rows: 3 });
    terminal.scroll_display(20);
    let frame = snap(&mut terminal);
    let view = frame.view().unwrap();
    let chips: String = (0..3)
        .flat_map(|row| view.row(row))
        .filter(|cell| cell.flags & kanso_frame::CELL_CHIP != 0)
        .filter_map(|cell| char::from_u32(cell.ch))
        .collect();
    assert_eq!(chips, "abcdefghijk");
}

/// 一行的文本（空单元格当作空格，行尾空白去掉，宽字符占位格跳过，字形簇展开）。
fn row_text(v: &FrameView<'_>, r: u16) -> String {
    let mut s = String::new();
    for c in v.row(r) {
        if c.flags & CELL_WIDE_SPACER != 0 {
            continue;
        }
        if let Some(g) = v.extra(c).and_then(|e| v.grapheme(e)) {
            s.push_str(g);
        } else if c.ch != 0 {
            s.push(char::from_u32(c.ch).unwrap());
        } else {
            s.push(' ');
        }
    }
    s.trim_end().to_owned()
}

fn find_row(v: &FrameView<'_>, needle: &str) -> Option<u16> {
    (0..v.header().rows).find(|&r| row_text(v, r).starts_with(needle))
}

fn lines(t: &mut Terminal, n: usize, now: i64) {
    for i in 1..=n {
        feed(t, &format!("L{i}\r\n"), now + i as i64);
    }
}

fn assert_consistent(t: &Terminal) {
    assert_eq!(t.core.resync_count, 0, "行元数据侧表与网格曾经错位");
}

#[test]
fn terminal_is_send() {
    fn is_send<T: Send>() {}
    is_send::<Terminal>();
}

// ---------------------------------------------------------------- 绝对行号

#[test]
fn abs_line_keeps_growing_after_scrollback_full() {
    let mut t = term(40, 10, 100);
    lines(&mut t, 1000, 1000);
    assert_consistent(&t);
    assert!(t.line_meta_len() <= 100 + 10);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let h = v.header();
    assert_eq!(h.scrollback_lines, 100);
    // 第 1000 行输出后光标在新的一行：最底行是第 1001 个物理行（abs 1000）。
    let bottom = v.line_meta(9).unwrap();
    assert_eq!(bottom.abs_line, h.first_abs_line + 9);
    assert_eq!(bottom.abs_line, 1000);
    // “L1000”那一行的显示行号是 1000。
    let r = find_row(&v, "L1000").unwrap();
    let m = v.line_meta(r).unwrap();
    assert_eq!(m.abs_line - h.line_number_base + 1, 1000);
    assert_eq!(m.abs_line, 999);
}

// ---------------------------------------------------------------- clear 语义

fn display_number(v: &FrameView<'_>, r: u16) -> u64 {
    v.line_meta(r).unwrap().abs_line - v.header().line_number_base + 1
}

#[test]
fn clear_linux_order_clears_scrollback_and_restarts_numbers() {
    let mut t = term(40, 10, 100);
    lines(&mut t, 50, 1000);
    feed(&mut t, "\x1b[H\x1b[2J\x1b[3Jnew\r\n", 2000);
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().scrollback_lines, 0);
    assert_eq!(row_text(&v, 0), "new");
    assert_eq!(display_number(&v, 0), 1);
    assert!(find_row(&v, "L50").is_none());
}

#[test]
fn clear_macos_order_clears_scrollback_and_restarts_numbers() {
    let mut t = term(40, 10, 100);
    lines(&mut t, 50, 1000);
    feed(&mut t, "\x1b[3J\x1b[H\x1b[2Jnew\r\n", 2000);
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    // ED 3 已经清掉了回滚，紧接着的 ED 2 不能把屏幕推进回滚。
    assert_eq!(v.header().scrollback_lines, 0);
    assert_eq!(row_text(&v, 0), "new");
    assert_eq!(display_number(&v, 0), 1);
    for r in 1..10 {
        assert_eq!(row_text(&v, r), "");
    }
    // AbsLine 本身没有回退。
    assert_eq!(v.line_meta(0).unwrap().abs_line, 41);
}

#[test]
fn lone_ed2_pushes_screen_into_scrollback() {
    let mut t = term(40, 10, 100);
    lines(&mut t, 50, 1000);
    feed(&mut t, "\x1b[H\x1b[2J", 2000);
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert!(v.header().scrollback_lines > 0);
    assert_eq!(row_text(&v, 0), "");
    // 之前屏幕上的内容出现在回滚里。
    t.scroll_display(10);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert!(find_row(&v, "L50").is_some() || find_row(&v, "L49").is_some());
    // 行号没有重置。
    assert_eq!(v.header().line_number_base, 0);
}

#[test]
fn same_batch_ed2_then_first_frame_is_kept_and_next_batch_ed2_keeps_scrollback() {
    let mut t = term(40, 10, 100);
    lines(&mut t, 30, 1000);
    // top：同一批里先清屏再画第一帧。
    feed(&mut t, "\x1b[H\x1b[2Jframe-1\x1b[2;1Hrow2", 2000);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(row_text(&v, 0), "frame-1");
    assert_eq!(row_text(&v, 1), "row2");
    assert!(v.header().scrollback_lines > 0);
    let before = v.header().scrollback_lines;
    // 下一批里单独的 ED 2：别的程序重绘，保留回滚，并把 frame-1 推进回滚。
    feed(&mut t, "\x1b[H\x1b[2J", 3000);
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert!(v.header().scrollback_lines > before);
    t.scroll_display(5);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert!(find_row(&v, "frame-1").is_some());
}

#[test]
fn ed2_with_scroll_region_just_erases() {
    let mut t = term(20, 6, 50);
    lines(&mut t, 20, 1000);
    feed(&mut t, "\x1b[2;5r\x1b[2J", 2000);
    assert_consistent(&t);
    let f = snap(&mut t);
    assert_eq!(f.view().unwrap().header().scrollback_lines, 15);
}

// ---------------------------------------------------------------- OSC 133

#[test]
fn repeated_prompt_marker_on_same_line_counts_once() {
    let mut t = term(40, 10, 100);
    feed(&mut t, "\x1b]133;A;kanso=first\x07$ ", 100);
    feed(&mut t, "\r\x1b]133;A\x07$ ", 101); // zsh 重绘
    feed(
        &mut t,
        "\x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;2\x07",
        102,
    );
    feed(&mut t, "\r\n\x1b]133;A\x07$ ", 103);
    let ev = t.take_events();
    assert_eq!(
        ev,
        vec![
            TermEvent::PromptStart {
                line: AbsLine(0),
                first: true
            },
            TermEvent::InputStart { line: AbsLine(0) },
            TermEvent::CommandStart { line: AbsLine(1) },
            TermEvent::CommandFinished { exit_code: Some(2) },
            TermEvent::PromptStart {
                line: AbsLine(3),
                first: false
            },
        ]
    );
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let m0 = v.line_meta(0).unwrap();
    assert_eq!(
        m0.flags & (LINE_PROMPT | LINE_FIRST_PROMPT),
        LINE_PROMPT | LINE_FIRST_PROMPT
    );
    assert_eq!(v.line_meta(1).unwrap().flags & LINE_PROMPT, 0);
    let m3 = v.line_meta(3).unwrap();
    assert_eq!(m3.flags & (LINE_PROMPT | LINE_FIRST_PROMPT), LINE_PROMPT);
}

#[test]
fn prompt_is_recorded_at_the_line_where_the_sequence_occurs() {
    // 同一批里先输出 3 行再发 A：标记必须落在第 4 行，而不是批末光标所在的行。
    let mut t = term(40, 10, 100);
    feed(&mut t, "a\r\nb\r\nc\r\n\x1b]133;A\x07$ \r\nlater\r\n", 100);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_ne!(v.line_meta(3).unwrap().flags & LINE_PROMPT, 0);
    assert_eq!(v.line_meta(5).unwrap().flags & LINE_PROMPT, 0);
}

#[test]
fn osc133_works_across_feed_boundaries() {
    let mut t = term(40, 10, 100);
    feed(&mut t, "x\r\n\x1b]13", 1);
    feed(&mut t, "3;A\x07$", 2);
    assert_eq!(
        t.take_events(),
        vec![TermEvent::PromptStart {
            line: AbsLine(1),
            first: false
        }]
    );
}

// ---------------------------------------------------------------- 工作目录

#[test]
fn cwd_reports() {
    let mut t = term(40, 5, 10);
    feed(
        &mut t,
        "\x1b]7;file://mac.local/Users/a%20b/%E4%B8%AD\x07",
        1,
    );
    let c = t.cwd().unwrap();
    assert_eq!(c.host.as_deref(), Some("mac.local"));
    assert_eq!(c.path, "/Users/a b/中");
    // 没有编码（直接写 $PWD）：按原样。
    feed(&mut t, "\x1b]7;file://h/tmp/a b\x1b\\", 2);
    assert_eq!(t.cwd().unwrap().path, "/tmp/a b");
    // 解码失败：按原样。
    feed(&mut t, "\x1b]7;file:///tmp/100%zz\x07", 3);
    assert_eq!(t.cwd().unwrap().path, "/tmp/100%zz");
    feed(&mut t, "\x1b]1337;CurrentDir=/var/log\x07", 4);
    assert_eq!(t.cwd().unwrap().path, "/var/log");
    assert_eq!(t.cwd().unwrap().host, None);
    feed(&mut t, "\x1b]9;9;\"C:\\Users\\me\"\x07", 5);
    assert_eq!(t.cwd().unwrap().path, "C:\\Users\\me");
    feed(&mut t, "\x1b]9;9;/home/me\x07", 6);
    assert_eq!(t.cwd().unwrap().path, "/home/me");
    // 其它 OSC 9 不是工作目录。
    feed(&mut t, "\x1b]9;4;1;50\x07\x1b]9;hello\x07", 7);
    assert_eq!(t.cwd().unwrap().path, "/home/me");
    let n = t
        .take_events()
        .iter()
        .filter(|e| matches!(e, TermEvent::CwdChanged(_)))
        .count();
    assert_eq!(n, 6);
}

// ---------------------------------------------------------------- 全屏

#[test]
fn alt_screen_flags_and_restore() {
    let mut t = term(20, 5, 50);
    feed(&mut t, "main\r\nline2", 1);
    assert!(!t.modes().alt_screen);
    feed(&mut t, "\x1b[?1049h\x1b[Hvim", 2);
    assert!(t.modes().alt_screen);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_ne!(v.header().flags & FRAME_ALT_SCREEN, 0);
    assert_eq!(v.header().scrollback_lines, 0);
    assert_eq!(row_text(&v, 0), "vim");
    assert_ne!(v.line_meta(0).unwrap().flags & LINE_BLANK, 0);
    feed(&mut t, "\x1b[?1049l", 3);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().flags & FRAME_ALT_SCREEN, 0);
    assert_eq!(row_text(&v, 0), "main");
    assert_eq!(row_text(&v, 1), "line2");
    // 全屏期间不污染主屏幕的行元数据。
    assert_eq!(v.line_meta(0).unwrap().timestamp_ms, 1);
    assert_consistent(&t);
}

// ---------------------------------------------------------------- 重排

#[test]
fn prompt_marker_follows_soft_wrap_reflow() {
    let mut t = term(30, 10, 100);
    lines(&mut t, 3, 10);
    // 提示符行很长，窄屏后会折成多行。
    feed(
        &mut t,
        "\x1b]133;A\x07$ cargo build --release --workspace --all-features\r\nout\r\n",
        50,
    );
    feed(&mut t, "\x1b]133;A\x07$ ", 60);
    let ev = t.take_events();
    assert_eq!(ev.len(), 2);
    t.resize(GridSize { cols: 10, rows: 20 });
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let r = find_row(&v, "$ cargo").unwrap();
    let m = v.line_meta(r).unwrap();
    assert_ne!(m.flags & LINE_PROMPT, 0, "提示符标记必须留在提示符的第一行");
    assert_eq!(m.timestamp_ms, 50);
    assert_ne!(m.flags & LINE_WRAPPED, 0);
    // 续行没有提示符标记，但保留时间。
    let cont = v.line_meta(r + 1).unwrap();
    assert_eq!(cont.flags & LINE_PROMPT, 0);
    assert_eq!(cont.timestamp_ms, 50);
    // 恰好两个提示符标记。
    let n = (0..20)
        .filter(|&i| v.line_meta(i).unwrap().flags & LINE_PROMPT != 0)
        .count();
    assert_eq!(n, 2);
    // 变宽后重新合并，标记仍然在位。
    t.resize(GridSize { cols: 60, rows: 20 });
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let r = find_row(&v, "$ cargo").unwrap();
    assert_ne!(v.line_meta(r).unwrap().flags & LINE_PROMPT, 0);
    let n = (0..20)
        .filter(|&i| v.line_meta(i).unwrap().flags & LINE_PROMPT != 0)
        .count();
    assert_eq!(n, 2);
}

#[test]
fn reflow_with_full_scrollback_keeps_alignment() {
    let mut t = term(40, 8, 30);
    for i in 0..200 {
        feed(
            &mut t,
            &format!("\x1b]133;A\x07$ cmd{i} {}\r\n", "x".repeat(30)),
            100 + i,
        );
    }
    t.resize(GridSize { cols: 12, rows: 8 });
    assert_consistent(&t);
    t.scroll_to_bottom();
    let f = snap(&mut t);
    let v = f.view().unwrap();
    // 屏幕上每个以 "$ cmd" 开头的行都带提示符标记，时间戳与命令编号对应。
    let mut seen = 0;
    for r in 0..8 {
        let txt = row_text(&v, r);
        if let Some(rest) = txt.strip_prefix("$ cmd") {
            let n: i64 = rest.split(' ').next().unwrap().parse().unwrap();
            let m = v.line_meta(r).unwrap();
            assert_ne!(m.flags & LINE_PROMPT, 0);
            assert_eq!(m.timestamp_ms, 100 + n);
            seen += 1;
        }
    }
    assert!(seen >= 1);
}

#[test]
fn resize_while_alt_screen_remaps_on_exit() {
    let mut t = term(30, 8, 50);
    feed(
        &mut t,
        "\x1b]133;A\x07$ a-very-long-prompt-line-that-wraps-on-narrow\r\n",
        5,
    );
    feed(&mut t, "\x1b[?1049hvim", 6);
    t.resize(GridSize { cols: 10, rows: 8 });
    feed(&mut t, "\x1b[?1049l", 7);
    assert_consistent(&t);
    // 重排把提示符的前几行挤进了回滚，往上翻才看得到。
    t.scroll_display(10);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let r = find_row(&v, "$ a-very").unwrap();
    let m = v.line_meta(r).unwrap();
    assert_ne!(m.flags & LINE_PROMPT, 0);
    assert_eq!(m.timestamp_ms, 5);
}

#[test]
fn height_resize_preserves_metadata_without_full_remap() {
    let mut terminal = term(30, 8, 100);
    for index in 0..80 {
        feed(
            &mut terminal,
            &format!("\x1b]133;A\x07$ cmd{index}\r\n"),
            100 + index,
        );
    }
    for rows in [4, 12, 3, 20, 8] {
        terminal.resize(GridSize { cols: 30, rows });
        assert_consistent(&terminal);
        let frame = snap(&mut terminal);
        let view = frame.view().unwrap();
        for row in 0..rows {
            if let Some(command) = row_text(&view, row).strip_prefix("$ cmd") {
                let index: i64 = command.parse().unwrap();
                let meta = view.line_meta(row).unwrap();
                assert_eq!(meta.timestamp_ms, 100 + index);
                assert_ne!(meta.flags & LINE_PROMPT, 0);
            }
        }
    }
    feed(&mut terminal, "\x1b[?1049hvim", 200);
    for rows in [4, 12, 8] {
        terminal.resize(GridSize { cols: 30, rows });
    }
    feed(&mut terminal, "\x1b[?1049l", 201);
    assert_consistent(&terminal);
    assert_eq!(terminal.core.full_remaps, 0);
    terminal.resize(GridSize { cols: 15, rows: 8 });
    assert_eq!(terminal.core.full_remaps, 1);
}

// ---------------------------------------------------------------- 单元格编码

#[test]
fn wide_combining_hyperlink_cells() {
    let mut t = term(20, 4, 10);
    feed(
        &mut t,
        "中a\u{301}b\x1b]8;id=1;http://example.com/x\x07link\x1b]8;;\x07z",
        1,
    );
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let row = v.row(0);
    assert_eq!(row[0].ch, '中' as u32);
    assert_ne!(row[0].flags & CELL_WIDE, 0);
    assert_eq!(row[1].ch, 0);
    assert_ne!(row[1].flags & CELL_WIDE_SPACER, 0);
    assert_eq!(row[2].ch, 'a' as u32);
    let g = v.grapheme(v.extra(&row[2]).unwrap()).unwrap();
    assert_eq!(g, "a\u{301}");
    assert_eq!(row[3].ch, 'b' as u32);
    assert!(v.extra(&row[3]).is_none());
    let e = v.extra(&row[4]).unwrap();
    assert_ne!(e.hyperlink_id, 0);
    assert_eq!(
        t.hyperlink_url(e.hyperlink_id),
        Some("http://example.com/x")
    );
    assert_eq!(v.extra(&row[7]).unwrap().hyperlink_id, e.hyperlink_id);
    assert!(v.extra(&row[8]).is_none());
}

#[test]
fn colors_and_styles_are_encoded_not_resolved() {
    use kanso_frame::{
        CELL_BOLD, CELL_INVERSE, CELL_ITALIC, CELL_STRIKE, CELL_UNDERLINE_MASK,
        CELL_UNDERLINE_SHIFT, Color, DecodedColor, UNDERLINE_CURLY,
    };
    use kanso_types::geometry::Rgb;
    let mut t = term(20, 3, 10);
    feed(
        &mut t,
        "\x1b[1;3;9;7;4:3;31;48;2;1;2;3mX\x1b[0m\x1b[38;5;200mY\x1b[0mZ",
        1,
    );
    let f = snap(&mut t);
    let v = f.view().unwrap();
    let x = v.row(0)[0];
    assert_eq!(
        x.flags & (CELL_BOLD | CELL_ITALIC | CELL_STRIKE | CELL_INVERSE),
        CELL_BOLD | CELL_ITALIC | CELL_STRIKE | CELL_INVERSE
    );
    assert_eq!(
        (x.flags & CELL_UNDERLINE_MASK) >> CELL_UNDERLINE_SHIFT,
        UNDERLINE_CURLY
    );
    assert_eq!(Color::decode(x.fg), DecodedColor::Indexed(1));
    assert_eq!(
        Color::decode(x.bg),
        DecodedColor::Rgb(Rgb { r: 1, g: 2, b: 3 })
    );
    assert_eq!(Color::decode(v.row(0)[1].fg), DecodedColor::Indexed(200));
    assert_eq!(Color::decode(v.row(0)[2].fg), DecodedColor::Default);
}

// ---------------------------------------------------------------- 回应、事件、模式

#[test]
fn device_attributes_and_reports_go_to_replies() {
    let mut t = term(20, 5, 10);
    feed(&mut t, "\x1b[c", 1);
    assert_eq!(t.take_replies(), b"\x1b[?6c");
    assert!(t.take_replies().is_empty());
    feed(&mut t, "ab\x1b[6n\x1b[5n", 2);
    assert_eq!(t.take_replies(), b"\x1b[1;3R\x1b[0n");
    // 颜色查询：未设置主题时用默认值，程序改过的颜色优先。
    t.set_theme_colors(ThemeColors {
        background: kanso_types::geometry::Rgb {
            r: 0x10,
            g: 0x20,
            b: 0x30,
        },
        ..ThemeColors::default()
    });
    feed(&mut t, "\x1b]11;?\x07", 3);
    assert_eq!(t.take_replies(), b"\x1b]11;rgb:1010/2020/3030\x07");
    feed(&mut t, "\x1b]4;1;rgb:ff/00/00\x07\x1b]4;1;?\x1b\\", 4);
    assert_eq!(t.take_replies(), b"\x1b]4;1;rgb:ffff/0000/0000\x1b\\");
}

#[test]
fn focus_reporting_and_cursor_shape() {
    let mut t = term(20, 5, 10);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().cursor_shape, CURSOR_HOLLOW_BLOCK);
    assert_eq!(v.header().flags & FRAME_FOCUSED, 0);
    t.set_focused(true);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().cursor_shape, CURSOR_BLOCK);
    assert_ne!(v.header().flags & (FRAME_FOCUSED | FRAME_CURSOR_VISIBLE), 0);
    assert!(t.take_replies().is_empty());
    feed(&mut t, "\x1b[?1004h", 1);
    t.set_focused(false);
    assert_eq!(t.take_replies(), b"\x1b[O");
    t.set_focused(true);
    assert_eq!(t.take_replies(), b"\x1b[I");
}

#[test]
fn modes_and_events() {
    let mut t = term(20, 5, 10);
    feed(
        &mut t,
        "\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h",
        1,
    );
    let m = t.modes();
    assert!(m.app_cursor && m.bracketed_paste && m.focus_reporting && m.mouse_sgr);
    assert_eq!(m.mouse, MouseMode::Click);
    let f = snap(&mut t);
    assert_ne!(f.view().unwrap().header().flags & FRAME_MOUSE_REPORTING, 0);
    feed(
        &mut t,
        "\x1b]2;hello\x07\x07\x07\x07\x1b]52;c;aGVsbG8=\x07",
        2,
    );
    assert_eq!(t.title(), Some("hello"));
    assert_eq!(
        t.take_events(),
        vec![
            TermEvent::TitleChanged,
            TermEvent::Bell,
            TermEvent::ClipboardStore("hello".into())
        ]
    );
    // kitty 键盘协议由程序主动开启。
    feed(&mut t, "\x1b[>1u", 3);
    assert_eq!(
        t.modes().kitty_keyboard & kitty::DISAMBIGUATE,
        kitty::DISAMBIGUATE
    );
}

// ---------------------------------------------------------------- 时间戳

#[test]
fn timestamps_first_write_wins_and_blank_rows() {
    let mut t = term(20, 5, 10);
    feed(&mut t, "a", 100);
    feed(&mut t, "\rbcd", 200);
    feed(&mut t, "\r\nx", 300);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.line_meta(0).unwrap().timestamp_ms, 100);
    assert_eq!(v.line_meta(1).unwrap().timestamp_ms, 300);
    assert_eq!(v.line_meta(0).unwrap().flags & LINE_BLANK, 0);
    for r in 2..5 {
        assert_ne!(v.line_meta(r).unwrap().flags & LINE_BLANK, 0);
        assert_eq!(v.line_meta(r).unwrap().timestamp_ms, 0);
    }
    assert_ne!(v.line_meta(1).unwrap().flags & LINE_CURSOR, 0);
}

#[test]
fn line_meta_stays_bounded_and_fast() {
    let mut t = term(80, 24, 10_000);
    let start = std::time::Instant::now();
    let chunk: String = (0..1000).map(|i| format!("line {i}\r\n")).collect();
    for k in 0..200 {
        feed(&mut t, &chunk, k);
        assert!(t.line_meta_len() <= 10_000 + 24);
    }
    assert_consistent(&t);
    assert!(
        start.elapsed().as_secs() < 20,
        "200k 行耗时 {:?}",
        start.elapsed()
    );
    let f = snap(&mut t);
    let h = f.view().unwrap().header();
    // 光标在第 200000 个物理行（abs 200000）的最底行，所以屏幕顶是 200000 - 23。
    assert_eq!(h.first_abs_line, 200_000 - 23);
}

// ---------------------------------------------------------------- 滚动、设置、⌘K

#[test]
fn scroll_display_and_scrollback_shrink() {
    let mut t = term(20, 5, 100);
    lines(&mut t, 300, 1);
    assert_eq!(snap(&mut t).view().unwrap().header().scrollback_lines, 100);
    t.scroll_display(10);
    let f = snap(&mut t);
    assert_eq!(f.view().unwrap().header().display_offset, 10);
    let top_abs = f.view().unwrap().header().first_abs_line;
    t.scroll_to_bottom();
    let f = snap(&mut t);
    assert_eq!(f.view().unwrap().header().first_abs_line, top_abs + 10);
    t.set_scrollback_lines(10);
    assert!(t.is_dirty());
    let f = snap(&mut t);
    assert_eq!(f.view().unwrap().header().scrollback_lines, 10);
    assert_eq!(t.line_meta_len(), 15);
    assert_consistent(&t);
    assert!(!t.is_dirty());
    assert!(t.take_events().is_empty(), "调整回滚不应产生标题事件");
}

fn cursor_of(t: &mut Terminal) -> (u8, bool) {
    let f = snap(t);
    let v = f.view().unwrap();
    (
        v.header().cursor_shape,
        v.header().flags & FRAME_CURSOR_BLINK != 0,
    )
}

#[test]
fn default_cursor_style_applies_and_decscusr_zero_returns_to_it() {
    use kanso_types::CursorShape as Shape;
    let mut t = term(20, 5, 10);
    t.set_focused(true);
    assert_eq!(cursor_of(&mut t), (CURSOR_BLOCK, false));

    t.set_default_cursor(Shape::Bar, true);
    assert!(t.is_dirty());
    assert_eq!(cursor_of(&mut t), (CURSOR_BAR, true));

    // 程序指定的样式（DECSCUSR 4：稳定下划线）优先；之后改默认值不影响它。
    feed(&mut t, "\x1b[4 q", 1);
    assert_eq!(cursor_of(&mut t), (CURSOR_UNDERLINE, false));
    t.set_default_cursor(Shape::Block, false);
    assert_eq!(cursor_of(&mut t), (CURSOR_UNDERLINE, false));

    // DECSCUSR 0 回到当前默认值，而不是 alacritty 的内置默认。
    t.set_default_cursor(Shape::Bar, true);
    feed(&mut t, "\x1b[0 q", 2);
    assert_eq!(cursor_of(&mut t), (CURSOR_BAR, true));
}

#[test]
fn default_cursor_survives_scrollback_changes() {
    use kanso_types::CursorShape as Shape;
    let mut t = term(20, 5, 100);
    t.set_focused(true);
    t.set_default_cursor(Shape::Underline, true);
    // `set_options` 整体替换配置：调回滚上限不能把默认光标样式冲掉。
    t.set_scrollback_lines(20);
    assert_eq!(cursor_of(&mut t), (CURSOR_UNDERLINE, true));
    t.clear_buffer();
    assert_eq!(cursor_of(&mut t), (CURSOR_UNDERLINE, true));
}

#[test]
fn shrinking_scrollback_drops_oldest_lines_immediately() {
    let mut t = term(20, 5, 1000);
    lines(&mut t, 300, 1);
    t.set_scrollback_lines(50);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().scrollback_lines, 50);
    // 屏幕 5 行 + 回滚 50 行；最老的行已被丢弃。
    assert_eq!(t.line_meta_len(), 55);
}

#[test]
fn clear_buffer_resets_everything() {
    let mut t = term(20, 5, 100);
    lines(&mut t, 100, 1);
    feed(&mut t, "\x1b]133;A\x07$ ", 5);
    t.clear_buffer();
    assert!(t.is_dirty());
    feed(&mut t, "hi", 9);
    assert_consistent(&t);
    let f = snap(&mut t);
    let v = f.view().unwrap();
    assert_eq!(v.header().scrollback_lines, 0);
    assert_eq!(row_text(&v, 0), "hi");
    assert_eq!(display_number(&v, 0), 1);
    assert_eq!(v.line_meta(0).unwrap().flags & LINE_PROMPT, 0);
    assert_eq!(v.line_meta(0).unwrap().abs_line, 96);
    assert_eq!(t.line_meta_len(), 5);
}

#[test]
fn dirty_flag() {
    let mut t = term(20, 5, 100);
    assert!(t.is_dirty());
    snap(&mut t);
    assert!(!t.is_dirty());
    feed(&mut t, "x", 1);
    assert!(t.is_dirty());
    snap(&mut t);
    t.resize(GridSize { cols: 30, rows: 6 });
    assert!(t.is_dirty());
    snap(&mut t);
    t.scroll_display(1); // 没有回滚，不变
    assert!(!t.is_dirty());
    lines(&mut t, 20, 1);
    snap(&mut t);
    t.scroll_display(1);
    assert!(t.is_dirty());
    snap(&mut t);
    t.clear_buffer();
    assert!(t.is_dirty());
}

#[test]
fn overlays_mark_cells() {
    let mut t = term(10, 3, 10);
    feed(&mut t, "hello\r\nworld", 1);
    let r = |l: u64, c: u16| CellPoint {
        line: AbsLine(l),
        col: c,
    };
    let sel = [CellRange {
        start: r(0, 3),
        end: r(1, 1),
    }];
    let cur = CellRange {
        start: r(1, 3),
        end: r(1, 4),
    };
    let f = t.snapshot_with(
        None,
        &Overlays {
            selection: &sel,
            matches: &[],
            current_match: Some(cur),
        },
    );
    let v = f.view().unwrap();
    assert_eq!(v.row(0)[2].flags & CELL_SELECTED, 0);
    assert_ne!(v.row(0)[3].flags & CELL_SELECTED, 0);
    assert_ne!(v.row(0)[9].flags & CELL_SELECTED, 0);
    assert_ne!(v.row(1)[1].flags & CELL_SELECTED, 0);
    assert_eq!(v.row(1)[2].flags & CELL_SELECTED, 0);
    assert_eq!(
        v.row(1)[3].flags & (CELL_MATCH | CELL_MATCH_CURRENT),
        CELL_MATCH | CELL_MATCH_CURRENT
    );
}

#[test]
fn frame_buffer_can_be_reused() {
    let mut t = term(10, 3, 10);
    feed(&mut t, "abc", 1);
    let f1 = t.snapshot(None);
    feed(&mut t, "\rXY", 2);
    let f2 = t.snapshot(Some(f1));
    let v = f2.view().unwrap();
    assert_eq!(row_text(&v, 0), "XYc");
    assert_eq!(v.header().seq, 2);
}

// ---------------------------------------------------------------- 与裸 Term 对拍

type Bare = (
    Term<EventSink>,
    Processor<NoSync>,
    std::sync::mpsc::Receiver<Event>,
);

fn bare(cols: usize, rows: usize, scrollback: usize) -> Bare {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Term::new(
        alacritty_config(scrollback),
        &Dims { cols, rows },
        EventSink(tx),
    );
    (t, Processor::new(), rx)
}

fn dump(term: &Term<EventSink>) -> String {
    let g = term.grid();
    let mut out = String::new();
    let hist = g.history_size() as i32;
    for l in -hist..g.screen_lines() as i32 {
        for c in 0..g.columns() {
            let cell = &g[Line(l)][Column(c)];
            out.push_str(&format!("{cell:?}"));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "{:?} wrap={} saved={:?} mode={:?} style={:?} off={}",
        g.cursor.point,
        g.cursor.input_needs_wrap,
        g.saved_cursor.point,
        term.mode(),
        term.cursor_style(),
        g.display_offset()
    ));
    out
}

/// 覆盖 vte `Handler` 的每一个方法。升级 vte / alacritty_terminal 后如果 `Handler` 多了方法，
/// 要把对应序列加到这里，并检查 `handler.rs` 的转发清单。
const SEQUENCES: &[&str] = &[
    "hello \x1b[1;3;4;5;7;8;9;31;42mstyled\x1b[0m world",
    "\x1b[4:3mcurl\x1b[4:4mdot\x1b[4:5mdash\x1b[21mdbl\x1b[24m\x1b[58;5;3mulc\x1b[59m\x1b[38;2;9;8;7mrgb\x1b[39;49m",
    "\x1b[5;10Hx\x1b[2Ay\x1b[3By\x1b[4Cy\x1b[2Dy\x1b[2Ey\x1b[1Fy\x1b[7Gy\x1b[3dy\x1b[9`y",
    "\x1b7\x1b[1;1Hsaved\x1b8back\x1b[s\x1b[3;3H\x1b[u!",
    "a\tb\t\tc\x1b[2Id\x1b[Ze\x1bH\x1b[0g\x1b[3g\x1b[?5W",
    "abcdefghij\x1b[4D\x1b[3@\x1b[2P\x1b[2X\x1b[2L\x1b[2M\x1b[K\x1b[1K\x1b[2K",
    "\x1b[3;10r\x1b[2S\x1b[2T\x1bM\x1bD\x1bE\x1b[r",
    "fill\r\nfill\r\nfill\x1b[J\x1b[1J\x1b[0J",
    "\x1b[?25l\x1b[?1h\x1b[?7l\x1b[?12h\x1b[4h\x1b[20h\x1b=\x1b>abc\x1b[4l\x1b[20l\x1b[?7h\x1b[?25h",
    "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1005h\x1b[?1007h\x1b[?2004h\x1b[?1004h\x1b[?1000l\x1b[?6h\x1b[?6l",
    "\x1b(0qqq\x1b(B\x0eqq\x0f\x1b)0\x1b*B\x1b+B",
    "\x1b]2;title\x07\x1b[22;0t\x1b]2;other\x07\x1b[23;0t\x1b]0;both\x1b\\",
    "\x1b[14t\x1b[18t\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?25$p\x1b[4$p\x1b[?1049$p",
    "\x1b[1000;1000H\x1b[3 q\x1b[5 q\x1b[0 q\x1b[?12l",
    "\x1b]4;2;rgb:11/22/33\x07\x1b]10;#aabbcc\x07\x1b]11;rgb:00/00/00\x07\x1b]104;2\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07",
    "\x1b]52;c;aGVsbG8=\x07\x1b]52;p;d29ybGQ=\x07\x1b]8;id=a;http://x/\x07linked\x1b]8;;\x07",
    "ab\x1b[3b\x07\x08\r\n\x0b\x0c",
    "\x1b#8",
    "\x1b[>1u\x1b[?u\x1b[<u\x1b[=5;1u\x1b[>4;2m\x1b[?4m\x1b[>4m\x1b[>3u\x1b[>7u\x1b[<2u",
    "\x1b[?1049hinalt\x1b[2J\x1b[?1049l",
    "\x1b[?3h\x1b[?3l after-deccolm",
    "wide中文😀\u{0301}x\r\n日本語日本語日本語日本語日本語日本語日本語日本語日本語",
    "\x1b[2J",
    "\x1bc",
    "\x1b[?1049h\x1b[?1049hdouble\x1b[?1049l\x1b[?1049l",
    "\x1b[1;5r\x1b[5;1H\n\n\n\x1b[r",
];

#[test]
fn forwarding_matches_bare_term() {
    let (cols, rows, sb) = (20, 8, 25);
    let mut ours = term(cols as u16, rows as u16, sb);
    let (mut b, mut proc, rx) = bare(cols, rows, sb);
    let mut bare_replies = String::new();
    // 每条序列先单独喂，再整体重复两遍，让回滚、滚动区域与全屏切换有足够的组合。
    let mut script: Vec<&str> = SEQUENCES.to_vec();
    script.extend_from_slice(SEQUENCES);
    for (i, seq) in script.iter().enumerate() {
        // 为了与裸 Term 的“每次 advance 一整条”一致，ESC 之前加足换行制造滚动。
        let input = format!("{seq}\r\nline{i}\r\n");
        ours.feed(input.as_bytes(), i as i64);
        proc.advance(&mut b, input.as_bytes());
        while let Ok(ev) = rx.try_recv() {
            if let Event::PtyWrite(s) = ev {
                bare_replies.push_str(&s);
            }
        }
        assert_eq!(
            dump(&ours.core.term),
            dump(&b),
            "序列 #{i} {seq:?} 之后与裸 Term 不一致"
        );
        assert_consistent(&ours);
    }
    // 颜色/文本区域查询在裸 Term 这边是闭包事件，只比较 PtyWrite 类回应。
    let ours_replies = String::from_utf8(ours.take_replies()).unwrap();
    for part in bare_replies.split('\x1b').filter(|p| !p.is_empty()) {
        assert!(ours_replies.contains(part), "缺少回应 {part:?}");
    }
}

// ---------------------------------------------------------------- 侧表与网格的随机一致性

fn lfeed(t: &mut Terminal, log: &mut Vec<String>, s: &str, now: i64) {
    log.push(format!("{s:?}"));
    feed(t, s, now);
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn row_empty(t: &Terminal, line: i32) -> bool {
    let g = t.core.term.grid();
    (0..g.columns()).all(|c| matches!(g[Line(line)][Column(c)].c, ' ' | '\t'))
}

/// 每行写入形如 `#000123` 的标记，行元数据里的时间戳就是 123。随机执行各种移动行的操作后，
/// 带标记的行必须仍然带着自己的时间戳，“未写过”的行必须真的是空的。
fn check_markers(t: &Terminal) -> Result<(), String> {
    if t.core.alt {
        return Ok(());
    }
    let g = t.core.term.grid();
    let hist = g.history_size() as i32;
    let top = t.core.table.first_abs();
    for l in -hist..g.screen_lines() as i32 {
        let abs = top + (l + hist) as u64;
        let meta = t.core.table.get(abs).unwrap();
        let written = meta.flags & crate::table::ROW_WRITTEN != 0;
        if !written {
            if !row_empty(t, l) {
                return Err(format!("行 {l} 有内容但元数据说从未写过"));
            }
            continue;
        }
        // 上一行末尾残留着 WRAPLINE（覆盖写入留下的）时，alacritty 把这一行当作上一行的续行，
        // 重排后只有逻辑行级别的时间戳；这种行不检查。
        let last = Column(g.columns() - 1);
        if l > -hist
            && g[Line(l - 1)][last]
                .flags
                .contains(alacritty_terminal::term::cell::Flags::WRAPLINE)
        {
            continue;
        }
        // 逻辑行里出现多个标记（被残留的 WRAPLINE 合并过）时，时间戳只有整个逻辑行的粒度，不检查。
        let mut group = String::new();
        let mut gl = l;
        loop {
            group.extend((0..g.columns()).map(|c| g[Line(gl)][Column(c)].c));
            if gl + 1 >= g.screen_lines() as i32
                || !g[Line(gl)][last]
                    .flags
                    .contains(alacritty_terminal::term::cell::Flags::WRAPLINE)
            {
                break;
            }
            gl += 1;
        }
        if group.matches("#0000").count() > 1 {
            continue;
        }
        let text: String = (0..7).map(|c| g[Line(l)][Column(c)].c).collect();
        if let Some(num) = text.strip_prefix('#').and_then(|n| n.parse::<i64>().ok())
            && meta.ts_ms != num
        {
            return Err(format!("行 {l}（{text}）的时间戳错位：{}", meta.ts_ms));
        }
    }
    Ok(())
}

/// 随机操作的范围。侧表的两类逻辑分开验证：
/// - `Scroll`：各种移动行的操作（区域滚动、IL/DL/SU/SD/RI、ED、光标定位），不 resize；
/// - `Reflow`：只写行并 resize（含全屏里 resize），不做光标定位 / 区域：覆盖写入会留下残留的
///   WRAPLINE 与重叠内容，那种状态下“逻辑行”本身就是含糊的，重排后只能尽力而为。
#[derive(Clone, Copy, PartialEq)]
enum Ops {
    Scroll,
    Reflow,
}

fn run_random(seed: u64, ops: Ops, steps: usize) {
    let mut rng = Lcg(seed + 1);
    let cols = 10 + rng.next(30) as u16;
    let rows = 4 + rng.next(14) as u16;
    let mut t = term(cols, rows, 5 + rng.next(40) as usize);
    let mut rows = rows;
    let mut n = 0i64;
    let mut log: Vec<String> = Vec::new();
    for step in 0..steps {
        let pick = rng.next(16);
        match (pick, ops) {
            (0..=6, _) => {
                // 只在光标所在行为空时写标记：标记行是“新写出的行”，时间戳必须等于它的编号。
                let mut written = false;
                if !t.core.alt {
                    let line = t.core.term.grid().cursor.point.line.0;
                    // 被擦空但元数据仍标着“写过”的行（EL / ED 擦除光标所在行）也不能写标记：
                    // 首次写出时间按设计保持不变，标记编号就会对不上。
                    let fresh = t
                        .core
                        .table
                        .get(t.core.cursor_abs())
                        .is_some_and(|m| m.flags & crate::table::ROW_WRITTEN == 0);
                    // 光标处于“等待换行”状态（重排会留下这种状态）时，下一个字符会先换到下一行，
                    // 所以不能在这里写标记。
                    let pending_wrap = t.core.term.grid().cursor.input_needs_wrap;
                    if row_empty(&t, line) && fresh && !pending_wrap {
                        n += 1;
                        let filler: String = (0..rng.next(45))
                            .map(|_| ["x", "中", "é", "y"][rng.next(4) as usize])
                            .collect();
                        lfeed(&mut t, &mut log, &format!("#{n:06}{filler}\r\n"), n);
                        written = true;
                    }
                }
                if !written {
                    lfeed(&mut t, &mut log, "\r\n", n);
                }
            }
            (7, Ops::Scroll) => {
                let top = 1 + rng.next(u64::from(rows) - 1);
                let bot = top + 1 + rng.next(u64::from(rows) - top);
                lfeed(&mut t, &mut log, &format!("\x1b[{top};{bot}r"), n);
            }
            (8, Ops::Scroll) => lfeed(&mut t, &mut log, "\x1b[r", n),
            (9, Ops::Scroll) => {
                let k = 1 + rng.next(4);
                let op = ["L", "M", "S", "T"][rng.next(4) as usize];
                lfeed(&mut t, &mut log, &format!("\x1b[{k}{op}"), n);
            }
            (10, Ops::Scroll) => lfeed(&mut t, &mut log, "\x1bM", n),
            (11, Ops::Scroll) => {
                let r = 1 + rng.next(u64::from(rows));
                lfeed(&mut t, &mut log, &format!("\x1b[{r};1H"), n);
            }
            (12, Ops::Scroll) => {
                let seq = ["\x1b[J", "\x1b[1J", "\x1b[2J"][rng.next(3) as usize];
                lfeed(&mut t, &mut log, seq, n);
            }
            (13, Ops::Reflow) => {
                let cols = 8 + rng.next(40) as u16;
                rows = 3 + rng.next(16) as u16;
                log.push(format!("resize {cols}x{rows}"));
                t.resize(GridSize { cols, rows });
            }
            (14, _) => {
                let seq = if t.core.alt {
                    "\x1b[?1049l"
                } else {
                    "\x1b[?1049h"
                };
                lfeed(&mut t, &mut log, seq, n);
            }
            _ => {}
        }
        let recent = &log[log.len().saturating_sub(40)..];
        assert_eq!(
            t.core.resync_count, 0,
            "seed {seed} step {step}：侧表与网格行数错位\n{recent:?}"
        );
        if let Err(e) = check_markers(&t) {
            panic!("seed {seed} step {step}: {e}\n最近操作 {recent:?}");
        }
    }
}

#[test]
fn side_table_stays_aligned_under_random_scrolling() {
    for seed in 0..60 {
        run_random(seed, Ops::Scroll, 400);
    }
}

#[test]
fn side_table_stays_aligned_under_random_reflow() {
    for seed in 0..60 {
        run_random(seed, Ops::Reflow, 300);
    }
}

// ---------------------------------------------------------------- OSC 扫描器与批次边界

#[test]
fn feeding_byte_by_byte_equals_one_batch_for_prompts() {
    let data =
        "a\r\n\x1b]133;A\x07$ ls\r\n\x1b]133;C\x07\x1b]7;file:///tmp\x07out\r\n\x1b]133;D;0\x07";
    let mut one = term(20, 6, 10);
    one.feed(data.as_bytes(), 1);
    let mut many = term(20, 6, 10);
    for b in data.as_bytes() {
        many.feed(std::slice::from_ref(b), 1);
    }
    assert_eq!(one.take_events(), many.take_events());
    assert_eq!(one.cwd(), many.cwd());
}

// ---------------------------------------------------------------- 吞吐（手动运行）

fn seq_output(max_bytes: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(max_bytes + 16);
    let mut i = 1u64;
    while v.len() < max_bytes {
        v.extend_from_slice(i.to_string().as_bytes());
        v.extend_from_slice(b"\r\n");
        i += 1;
    }
    v
}

/// `cargo test -p kanso-term --release -- --ignored --nocapture throughput`
#[test]
#[ignore]
fn throughput_vs_bare_alacritty() {
    let data = seq_output(50 * 1024 * 1024);
    let mb = data.len() as f64 / (1024.0 * 1024.0);
    let chunk = 64 * 1024;
    let mut best_bare = 0f64;
    let mut best_ours = 0f64;
    // 交替运行取最好的一次，削弱调度与缓存预热带来的噪声。
    for _ in 0..3 {
        let (mut b, mut proc, rx) = bare(80, 24, 10_000);
        let start = std::time::Instant::now();
        for c in data.chunks(chunk) {
            proc.advance(&mut b, c);
            while rx.try_recv().is_ok() {}
        }
        best_bare = best_bare.max(mb / start.elapsed().as_secs_f64());

        let mut t = term(80, 24, 10_000);
        let start = std::time::Instant::now();
        for (i, c) in data.chunks(chunk).enumerate() {
            t.feed(c, i as i64);
        }
        best_ours = best_ours.max(mb / start.elapsed().as_secs_f64());
        assert_consistent(&t);
    }
    println!("裸 alacritty Term + Processor: {best_bare:.1} MB/s");
    println!("kanso Terminal::feed:          {best_ours:.1} MB/s");
    println!(
        "比值（ours/bare）:              {:.3}",
        best_ours / best_bare
    );
}

#[test]
fn ed2_keeps_side_table_in_sync_for_all_small_geometries() {
    for rows in 1..7u16 {
        for sb in [0usize, 1, 3, 20] {
            for n in 0..12 {
                let mut t = term(10, rows, sb);
                lines(&mut t, n, 1);
                feed(&mut t, "\x1b[H\x1b[2J", 2);
                assert_eq!(t.core.resync_count, 0, "rows {rows} sb {sb} n {n}");
            }
        }
    }
}

// ---- 文本读取（MCP，spec/mcp.md §5）----

#[test]
fn read_text_joins_soft_wraps_trims_trailing_spaces_and_counts_wide_chars_once() {
    let mut t = term(10, 5, 100);
    // 25 个字符在 10 列上软换行成 3 行，应拼回一整行；"中文" 各占两格，只应出现一次。
    feed(&mut t, "0123456789ABCDEFGHIJabcde\r\n中文   \r\nplain  ", 1);
    assert_eq!(
        t.read_text(ReadScope::Screen),
        "0123456789ABCDEFGHIJabcde\n中文\nplain"
    );
}

#[test]
fn read_text_keeps_spaces_at_a_soft_wrap_boundary() {
    let mut t = term(5, 4, 10);
    // 行尾的空格落在换行处：它是内容，不能当作"行尾多余的空格"去掉。
    feed(&mut t, "ab   cd", 1);
    assert_eq!(t.read_text(ReadScope::Screen), "ab   cd");
}

#[test]
fn read_text_recent_spans_scrollback_and_counts_logical_lines() {
    let mut t = term(20, 3, 100);
    for i in 0..10 {
        feed(&mut t, &format!("L{i}\r\n"), 1);
    }
    // 屏幕只有 3 行，更早的在回滚里。
    let screen = t.read_text(ReadScope::Screen);
    assert!(!screen.contains("L0"));
    assert_eq!(
        t.read_text(ReadScope::Recent(3)),
        "L7\nL8\nL9",
        "末尾的空行不占行数"
    );
    let all = t.read_text(ReadScope::Recent(1000));
    assert!(all.starts_with("L0\nL1\n"));
    assert_eq!(t.read_text(ReadScope::Recent(0)), "");
}

#[test]
fn read_text_screen_ignores_scrolled_view_and_reports_alt_screen() {
    let mut t = term(20, 3, 100);
    for i in 0..10 {
        feed(&mut t, &format!("L{i}\r\n"), 1);
    }
    t.scroll_display(5);
    // 用户正在往回翻：读的仍然是当前屏幕，不是翻到的位置。
    assert!(t.read_text(ReadScope::Screen).contains("L9"));
    feed(&mut t, "\x1b[?1049h\x1b[Hfull", 2);
    assert!(t.modes().alt_screen);
    assert_eq!(t.read_text(ReadScope::Recent(50)), "full");
    assert_eq!(t.screen_cursor(), (0, 4));
}

#[test]
fn clear_count_counts_paired_clears_and_buffer_clear_but_not_plain_ed2() {
    let mut t = term(20, 3, 100);
    feed(&mut t, "a\r\n", 1);
    // 只有 ED 2：全屏程序重绘，屏幕内容被推进回滚而不是抹掉，不算清屏。
    feed(&mut t, "\x1b[2J", 2);
    assert_eq!(t.clear_count(), 0);
    // 同一批里 ED 2 与 ED 3 成对才是 `clear`。
    feed(&mut t, "\x1b[H\x1b[2J\x1b[3J", 3);
    assert_eq!(t.clear_count(), 1);
    t.clear_buffer();
    assert_eq!(t.clear_count(), 2);
}

#[test]
fn text_between_reads_from_the_line_after_a_wrapped_logical_line() {
    let mut t = term(10, 6, 100);
    feed(&mut t, "$ 0123456789ABC\r\nout1\r\nout2\r\n$ ", 1);
    let start = CellPoint {
        line: AbsLine(0),
        col: 2,
    };
    let from = CellPoint {
        line: t.line_after_logical(start.line),
        col: 0,
    };
    // 命令回显软换行成两行：输出从第三行开始。
    assert_eq!(from.line, AbsLine(2));
    let end = CellPoint {
        line: t
            .logical_line_start(t.cursor_point().unwrap().line)
            .unwrap(),
        col: 0,
    };
    assert_eq!(t.text_between(from, end), "out1\nout2");
}
