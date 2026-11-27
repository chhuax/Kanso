//! 鼠标选区与复制文本（terminal.md §8.1、§8.2）。
//!
//! # 选区放在哪里
//!
//! 选区直接用 alacritty 的 `Term::selection`（内部状态），而不是自己维护一份绝对行号区间再
//! 经 `Overlays` 传入：
//! - alacritty 在每一次行移动（滚动、插删行、重排）里都会对选区做 `rotate`，选区天然跟着
//!   所选内容走；`Term` 内部的清除路径（ED 0/1/3、行清除、切换全屏、RIS、改列宽）也会正确地
//!   清掉或截断它。自己维护就得在 `Core` 里把这些全部镜像一遍，漏一处就错位。
//! - 复制文本用 `Term::selection_to_string`，软换行连接、宽字符、组合字符、矩形截取都已处理。
//!
//! 帧快照时 [`Core::selection_cell_ranges`] 再把它换成绝对行号区间，经 `Overlays` 标到单元格上。
//!
//! # 时间戳栏
//!
//! 时间戳与行号栏是帧里的独立一列（`LineMeta`），不在网格里，所以选区、复制文本
//! 天然不会包含它（terminal.md §2）。不要把栏的文字拼进网格来渲染，否则这个保证就没了。
//!
//! # 一次拖动的生命周期
//!
//! `Terminal::drag` 为真表示“终端正在处理一次左键拖动”。它在左键 `Down` 时置位，在 `Up`
//! 时清除；任何新的 `Down`、程序接管鼠标、`Move`（说明按键其实已经松开）、清空缓冲区都会
//! 先结束上一次拖动，所以丢失 `Up` 事件不会让选区粘在鼠标上。

use alacritty_terminal::grid::{Dimensions, GridCell};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use kanso_types::geometry::{AbsLine, CellPoint, CellSide, ViewportPoint};
use kanso_types::input::{MouseButton, MouseEvent, MouseEventKind};

use crate::Terminal;
use crate::handler::Core;
use crate::input::encode_mouse;
use crate::types::{CellRange, MouseOutcome};

impl Core {
    /// 视口坐标 → 网格坐标（回滚里的行是负数）。超出范围的行列夹到网格内。
    ///
    /// 要减掉 `display_offset`：用户向上滚动后，视口第 0 行是回滚里的某一行。
    fn viewport_to_grid(&self, p: ViewportPoint) -> (Point, Side) {
        let row = usize::from(p.row).min(self.rows - 1);
        let line = if self.has_folded() {
            let (viewport, _, _) = self.viewport_map();
            match viewport[row] {
                Some(abs) => Line((abs as i64 - self.table.screen_top_abs() as i64) as i32),
                None => Line(self.rows as i32 - 1),
            }
        } else {
            let offset = self.term.grid().display_offset() as i32;
            Line(row as i32 - offset)
        };
        let col = usize::from(p.col).min(self.cols - 1);
        let side = match p.side {
            CellSide::Left => Side::Left,
            CellSide::Right => Side::Right,
        };
        (Point::new(line, Column(col)), side)
    }

    /// 网格行 → 绝对行号。
    fn grid_line_abs(&self, line: Line) -> u64 {
        (self.table.screen_top_abs() as i64 + i64::from(line.0)).max(0) as u64
    }

    /// 当前选区的有效范围（空选区、已被挤出回滚的选区返回 `None`）。
    fn selection_range(&self) -> Option<alacritty_terminal::selection::SelectionRange> {
        self.term.selection.as_ref()?.to_range(&self.term)
    }

