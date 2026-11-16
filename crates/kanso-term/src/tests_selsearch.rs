//! 选区（terminal.md §8）与搜索（terminal.md §11）的测试。

use kanso_frame::{CELL_MATCH, CELL_MATCH_CURRENT, CELL_SELECTED, CELL_WIDE_SPACER, FrameView};
use kanso_types::geometry::{CellSide, GridSize, ViewportPoint};
use kanso_types::input::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::*;

fn term(cols: u16, rows: u16, scrollback: usize) -> Terminal {
    Terminal::new(TermConfig {
        size: GridSize { cols, rows },
        scrollback_lines: scrollback,
    })
}

fn feed(t: &mut Terminal, s: &str) {
    t.feed(s.as_bytes(), 1);
}

fn lines(t: &mut Terminal, n: usize) {
    for i in 1..=n {
        feed(t, &format!("L{i}\r\n"));
    }
}

fn vp(col: u16, row: u16, side: CellSide) -> ViewportPoint {
    ViewportPoint { col, row, side }
}

fn ev(kind: MouseEventKind, col: u16, row: u16, side: CellSide, clicks: u8) -> MouseEvent {
    MouseEvent {
        kind,
        button: MouseButton::Left,
        point: vp(col, row, side),
        mods: Modifiers::NONE,
        click_count: clicks,
    }
}

use CellSide::{Left as L, Right as R};
use MouseEventKind::{Down, Drag, Move, Up};

/// 单击拖动选 `(c0, r0)` 左侧到 `(c1, r1)` 右侧（含两端单元格）。
fn drag_select(
    t: &mut Terminal,
    from: (u16, u16),
    to: (u16, u16),
    mods: Modifiers,
) -> MouseOutcome {
    let mut d = ev(Down, from.0, from.1, L, 1);
    d.mods = mods;
    t.handle_mouse(&d);
    let mut m = ev(Drag, to.0, to.1, R, 1);
    m.mods = mods;
    t.handle_mouse(&m);
    let mut u = ev(Up, to.0, to.1, R, 1);
    u.mods = mods;
    t.handle_mouse(&u)
}

fn cell_flags(v: &FrameView<'_>, r: u16, c: u16) -> u16 {
    v.row(r)[usize::from(c)].flags
}

fn row_with(v: &FrameView<'_>, needle: &str) -> u16 {
    (0..v.header().rows)
        .find(|&r| {
            let s: String = v
                .row(r)
                .iter()
                .map(|c| {
                    if c.ch == 0 {
                        ' '
                    } else {
                        char::from_u32(c.ch).unwrap()
                    }
                })
                .collect();
            s.contains(needle)
        })
        .unwrap_or_else(|| panic!("找不到 {needle}"))
}

// ---------------------------------------------------------------- 选区

#[test]
fn drag_selects_text() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world");
    let out = drag_select(&mut t, (0, 0), (4, 0), Modifiers::NONE);
    assert_eq!(
        out,
        MouseOutcome::SelectionFinished {
            has_selection: true
        }
    );
    assert_eq!(t.selection_text().as_deref(), Some("hello"));
}

#[test]
fn side_decides_cell_boundaries() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world");
    // 从 'e'(1) 右半到 'l'(3) 左半：只选 'l'(2)。
    t.handle_mouse(&ev(Down, 1, 0, R, 1));
    t.handle_mouse(&ev(Up, 3, 0, L, 1));
    assert_eq!(t.selection_text().as_deref(), Some("l"));
}

#[test]
fn plain_click_makes_no_selection() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello");
    t.handle_mouse(&ev(Down, 2, 0, L, 1));
    let out = t.handle_mouse(&ev(Up, 2, 0, L, 1));
    assert_eq!(
        out,
        MouseOutcome::SelectionFinished {
            has_selection: false
        }
    );
    assert_eq!(t.selection_text(), None);
    assert!(!t.has_selection());
}