    /// 选区换成绝对行号区间，供帧快照标 `SELECTED`。
    ///
    /// 普通选区是一个首尾区间（中间行整行）；矩形选区每行的列范围相同，只生成当前视口内的行，
    /// 避免选了很长的回滚时产生海量区间。宽字符的后半格一并算选中（与复制文本一致）。
    pub(crate) fn selection_cell_ranges(&self) -> Vec<CellRange> {
        let Some(r) = self.selection_range() else {
            return Vec::new();
        };
        let grid = self.term.grid();
        let last = self.cols as u16 - 1;
        // 选区端点落在宽字符的后半格时向前补到它的前半格，落在前半格时向后补到后半格。
        let start_col = |line: Line, col: Column| -> u16 {
            let c = col.0 as u16;
            if c > 0 && grid[line][col].flags.contains(Flags::WIDE_CHAR_SPACER) {
                c - 1
            } else {
                c
            }
        };
        let end_col = |line: Line, col: Column| -> u16 {
            let c = col.0 as u16;
            if c < last && grid[line][col].flags.contains(Flags::WIDE_CHAR) {
                c + 1
            } else {
                c
            }
        };
        let cp = |line: Line, col: u16| CellPoint {
            line: AbsLine(self.grid_line_abs(line)),
            col,
        };
        if !r.is_block {
            return vec![CellRange {
                start: cp(r.start.line, start_col(r.start.line, r.start.column)),
                end: cp(r.end.line, end_col(r.end.line, r.end.column)),
            }];
        }
        let offset = grid.display_offset() as i32;
        let from = r.start.line.0.max(-offset);
        let to = r.end.line.0.min(self.rows as i32 - 1 - offset);
        (from..=to)
            .map(|l| {
                let line = Line(l);
                CellRange {
                    start: cp(line, start_col(line, r.start.column)),
                    end: cp(line, end_col(line, r.end.column)),
                }
            })
            .collect()
    }
}

/// 复制文本的后处理：行尾空白去掉（最后一段除外），整行选择不带末尾换行。
///
/// alacritty 只去掉“默认样式的空白单元格”，带背景色的行尾空格会留下，所以这里再按硬换行
/// 统一去一遍；软换行处的空格是内容（单词在行宽处折行时空格落在行尾），不在 `\n` 之前，不受影响。
/// 最后一段不去：它的终点是用户选的位置，用户选中 `hello ` 就应该得到带空格的文本。
/// 整行选择（三击）alacritty 会在末尾补 `\n`；粘贴到 shell 会被当成回车直接执行，所以去掉。
pub(crate) fn normalize_text(s: &str, lines: bool) -> String {
    let s = if lines {
        s.strip_suffix('\n').unwrap_or(s)
    } else {
        s
    };
    let mut out = String::with_capacity(s.len());
    let mut parts = s.split('\n').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            out.push_str(part.trim_end());
            out.push('\n');
        } else {
            out.push_str(part);
        }
    }
    out
}

impl Terminal {
    /// 处理一次鼠标事件：要么编码后交给程序，要么由终端自己做选择。
    ///
    /// 去向（terminal.md §8.1）：
    /// 1. 程序开启了鼠标上报且没按 Shift → [`MouseOutcome::Report`]，调用方把字节写回会话；
    /// 2. 否则左键按下按 `click_count` 开始选择：1 普通（按住 Option 为矩形）、2 选词、3 选整行；
    ///    拖动更新终点；松开结束并返回 [`MouseOutcome::SelectionFinished`]；
    /// 3. 中键、右键不改变选区（右键菜单前的选词用 [`Terminal::select_word_at`]）。
    ///
    /// 一次拖动一旦由终端开始，就由终端处理到结束，即使中途 Shift 松开了也不会把后续事件
    /// 报给程序（程序没见过那次 `Down`）。调用方应在拖动选择期间继续把事件都送进来。
    pub fn handle_mouse(&mut self, event: &MouseEvent) -> MouseOutcome {
        if !self.drag && !event.mods.shift {
            let modes = self.core.modes();
            if modes.mouse_reporting() {
                return encode_mouse(event, &modes)
                    .map_or(MouseOutcome::None, MouseOutcome::Report);
            }
        }
        match (event.kind, event.button) {
            (MouseEventKind::Down, MouseButton::Left) => {
                // 上一次拖动若丢了 Up：新的 Down 直接覆盖它（选区本身被下面的新选区替换）。
                self.drag = true;
                let (point, side) = self.core.viewport_to_grid(event.point);
                let (ty, side) = match event.click_count {
                    0 | 1 if event.mods.alt() => (SelectionType::Block, side),
                    0 | 1 => (SelectionType::Simple, side),
                    // 选词、选行与点在单元格哪一侧无关；固定用左侧，与 `select_word_at` 一致。
                    2 => (SelectionType::Semantic, Side::Left),
                    _ => (SelectionType::Lines, Side::Left),
                };
                self.core.term.selection = Some(Selection::new(ty, point, side));
                self.dirty = true;
                MouseOutcome::None
            }
            (MouseEventKind::Down, _) => {
                self.drag = false;
                MouseOutcome::None
            }
            (MouseEventKind::Drag, MouseButton::Left) if self.drag => {
                self.update_selection_end(event.point);
                MouseOutcome::None
            }
            (MouseEventKind::Up, MouseButton::Left) if self.drag => {
                self.update_selection_end(event.point);
                self.finish_drag()
            }
            // 没有按键的移动：按键其实已经松开，说明 Up 丢了。
            (MouseEventKind::Move, _) if self.drag => self.finish_drag(),
            _ => MouseOutcome::None,
        }
    }