#[test]
fn double_click_selects_whole_path() {
    let mut t = term(40, 3, 10);
    feed(&mut t, "cd /usr/local/bin/foo-bar_1.2~x (ok)");
    t.handle_mouse(&ev(Down, 10, 0, L, 2));
    t.handle_mouse(&ev(Up, 10, 0, L, 2));
    assert_eq!(
        t.selection_text().as_deref(),
        Some("/usr/local/bin/foo-bar_1.2~x")
    );
}

#[test]
fn custom_word_separators() {
    let mut t = term(40, 3, 10);
    t.set_word_separators(" /");
    feed(&mut t, "cd /usr/local/bin");
    t.handle_mouse(&ev(Down, 8, 0, L, 2));
    t.handle_mouse(&ev(Up, 8, 0, L, 2));
    assert_eq!(t.selection_text().as_deref(), Some("local"));
    assert_eq!(t.word_separators(), " /");
}

#[test]
fn triple_click_selects_line_without_trailing_newline() {
    let mut t = term(20, 4, 10);
    feed(&mut t, "foo bar\r\nbaz");
    t.handle_mouse(&ev(Down, 2, 0, L, 3));
    t.handle_mouse(&ev(Up, 2, 0, L, 3));
    assert_eq!(t.selection_text().as_deref(), Some("foo bar"));
}

#[test]
fn option_drag_selects_rectangle() {
    let mut t = term(20, 4, 10);
    feed(&mut t, "abcdef\r\nghijkl\r\nmnopqr");
    for alt_left in [true, false] {
        let mods = Modifiers {
            alt_left,
            alt_right: !alt_left,
            ..Modifiers::NONE
        };
        drag_select(&mut t, (1, 0), (3, 2), mods);
        assert_eq!(t.selection_text().as_deref(), Some("bcd\nhij\nnop"));
    }
}

#[test]
fn soft_wrapped_lines_join_without_newline() {
    let mut t = term(5, 4, 10);
    feed(&mut t, "abcdefgh\r\nxy");
    t.select_all();
    assert_eq!(t.selection_text().as_deref(), Some("abcdefgh\nxy"));
}

#[test]
fn trailing_whitespace_trimmed_per_line() {
    let mut t = term(10, 4, 10);
    // 带背景色的行尾空格不是“空单元格”，alacritty 自己不会去掉。
    feed(&mut t, "\x1b[41mab   \x1b[0m\r\ncd  \r\nef");
    t.select_all();
    assert_eq!(t.selection_text().as_deref(), Some("ab\ncd\nef"));
}

#[test]
fn wide_and_combining_chars_copy_correctly() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "a世界b e\u{301}x");
    t.select_all();
    assert_eq!(t.selection_text().as_deref(), Some("a世界b e\u{301}x"));
    // 只选到宽字符的后半格：仍然得到这个字符。
    t.handle_mouse(&ev(Down, 2, 0, L, 1));
    t.handle_mouse(&ev(Up, 2, 0, R, 1));
    assert_eq!(t.selection_text().as_deref(), Some("世"));
}

#[test]
fn select_all_includes_scrollback_but_not_blank_tail() {
    let mut t = term(10, 5, 100);
    lines(&mut t, 12);
    t.select_all();
    let text = t.selection_text().unwrap();
    assert!(text.starts_with("L1\nL2\n"));
    assert!(text.ends_with("L12"), "{text:?}");
}

#[test]
fn selection_in_scrollback_follows_content() {
    let mut t = term(10, 5, 1000);
    lines(&mut t, 30);
    t.scroll_display(10);
    // 视口第 0 行现在是回滚里的某一行。
    t.handle_mouse(&ev(Down, 0, 1, L, 3));
    t.handle_mouse(&ev(Up, 0, 1, L, 3));
    let before = t.selection_text().unwrap();
    assert!(before.starts_with('L'));
    lines(&mut t, 40);
    assert_eq!(t.selection_text().unwrap(), before);
    // 帧里被标记的那一行仍是同一行内容（视口随新输出保持不动或被推走都可以，
    // 这里只检查内容：滚回选区所在位置能看到它）。
    t.scroll_to_bottom();
    t.scroll_display(50);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    let r = row_with(&v, &before);
    assert!(cell_flags(&v, r, 0) & CELL_SELECTED != 0);
}

#[test]
fn selection_survives_ed2_push_but_not_clear() {
    // 只有 ED 2：屏幕被推进回滚，选区跟着内容走。
    let mut t = term(10, 5, 100);
    lines(&mut t, 3);
    t.select_all();
    let before = t.selection_text().unwrap();
    assert_eq!(before, "L1\nL2\nL3");
    feed(&mut t, "\x1b[H\x1b[2J");
    assert_eq!(t.selection_text().as_deref(), Some("L1\nL2\nL3"));
    // 回滚里的文字被帧标了 SELECTED：滚上去看。
    t.scroll_display(3);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    let r = row_with(&v, "L2");
    assert!(cell_flags(&v, r, 0) & CELL_SELECTED != 0);

    // macOS 的 clear：ED 3 + ED 2 → 选区清除。
    let mut t = term(10, 5, 100);
    lines(&mut t, 3);
    t.select_all();
    feed(&mut t, "\x1b[H\x1b[3J\x1b[2J");
    assert!(!t.has_selection());
    // Linux 的 clear：ED 2 + ED 3 → 选区清除。
    let mut t = term(10, 5, 100);
    lines(&mut t, 3);
    t.select_all();
    feed(&mut t, "\x1b[H\x1b[2J\x1b[3J");
    assert!(!t.has_selection());
}

#[test]
fn selection_in_scrollback_survives_ed3_clipped() {
    // 单独 ED 3 只清回滚；全落在回滚里的选区被清掉。
    let mut t = term(10, 3, 100);
    lines(&mut t, 10);
    t.scroll_display(3);
    t.handle_mouse(&ev(Down, 0, 0, L, 3));
    t.handle_mouse(&ev(Up, 0, 0, L, 3));
    assert!(t.has_selection());
    feed(&mut t, "\x1b[3J");
    assert!(!t.has_selection());
}

#[test]
fn clear_buffer_and_alt_screen_clear_selection() {
    let mut t = term(10, 4, 100);
    lines(&mut t, 10);
    t.select_all();
    t.clear_buffer();
    assert!(!t.has_selection());
    assert_eq!(t.selection_text(), None);

    let mut t = term(10, 4, 100);
    feed(&mut t, "abc");
    t.select_all();
    assert!(t.has_selection());
    feed(&mut t, "\x1b[?1049h");
    assert!(!t.has_selection());

    let mut t = term(10, 4, 100);
    feed(&mut t, "abc");
    t.select_all();
    t.clear_selection();
    assert!(!t.has_selection());
}

#[test]
fn selection_marks_dirty() {
    let mut t = term(10, 4, 10);
    feed(&mut t, "abc");
    t.snapshot(None);
    assert!(!t.is_dirty());
    t.select_all();
    assert!(t.is_dirty());
    t.snapshot(None);
    t.clear_selection();
    assert!(t.is_dirty());
}

#[test]
fn mouse_reporting_vs_shift_bypass() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    let out = t.handle_mouse(&ev(Down, 1, 0, L, 1));
    assert_eq!(out, MouseOutcome::Report(b"\x1b[<0;2;1M".to_vec()));
    assert!(matches!(
        t.handle_mouse(&ev(Drag, 3, 0, R, 1)),
        MouseOutcome::Report(_)
    ));
    assert!(!t.has_selection());

    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let out = drag_select(&mut t, (0, 0), (4, 0), shift);
    assert_eq!(
        out,
        MouseOutcome::SelectionFinished {
            has_selection: true
        }
    );
    assert_eq!(t.selection_text().as_deref(), Some("hello"));
}