    fn update_selection_end(&mut self, p: ViewportPoint) {
        let (point, side) = self.core.viewport_to_grid(p);
        if let Some(sel) = self.core.term.selection.as_mut() {
            sel.update(point, side);
            self.dirty = true;
        }
    }

    fn finish_drag(&mut self) -> MouseOutcome {
        self.drag = false;
        self.dirty = true;
        MouseOutcome::SelectionFinished {
            has_selection: self.has_selection(),
        }
    }

    /// 选中视口里 `point` 处的单词（右键弹菜单前调用，terminal.md §8.1）。
    ///
    /// 指针在空白上、或已经落在现有选区内时不改变选区：前者没有单词可选，后者说明用户在选区上
    /// 右键，要对这块选区做“复制”。单词边界见 [`Terminal::set_word_separators`]。
    pub fn select_word_at(&mut self, point: ViewportPoint) {
        let (pt, _) = self.core.viewport_to_grid(point);
        let cell = &self.core.term.grid()[pt.line][pt.column];
        if matches!(cell.c, ' ' | '\t' | '\0') && !cell.flags.contains(Flags::WIDE_CHAR) {
            return;
        }
        if self.core.selection_range().is_some_and(|r| r.contains(pt)) {
            return;
        }
        self.drag = false;
        self.core.term.selection = Some(Selection::new(SelectionType::Semantic, pt, Side::Left));
        self.dirty = true;
    }

    /// 全选：整个回滚加屏幕，到最后一个有内容的行为止（不把屏幕下方的空行都复制出来）。
    pub fn select_all(&mut self) {
        let term = &self.core.term;
        let top = term.topmost_line();
        let mut end = Line(0);
        'scan: for l in (top.0..self.core.rows as i32).rev() {
            let row = &term.grid()[Line(l)];
            for c in 0..self.core.cols {
                if !row[Column(c)].is_empty() {
                    end = Line(l);
                    break 'scan;
                }
            }
        }
        let mut sel = Selection::new(
            SelectionType::Simple,
            Point::new(top, Column(0)),
            Side::Left,
        );
        sel.update(Point::new(end, term.last_column()), Side::Right);
        self.core.term.selection = Some(sel);
        self.drag = false;
        self.dirty = true;
    }

    /// 清除选区。
    pub fn clear_selection(&mut self) {
        self.drag = false;
        if self.core.term.selection.take().is_some() {
            self.dirty = true;
        }
    }

    /// 是否有非空选区（单击没有拖动产生的空选区不算）。
    pub fn has_selection(&self) -> bool {
        self.core.selection_range().is_some()
    }

    /// 选区的文本。没有非空选区时为 `None`。
    ///
    /// 软换行的行直接连接，硬换行之间用 `\n`；行尾空白去掉；宽字符只产生一次字符，组合字符保留；
    /// 矩形选择按列截取每一行。时间戳栏不在网格里，永远不会出现在结果中。
    pub fn selection_text(&self) -> Option<String> {
        self.core.selection_range()?;
        let raw = self.core.term.selection_to_string()?;
        let lines = self
            .core
            .term
            .selection
            .as_ref()
            .is_some_and(|s| s.ty == SelectionType::Lines);
        Some(normalize_text(&raw, lines))
    }

    /// 设置选词（双击、右键选词）的分隔字符，默认 [`crate::DEFAULT_WORD_SEPARATORS`]。
    ///
    /// 不在集合里的字符都算单词的一部分，所以默认值不含 `/ . - _ ~`，双击能选中整个路径。
    pub fn set_word_separators(&mut self, separators: &str) {
        self.core.separators = separators.to_owned();
        self.core.set_word_separators();
    }

    /// 当前的选词分隔字符。
    pub fn word_separators(&self) -> &str {
        &self.core.separators
    }
}