#[test]
fn shift_released_mid_drag_does_not_report_to_program() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world\x1b[?1002h\x1b[?1006h");
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let mut d = ev(Down, 0, 0, L, 1);
    d.mods = shift;
    assert_eq!(t.handle_mouse(&d), MouseOutcome::None);
    // 中途松开 Shift：程序没见过这次 Down，不能把 Drag / Up 报给它。
    assert_eq!(t.handle_mouse(&ev(Drag, 4, 0, R, 1)), MouseOutcome::None);
    assert_eq!(
        t.handle_mouse(&ev(Up, 4, 0, R, 1)),
        MouseOutcome::SelectionFinished {
            has_selection: true
        }
    );
}

#[test]
fn lost_up_does_not_stick() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world");
    // Down + Drag，没有 Up；再来一个 Down：旧拖动被结束，新选择从新位置开始。
    t.handle_mouse(&ev(Down, 0, 0, L, 1));
    t.handle_mouse(&ev(Drag, 4, 0, R, 1));
    t.handle_mouse(&ev(Down, 6, 0, L, 1));
    assert!(!t.has_selection());
    let out = t.handle_mouse(&ev(Up, 6, 0, L, 1));
    assert_eq!(
        out,
        MouseOutcome::SelectionFinished {
            has_selection: false
        }
    );
    // Up 之后的 Drag 不再更新选区。
    t.handle_mouse(&ev(Drag, 10, 0, R, 1));
    assert!(!t.has_selection());

    // 没有按键的 Move 说明按键已经松开：结束拖动，之后的 Drag 不生效。
    t.handle_mouse(&ev(Down, 0, 0, L, 1));
    t.handle_mouse(&ev(Drag, 4, 0, R, 1));
    let mut mv = ev(Move, 6, 0, L, 1);
    mv.button = MouseButton::None;
    assert_eq!(
        t.handle_mouse(&mv),
        MouseOutcome::SelectionFinished {
            has_selection: true
        }
    );
    t.handle_mouse(&ev(Drag, 10, 0, R, 1));
    assert_eq!(t.selection_text().as_deref(), Some("hello"));
}

#[test]
fn middle_and_right_buttons_keep_selection() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world");
    drag_select(&mut t, (0, 0), (4, 0), Modifiers::NONE);
    for b in [MouseButton::Middle, MouseButton::Right] {
        for k in [Down, Up] {
            let mut e = ev(k, 8, 0, L, 1);
            e.button = b;
            t.handle_mouse(&e);
        }
    }
    assert_eq!(t.selection_text().as_deref(), Some("hello"));
}

#[test]
fn select_word_at_for_context_menu() {
    let mut t = term(30, 3, 10);
    feed(&mut t, "echo hello_world  x");
    t.select_word_at(vp(8, 0, L));
    assert_eq!(t.selection_text().as_deref(), Some("hello_world"));
    // 已在选区内右键：不改变选区。
    t.select_word_at(vp(6, 0, R));
    assert_eq!(t.selection_text().as_deref(), Some("hello_world"));
    // 空白处右键：不选。
    t.clear_selection();
    t.select_word_at(vp(17, 0, L));
    assert!(!t.has_selection());
}

#[test]
fn selection_in_scrolled_view_uses_display_offset() {
    let mut t = term(10, 3, 100);
    lines(&mut t, 20);
    t.scroll_display(5);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    let top = (0..3)
        .map(|r| {
            let s: String = v
                .row(r)
                .iter()
                .filter(|c| c.ch != 0)
                .map(|c| char::from_u32(c.ch).unwrap())
                .collect();
            s
        })
        .collect::<Vec<_>>();
    t.handle_mouse(&ev(Down, 0, 0, L, 3));
    t.handle_mouse(&ev(Up, 0, 0, L, 3));
    assert_eq!(t.selection_text().unwrap(), top[0]);
}

#[test]
fn frame_marks_selected_cells() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "hello world\r\nsecond");
    drag_select(&mut t, (6, 0), (2, 1), Modifiers::NONE);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    for c in 0..20 {
        let expect0 = c >= 6;
        assert_eq!(
            cell_flags(&v, 0, c) & CELL_SELECTED != 0,
            expect0,
            "row0 col{c}"
        );
        let expect1 = c <= 2;
        assert_eq!(
            cell_flags(&v, 1, c) & CELL_SELECTED != 0,
            expect1,
            "row1 col{c}"
        );
    }
    // 宽字符整体（含后半格）被标记。
    let mut t = term(10, 2, 10);
    feed(&mut t, "a世b");
    t.handle_mouse(&ev(Down, 1, 0, L, 1));
    t.handle_mouse(&ev(Up, 1, 0, R, 1));
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    assert!(cell_flags(&v, 0, 1) & CELL_SELECTED != 0);
    assert!(cell_flags(&v, 0, 2) & CELL_SELECTED != 0);
    assert!(cell_flags(&v, 0, 2) & CELL_WIDE_SPACER != 0);
    assert!(cell_flags(&v, 0, 3) & CELL_SELECTED == 0);
}

#[test]
fn frame_marks_block_selection() {
    let mut t = term(10, 4, 10);
    feed(&mut t, "abcdef\r\nghijkl\r\nmnopqr");
    let mods = Modifiers {
        alt_left: true,
        ..Modifiers::NONE
    };
    drag_select(&mut t, (1, 0), (3, 2), mods);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    for r in 0..3 {
        for c in 0..6 {
            assert_eq!(
                cell_flags(&v, r, c) & CELL_SELECTED != 0,
                (1..=3).contains(&c),
                "r{r} c{c}"
            );
        }
    }
}

// ---------------------------------------------------------------- 搜索

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        case_sensitive: false,
        regex: false,
    }
}

#[test]
fn search_literal_case_and_regex() {
    let mut t = term(40, 5, 10);
    feed(&mut t, "Foo foo FOO f.o\r\nfoooo");
    assert_eq!(t.search(&q("foo")).total, 4);
    let mut cs = q("foo");
    cs.case_sensitive = true;
    assert_eq!(t.search(&cs).total, 2);
    // 非 regex 时 `.` 是字面量。
    assert_eq!(t.search(&q("f.o")).total, 1);
    let mut re = q("fo+");
    re.regex = true;
    assert_eq!(t.search(&re).total, 4);
    re.text = "f.o".into();
    assert_eq!(t.search(&re).total, 5);
    re.text = "(".into();
    let s = t.search(&re);
    assert!(s.invalid_regex);
    assert_eq!(s.total, 0);
    assert_eq!(t.search(&q("")).total, 0);
}

#[test]
fn search_matches_across_soft_wrap() {
    let mut t = term(5, 4, 10);
    feed(&mut t, "abcdefgh");
    let s = t.search(&q("cdef"));
    assert_eq!(s.total, 1);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    for (r, c, on) in [
        (0, 1, false),
        (0, 2, true),
        (0, 4, true),
        (1, 0, true),
        (1, 1, false),
    ] {
        assert_eq!(cell_flags(&v, r, c) & CELL_MATCH != 0, on, "r{r} c{c}");
    }
    // 折行处的空格是内容：能搜到跨过空格的匹配。
    let mut t = term(5, 4, 10);
    feed(&mut t, "abcd efgh");
    assert_eq!(t.search(&q("d e")).total, 1);
}

#[test]
fn search_incremental_keeps_current_match() {
    let mut t = term(40, 5, 10);
    feed(&mut t, "ab ac ab ad ab");
    let s = t.search(&q("a"));
    assert_eq!(s.total, 5);
    let first = s.current.unwrap();
    // 手动走到第 3 个（"ab" 的第二处）。
    let mut cur = first;
    while cur != 3 {
        cur = t.search_next(false).current.unwrap();
    }
    // 输入 "ab"：第 3 个 "a" 起点仍匹配，当前项保持在同一位置（现在是 "ab" 的第 2 个）。
    let s = t.search(&q("ab"));
    assert_eq!(s.total, 3);
    assert_eq!(s.current, Some(2));
    // 改成 "ad"：原当前项不再匹配，移到它之后最近的一个（唯一一个）。
    let s = t.search(&q("ad"));
    assert_eq!((s.total, s.current), (1, Some(1)));
}

#[test]
fn search_next_prev_cycle() {
    let mut t = term(40, 5, 10);
    feed(&mut t, "x x x");
    let s = t.search(&q("x"));
    assert_eq!(s.total, 3);
    let start = s.current.unwrap();
    let mut seen = vec![start];
    for _ in 0..3 {
        seen.push(t.search_next(false).current.unwrap());
    }
    assert_eq!(seen[0], seen[3]);
    assert_eq!(seen[1], seen[0] % 3 + 1);
    let back = t.search_next(true).current.unwrap();
    assert_eq!(back, seen[2]);
    // 从第 1 个往回绕到第 3 个。
    while t.search_next(true).current != Some(1) {}
    assert_eq!(t.search_next(true).current, Some(3));
}

#[test]
fn search_next_scrolls_viewport_to_match() {
    let mut t = term(10, 5, 1000);
    lines(&mut t, 100);
    let mut r = q("^L7$");
    r.regex = true;
    let s = t.search(&r);
    assert_eq!(s.total, 1);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    assert!(v.header().display_offset > 0);
    let row = row_with(&v, "L7");
    assert!(cell_flags(&v, row, 0) & CELL_MATCH_CURRENT != 0);
    // 滚回底部后 next 再把它找回来。
    t.scroll_to_bottom();
    t.search_next(false);
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    assert!(v.header().display_offset > 0);
}

#[test]
fn search_updates_with_output_and_close_clears() {
    let mut t = term(20, 4, 100);
    lines(&mut t, 10);
    assert_eq!(t.search(&q("L")).total, 10);
    feed(&mut t, "L11\r\nxx\r\nL12\r\n");
    assert_eq!(t.search_status().total, 12);
    // 屏幕上正在改写的行：覆盖掉一个匹配。
    feed(&mut t, "\x1b[2;1HZZZ");
    let total = t.search_status().total;
    // 与全量重搜一致。
    assert_eq!(t.search(&q("L")).total, total);

    t.scroll_to_bottom();
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    let any = (0..4).any(|r| (0..20).any(|c| cell_flags(&v, r, c) & CELL_MATCH != 0));
    assert!(any);

    t.search_close();
    assert_eq!(t.search_status(), SearchStatus::default());
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    for r in 0..4 {
        for c in 0..20 {
            assert_eq!(cell_flags(&v, r, c) & (CELL_MATCH | CELL_MATCH_CURRENT), 0);
        }
    }
    // 关闭后输出不再触发搜索。
    feed(&mut t, "L99\r\n");
    assert_eq!(t.search_status().total, 0);
}

#[test]
fn incremental_refresh_equals_full_search() {
    // 回滚很小：最老的匹配不断被挤掉；穿插 ED 2、软换行、滚动区域。
    let mut t = term(8, 4, 12);
    let mut qy = q("ab");
    t.search(&qy);
    let chunks = [
        "ab ab\r\n",
        "xabab\r\n",
        "abcdefghabab\r\n",
        "\x1b[2J\x1b[Hab\r\n",
        "ab\r\nab\r\nab\r\n",
        "\x1b[1;3r\x1b[3;1H\nab\n\nab\x1b[r",
        "zz\r\nab",
        "\x1b[3Jab\r\n",
        "ab\r\n",
        "ab\r\n",
        "ab\r\n",
    ];
    for c in chunks {
        feed(&mut t, c);
        let inc = t.search_status().total;
        let full = t.search(&qy).total;
        assert_eq!(inc, full, "after {c:?}");
    }
    qy.text = "b".into();
    t.search(&qy);
    t.resize(GridSize { cols: 5, rows: 3 });
    let inc = t.search_status().total;
    assert_eq!(inc, t.search(&qy).total);
}

#[test]
fn search_match_cap_and_truncated_flag() {
    let mut t = term(100, 24, 20_000);
    let row = "a".repeat(100);
    for _ in 0..1_100 {
        feed(&mut t, &format!("{row}\r\n"));
    }
    let mut r = q(".");
    r.regex = true;
    let s = t.search(&r);
    assert_eq!(s.total as usize, MAX_MATCHES);
    assert!(s.truncated);
    let scanned = t.core.search_rows.get();
    // 新输出到来时仍然有上限。
    feed(&mut t, &format!("{row}\r\n"));
    assert_eq!(t.search_status().total as usize, MAX_MATCHES);
    assert!(t.core.search_rows.get() - scanned < 10);
}

#[test]
fn reading_search_status_neither_scans_nor_marks_dirty() {
    let mut terminal = term(20, 3, 100);
    lines(&mut terminal, 80);
    terminal.search(&q("L"));
    terminal.snapshot(None);
    let scanned = terminal.core.search_rows.get();
    for _ in 0..10 {
        assert!(terminal.search_status().total > 0);
        assert!(!terminal.is_dirty());
    }
    assert_eq!(terminal.core.search_rows.get(), scanned);
}

#[test]
fn truncated_search_refills_after_history_eviction_without_full_scan() {
    let mut terminal = term(100, 4, 1005);
    let row = format!("{}\r\n", "a".repeat(100));
    for _ in 0..1100 {
        feed(&mut terminal, &row);
    }
    terminal.search(&q("a"));
    for _ in 0..12 {
        let scanned = terminal.core.search_rows.get();
        feed(&mut terminal, &row);
        let incremental = terminal.search_status();
        assert!(terminal.core.search_rows.get() - scanned < 12);
        assert_eq!(incremental, terminal.search(&q("a")));
    }
}

#[test]
fn frame_marks_current_match_and_other_matches() {
    let mut t = term(20, 3, 10);
    feed(&mut t, "ab cd ab");
    t.search(&q("ab"));
    let cur = t.search_status().current.unwrap();
    let f = t.snapshot(None);
    let v = f.view().unwrap();
    let first_is_current = cur == 1;
    for (c, matched, current) in [
        (0u16, true, first_is_current),
        (1, true, first_is_current),
        (2, false, false),
        (6, true, !first_is_current),
        (7, true, !first_is_current),
        (8, false, false),
    ] {
        let fl = cell_flags(&v, 0, c);
        assert_eq!(fl & CELL_MATCH != 0, matched, "col{c}");
        assert_eq!(fl & CELL_MATCH_CURRENT != 0, current, "col{c}");
    }
}

/// 性能：10 万行回滚里搜索常见词应 < 50 ms（design.md §6）。
/// `cargo test -p kanso-term --release -- --ignored --nocapture`
#[test]
#[ignore = "性能测试，需要 --release 并手动运行"]
fn search_perf_100k_lines() {
    use std::time::Instant;
    let mut t = term(120, 40, 100_000);
    let start = Instant::now();
    let mut buf = String::new();
    for i in 0..100_040u32 {
        let level = if i % 25 == 0 {
            "ERROR timeout"
        } else {
            "INFO ok"
        };
        buf.push_str(&format!(
            "2026-11-13 12:00:{:02}.{:03} {level} request handled user=alice path=/api/v1/items/{i} status=200\r\n",
            i % 60,
            i % 1000
        ));
        if buf.len() > 64 * 1024 {
            t.feed(buf.as_bytes(), i64::from(i));
            buf.clear();
        }
    }
    t.feed(buf.as_bytes(), 0);
    println!("填充 10 万行: {:?}", start.elapsed());

    for (name, query) in [
        ("字面量 timeout（4% 的行）", q("timeout")),
        ("字面量 alice（每行都有，触发 10 万上限）", q("alice")),
        ("字面量 nomatch（无匹配）", q("zzzzqq")),
        (
            "正则 ERROR\\s+time\\w+",
            SearchQuery {
                text: r"ERROR\s+time\w+".into(),
                case_sensitive: true,
                regex: true,
            },
        ),
    ] {
        let mut best = std::time::Duration::MAX;
        let mut st = SearchStatus::default();
        for _ in 0..5 {
            t.search_close();
            let s = Instant::now();
            st = t.search(&query);
            best = best.min(s.elapsed());
        }
        println!(
            "{name}: {best:?}（total {} truncated {}）",
            st.total, st.truncated
        );
    }

    // 增量更新：搜索打开时每批输出的开销。
    t.search(&q("timeout"));
    let s = Instant::now();
    for i in 0..1000 {
        t.feed(format!("new line {i} timeout\r\n").as_bytes(), 1);
    }
    println!("搜索打开时 1000 次 feed（各 1 行）: {:?}", s.elapsed());
}
