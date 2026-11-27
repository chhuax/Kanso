use std::collections::VecDeque;

use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use kanso_frame::CommandBlock;
use kanso_types::Message;
use kanso_types::geometry::{AbsLine, CellPoint};

use crate::Terminal;
use crate::handler::Core;
use crate::osc::Osc133;
use crate::selection::normalize_text;
use crate::table::Group;

#[derive(Clone, Debug)]
struct Block {
    id: u64,
    start: CellPoint,
    input: Option<CellPoint>,
    execution: Option<CellPoint>,
    finish: Option<CellPoint>,
    end: Option<CellPoint>,
    started_ms: i64,
    elapsed_ms: Option<u64>,
    exit_code: Option<i32>,
    has_command: bool,
    clipped: bool,
    folded: bool,
}

#[derive(Default)]
pub(crate) struct Blocks {
    entries: VecDeque<Block>,
    next_id: u64,
    /// 有折叠块时使用的视口滚动偏移（单位：可见行，0 为贴底）。
    pub(crate) fold_offset: usize,
}

#[derive(Default)]
pub(crate) struct ReflowColumns {
    width: usize,
    padding: Vec<u64>,
}

impl ReflowColumns {
    pub fn capture(grid: &Grid<Cell>, first: u64) -> Self {
        let width = grid.columns();
        let padding = (0..grid.total_lines())
            .filter(|&row| {
                grid[Line(row as i32 - grid.history_size() as i32)][Column(width - 1)]
                    .flags
                    .contains(Flags::LEADING_WIDE_CHAR_SPACER)
            })
            .map(|row| first + row as u64)
            .collect();
        Self { width, padding }
    }
}

impl Blocks {
    pub fn prompt_columns(&self, line: AbsLine, cols: u16) -> Option<std::ops::Range<u16>> {
        let index = self
            .entries
            .partition_point(|block| block.start.line <= line)
            .checked_sub(1)?;
        let block = &self.entries[index];
        if block.clipped {
            return None;
        }
        let end = block.input.or(block.end);
        if end.is_some_and(|end| end.line < line) {
            return None;
        }
        let start_col = if block.start.line == line {
            block.start.col
        } else {
            0
        };
        let end_col = end
            .filter(|end| end.line == line)
            .map_or(cols, |end| end.col);
        Some(start_col..end_col)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.fold_offset = 0;
    }

    pub fn remap(
        &mut self,
        old: &[Group],
        old_first: u64,
        columns: (&ReflowColumns, &ReflowColumns),
        counts: &[usize],
        mapping: &[Option<usize>],
        first: u64,
    ) {
        let (old_columns, new_columns) = columns;
        let (old_cols, cols) = (old_columns.width, new_columns.width);
        let mut destinations = vec![None; old.len()];
        let mut row = first;
        for (&count, &source) in counts.iter().zip(mapping) {
            if let Some(index) = source.filter(|&index| index < old.len()) {
                destinations[index] = Some((row, count));
            }
            row += count as u64;
        }
        let remap = |point: CellPoint| -> Option<CellPoint> {
            let index = usize::try_from(point.line.0.checked_sub(old_first)?).ok()?;
            let group = old
                .partition_point(|group| group.start <= index)
                .checked_sub(1)?;
            let (start, count) = destinations[group]?;
            let group_start = old_first + old[group].start as u64;
            let before_group = old_columns
                .padding
                .partition_point(|&line| line < group_start);
            let before_point = old_columns.padding.partition_point(|&line| {
                line < point.line.0 || (line == point.line.0 && usize::from(point.col) == old_cols)
            });
            let mut offset = (index - old[group].start) * old_cols + usize::from(point.col)
                - (before_point - before_group);
            let padding_start = new_columns.padding.partition_point(|&line| line < start);
            for &line in &new_columns.padding[padding_start..] {
                if line >= start + count as u64
                    || (line - start) as usize * cols + cols - 1 > offset
                {
                    break;
                }
                offset += 1;
            }
            if offset > count * cols {
                return None;
            }
            if offset == count * cols {
                return Some(CellPoint {
                    line: AbsLine(start + count as u64 - 1),
                    col: cols as u16,
                });
            }
            Some(CellPoint {
                line: AbsLine(start + (offset / cols) as u64),
                col: (offset % cols) as u16,
            })
        };
        self.entries.retain_mut(|block| {
            let start = remap(block.start);
            block.input = block.input.and_then(remap);
            block.execution = block.execution.and_then(remap);
            block.finish = block.finish.and_then(remap);
            block.end = block.end.and_then(remap);
            if let Some(start) = start {
                block.start = start;
                true
            } else if block.end.is_some() || block.finish.is_some() || block.execution.is_some() {
                block.start = CellPoint {
                    line: AbsLine(first),
                    col: 0,
                };
                block.clipped = true;
                block.input = None;
                block.folded = false;
                true
            } else {
                false
            }
        });
    }
}

fn command_duration(milliseconds: u64) -> Message {
    if milliseconds < 1_000 {
        Message::CommandDurationMilliseconds {
            value: milliseconds.to_string(),
        }
    } else if milliseconds < 60_000 {
        Message::CommandDurationSeconds {
            value: format!("{:.1}", milliseconds as f64 / 1_000.0),
        }
    } else if milliseconds < 3_600_000 {
        Message::CommandDurationMinutes {
            minutes: (milliseconds / 60_000).to_string(),
            seconds: format!("{:02}", milliseconds / 1_000 % 60),
        }
    } else {
        Message::CommandDurationHours {
            hours: (milliseconds / 3_600_000).to_string(),
            minutes: format!("{:02}", milliseconds / 60_000 % 60),
        }
    }
}

impl Core {
    pub(crate) fn block_point(&self) -> CellPoint {
        CellPoint {
            line: AbsLine(self.cursor_abs()),
            col: (self.term.grid().cursor.point.column.0
                + usize::from(self.term.grid().cursor.input_needs_wrap)) as u16,
        }
    }

    pub(crate) fn range_text(&mut self, start: CellPoint, end: CellPoint) -> String {
        let start = if usize::from(start.col) == self.cols {
            CellPoint {
                line: AbsLine(start.line.0 + 1),
                col: 0,
            }
        } else {
            start
        };
        let start = start.max(CellPoint {
            line: AbsLine(self.table.first_abs()),
            col: 0,
        });
        if end <= start {
            return String::new();
        }
        let top = self.table.screen_top_abs() as i64;
        let point = |point: CellPoint| {
            Point::new(
                Line((point.line.0 as i64 - top) as i32),
                Column(usize::from(point.col)),
            )
        };
        let mut selection = Selection::new(SelectionType::Simple, point(start), Side::Left);
        let last = if end.col == 0 {
            CellPoint {
                line: AbsLine(end.line.0 - 1),
                col: (self.cols - 1) as u16,
            }
        } else {
            CellPoint {
                col: end.col - 1,
                ..end
            }
        };
        selection.update(point(last), Side::Right);
        let saved = self.term.selection.replace(selection);
        let text = self.term.selection_to_string().unwrap_or_default();
        self.term.selection = saved;
        normalize_text(&text, false).trim_end().to_owned()
    }

    pub(crate) fn on_block_marker(&mut self, marker: &Osc133) {
        let point = self.block_point();
        match marker {
            Osc133::PromptStart { .. } => {
                if self
                    .blocks
                    .entries
                    .back()
                    .is_some_and(|block| block.start.line == point.line)
                {
                    return;
                }
                if let Some(block) = self.blocks.entries.back_mut() {
                    block.end = Some(point);
                }
                self.blocks.next_id += 1;
                self.blocks.entries.push_back(Block {
                    id: self.blocks.next_id,
                    start: point,
                    input: None,
                    execution: None,
                    finish: None,
                    end: None,
                    started_ms: 0,
                    elapsed_ms: None,
                    exit_code: None,
                    has_command: false,
                    clipped: false,
                    folded: false,
                });
            }
            Osc133::InputStart => {
                if let Some(block) = self
                    .blocks
                    .entries
                    .back_mut()
                    .filter(|block| block.execution.is_none())
                {
                    block.input = Some(point);
                }
            }
            Osc133::CommandStart => {
                let input = self
                    .blocks
                    .entries
                    .back()
                    .filter(|block| block.execution.is_none())
                    .and_then(|block| block.input);
                let has_command =
                    input.is_some_and(|input| !self.range_text(input, point).trim().is_empty());
                if let Some(block) = self
                    .blocks
                    .entries
                    .back_mut()
                    .filter(|block| block.execution.is_none())
                {
                    block.execution = Some(point);
                    block.started_ms = self.now_ms;
                    block.has_command = has_command;
                }
            }
            Osc133::CommandFinished { exit_code } => {
                if let Some(block) = self
                    .blocks
                    .entries
                    .back_mut()
                    .filter(|block| block.execution.is_some() && block.finish.is_none())
                {
                    block.finish = Some(point);
                    block.elapsed_ms =
                        Some(self.now_ms.saturating_sub(block.started_ms).max(0) as u64);
                    block.exit_code = *exit_code;
                }
            }
        }
    }

    pub(crate) fn prune_blocks(&mut self) {
        let first = self.table.first_abs();
        while self
            .blocks
            .entries
            .front()
            .is_some_and(|block| block.end.is_some_and(|end| end.line.0 <= first))
        {
            self.blocks.entries.pop_front();
        }
        if let Some(block) = self
            .blocks
            .entries
            .front_mut()
            .filter(|block| block.start.line.0 < first)
        {
            block.start = CellPoint {
                line: AbsLine(first),
                col: 0,
            };
            block.input = None;
            block.clipped = true;
            block.folded = false;
        }
    }

    fn block_command_line(block: &Block) -> Option<u64> {
        block
            .execution
            .filter(|_| block.has_command)
            .map(|execution| {
                execution.line.0.saturating_sub(u64::from(
                    execution.col == 0 && execution.line > block.start.line,
                ))
            })
    }

    fn block_end_line(block: &Block, now: CellPoint) -> u64 {
        let end = block.end.unwrap_or(now);
        end.line
            .0
            .saturating_sub(u64::from(end.col == 0 && end.line > block.start.line))
            .max(block.start.line.0)
    }

    /// 块可折叠时返回被隐藏的输出绝对行范围（含端点）：`(command_line + 1, end_line)`。
    fn block_fold_range(block: &Block, now: CellPoint) -> Option<(u64, u64)> {
        let running = block.has_command && block.finish.is_none() && block.end.is_none();
        if running || !block.has_command || block.clipped {
            return None;
        }
        let cmd = Self::block_command_line(block)?;
        let end_line = Self::block_end_line(block, now);
        (end_line > cmd).then_some((cmd + 1, end_line))
    }

    /// 当前是否存在任何已折叠的命令块（全屏期间视为无）。
    pub(crate) fn has_folded(&self) -> bool {
        !self.alt && self.blocks.entries.iter().any(|b| b.folded)
    }

    /// 返回按绝对行号升序排列、互不重叠的隐藏行区间（含端点），裁剪到当前缓冲范围内。
    pub(crate) fn hidden_ranges(&self) -> Vec<(u64, u64)> {
        if !self.has_folded() {
            return Vec::new();
        }
        let first = self.table.first_abs();
        let last = self.table.screen_top_abs() + self.rows as u64 - 1;
        let now = self.block_point();
        let mut ranges = Vec::new();
        for block in &self.blocks.entries {
            if !block.folded {
                continue;
            }
            if let Some((s, e)) = Self::block_fold_range(block, now)
                && s <= last
                && e >= first
            {
                ranges.push((s.max(first), e.min(last)));
            }
        }
        ranges
    }

    /// 计算当前视口每一行对应的绝对行号（折叠导致可见总行数少于视口时末尾补 `None`），
    /// 同时返回 `(visual_offset, max_visual_offset)`。
    pub(crate) fn viewport_map(&self) -> (Vec<Option<u64>>, usize, usize) {
        let rows = self.rows;
        let hist = self.term.grid().history_size();
        let hidden = self.hidden_ranges();
        if hidden.is_empty() {
            let off = self.term.grid().display_offset();
            let top = self.table.screen_top_abs() - off as u64;
            let lines = (0..rows).map(|r| Some(top + r as u64)).collect();
            return (lines, off, hist);
        }
        let first = self.table.first_abs();
        let last = self.table.screen_top_abs() + rows as u64 - 1;
        let total_logical = hist + rows;
        let total_hidden: usize = hidden.iter().map(|&(s, e)| (e - s + 1) as usize).sum();
        let total_visible = total_logical.saturating_sub(total_hidden);
        let max_off = total_visible.saturating_sub(rows);
        let off = self.blocks.fold_offset.min(max_off);
        let mut skip = max_off - off;
        let mut lines = Vec::with_capacity(rows);
        let mut cur = first;
        let mut hi = 0usize;
        while cur <= last && lines.len() < rows {
            if hi < hidden.len() && cur >= hidden[hi].0 {
                cur = hidden[hi].1.saturating_add(1);
                hi += 1;
                continue;
            }
            let seg_end = if hi < hidden.len() {
                hidden[hi].0.saturating_sub(1).min(last)
            } else {
                last
            };
            let seg_len = (seg_end - cur + 1) as usize;
            if skip >= seg_len {
                skip -= seg_len;
                cur = seg_end.saturating_add(1);
                continue;
            }
            cur += skip as u64;
            skip = 0;
            let take = ((seg_end - cur + 1) as usize).min(rows - lines.len());
            for i in 0..take {
                lines.push(Some(cur + i as u64));
            }
            cur += take as u64;
        }
        lines.resize(rows, None);
        (lines, off, max_off)
    }

    /// 计算绝对行 `abs` 在可见行序列里的下标（隐藏行夹到它前面最近的可见行）。
    pub(crate) fn visible_index_of(&self, abs: u64, hidden: &[(u64, u64)]) -> usize {
        let first = self.table.first_abs();
        if abs <= first {
            return 0;
        }
        let mut hidden_before = 0u64;
        for &(s, e) in hidden {
            if s > abs {
                break;
            }
            let hi = e.min(abs);
            if hi >= s {
                hidden_before += hi - s + 1;
            }
        }
        (abs - first).saturating_sub(hidden_before) as usize
    }

    /// 若 `abs` 落在某个已折叠块的隐藏输出里，展开该块并返回 true。
    pub(crate) fn unfold_line(&mut self, abs: u64) -> bool {
        if !self.has_folded() {
            return false;
        }
        let now = self.block_point();
        let mut changed = false;
        for block in &mut self.blocks.entries {
            if block.folded
                && let Some((s, e)) = Self::block_fold_range(block, now)
                && (s..=e).contains(&abs)
            {
                block.folded = false;
                changed = true;
            }
        }
        if changed && !self.has_folded() {
            let target = self.blocks.fold_offset.min(self.term.grid().history_size());
            let cur = self.term.grid().display_offset();
            if target != cur {
                self.term
                    .scroll_display(Scroll::Delta(target as i32 - cur as i32));
                self.drain_events();
            }
        }
        changed
    }

    pub(crate) fn visible_blocks(&self, first: u64, last: u64) -> Vec<CommandBlock> {
        if self.alt || first > last {
            return Vec::new();
        }
        let now = self.block_point();
        let entries = &self.blocks.entries;
        let index =
            entries.partition_point(|block| block.end.is_some_and(|end| end.line.0 <= first));
        entries
            .iter()
            .skip(index)
            .take_while(|block| block.start.line.0 <= last)
            .filter_map(|block| {
                let end_line = Self::block_end_line(block, now);
                let command_line = Self::block_command_line(block);
                if end_line < first {
                    return None;
                }
                let running = block.has_command && block.finish.is_none() && block.end.is_none();
                let foldable = Self::block_fold_range(block, now).is_some();
                Some(CommandBlock {
                    id: block.id,
                    latest: block.id == self.blocks.next_id,
                    start_line: block.start.line.0,
                    end_line,
                    command_line,
                    running,
                    exit_code: block.has_command.then_some(block.exit_code).flatten(),
                    duration: block
                        .has_command
                        .then(|| block.elapsed_ms.map(command_duration))
                        .flatten(),
                    can_copy_command: block.has_command && !block.clipped && block.input.is_some(),
                    can_copy_output: block.has_command,
                    foldable,
                    folded: block.folded && foldable,
                })
            })
            .collect()
    }
}

impl Terminal {
    /// OSC 133 B 之后，光标是否仍停在输入起点上。
    ///
    /// 用于判断输入行是否为空（spec/mcp.md §4.2）：用户输入过任何字符，光标都会离开输入起点。
    /// 只看光标位置，不看起点之后的文字（zsh 自动建议的灰字不会移动光标）。
    /// 没有 OSC 133 B 的信息（没有集成、命令正在执行、屏幕刚被清空、全屏期间）时返回 `None`，由调用方按"不要求"处理。
    pub fn at_input_start(&self) -> Option<bool> {
        if self.core.alt {
            return None;
        }
        let block = self.core.blocks.entries.back()?;
        if block.execution.is_some() {
            return None;
        }
        Some(self.core.block_point() == block.input?)
    }

    /// 提取块的命令（0）、输出（1）或全部（2），规则与选区复制相同。
    pub fn command_block_text(&mut self, id: u64, part: u8) -> Option<String> {
        if self.core.alt {
            return None;
        }
        let block = self
            .core
            .blocks
            .entries
            .iter()
            .find(|block| block.id == id)?
            .clone();
        let first = CellPoint {
            line: AbsLine(self.core.table.first_abs()),
            col: 0,
        };
        let end = block.end.unwrap_or(self.core.block_point());
        let command = if part != 1 && block.has_command && !block.clipped {
            block
                .input
                .zip(block.execution)
                .map(|(start, end)| self.core.range_text(start, end))
        } else {
            None
        };
        let output = if part != 0 && block.has_command {
            Some({
                let start = block.execution.unwrap_or(block.start);
                let text = self
                    .core
                    .range_text(start.max(first), block.finish.unwrap_or(end));
                text.strip_prefix('\n').unwrap_or(&text).to_owned()
            })
        } else {
            None
        };
        match part {
            0 => command,
            1 => output,
            _ => {
                if block.has_command {
                    let mut text = command.unwrap_or_default();
                    let output = output.unwrap_or_default();
                    if !text.is_empty() && !output.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&output);
                    Some(text)
                } else {
                    Some(self.core.range_text(block.start, end))
                }
            }
        }
    }

    /// 切换指定命令块的折叠状态；块不存在或没有可折叠输出时返回 false。
    pub fn toggle_command_block_fold(&mut self, id: u64) -> bool {
        if self.core.alt {
            return false;
        }
        let was_folded = self.core.has_folded();
        if !was_folded {
            self.core.blocks.fold_offset = self.core.term.grid().display_offset();
        }
        let now = self.core.block_point();
        let Some(block) = self
            .core
            .blocks
            .entries
            .iter_mut()
            .find(|block| block.id == id)
        else {
            return false;
        };
        if Core::block_fold_range(block, now).is_none() {
            return false;
        }
        block.folded = !block.folded;
        let (_, off, _) = self.core.viewport_map();
        self.core.blocks.fold_offset = off;
        if !self.core.has_folded() {
            let cur = self.core.term.grid().display_offset();
            if off != cur {
                self.core
                    .term
                    .scroll_display(Scroll::Delta(off as i32 - cur as i32));
                self.core.drain_events();
            }
        }
        self.dirty = true;
        true
    }

    /// 将上一个或下一个完整块的提示符滚动到视口顶部附近。
    pub fn jump_command_block(&mut self, forward: bool) -> Option<u64> {
        if self.core.alt {
            return None;
        }
        let (viewport, cur_off, max_off) = self.core.viewport_map();
        let top = viewport
            .first()
            .copied()
            .flatten()
            .unwrap_or(self.core.table.screen_top_abs());
        let entries = &self.core.blocks.entries;
        let target = if forward {
            entries
                .iter()
                .find(|block| !block.clipped && block.start.line.0 > top)
        } else {
            entries
                .iter()
                .rev()
                .find(|block| !block.clipped && block.start.line.0 < top)
        };
        if let Some(target) = target {
            let id = target.id;
            let hidden = self.core.hidden_ranges();
            let target_off = if hidden.is_empty() {
                self.core
                    .table
                    .screen_top_abs()
                    .saturating_sub(target.start.line.0) as usize
            } else {
                let vis_idx = self.core.visible_index_of(target.start.line.0, &hidden);
                max_off.saturating_sub(vis_idx)
            };
            self.dirty |= self
                .core
                .scroll_display(Scroll::Delta(target_off as i32 - cur_off as i32));
            return Some(id);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TermConfig;
    use kanso_types::geometry::GridSize;

    const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";
    const EXECUTE: &str = "\r\n\x1b]133;C\x07";

    fn terminal(cols: u16, rows: u16, history: usize) -> Terminal {
        Terminal::new(TermConfig {
            size: GridSize { cols, rows },
            scrollback_lines: history,
        })
    }

    fn feed(term: &mut Terminal, text: &str, now: i64) {
        term.feed(text.as_bytes(), now);
    }

    fn run(term: &mut Terminal, command: &str, output: &str, code: &str) {
        feed(term, &format!("{PROMPT}{command}{EXECUTE}{output}"), 100);
        feed(term, &format!("\x1b]133;D{code}\x07{PROMPT}"), 420);
    }

    #[test]
    fn status_exit_code_and_elapsed_follow_markers() {
        for code in [";0", ";3", ""] {
            let mut term = terminal(60, 12, 100);
            run(&mut term, "echo hello", "hello\r\n", code);
            let frame = term.snapshot(None);
            let block = &frame.command_blocks[0];
            assert_eq!(block.start_line, 0);
            assert_eq!(block.command_line, Some(0));
            assert_eq!(block.end_line, 1);
            assert!(!block.running);
            assert_eq!(
                block.exit_code,
                match code {
                    ";0" => Some(0),
                    ";3" => Some(3),
                    _ => None,
                }
            );
            assert_eq!(block.duration, Some(command_duration(320)));
            assert_eq!(
                term.command_block_text(block.id, 0).as_deref(),
                Some("echo hello")
            );
            assert_eq!(
                term.command_block_text(block.id, 1).as_deref(),
                Some("hello")
            );
            assert_eq!(
                term.command_block_text(block.id, 2).as_deref(),
                Some("echo hello\nhello")
            );
        }
    }

    #[test]
    fn running_and_no_output() {
        let mut term = terminal(40, 10, 100);
        feed(&mut term, &format!("{PROMPT}true{EXECUTE}"), 200);
        let block = term.snapshot(None).command_blocks[0].clone();
        assert!(block.running);
        assert!(block.duration.is_none());
        feed(&mut term, "\x1b]133;D;0\x07", 240);
        assert_eq!(term.command_block_text(block.id, 1).as_deref(), Some(""));
        assert_eq!(
            term.command_block_text(block.id, 2).as_deref(),
            Some("true")
        );
    }

    #[test]
    fn empty_enter_interrupt_and_repaint_have_no_command_status() {
        let mut term = terminal(40, 10, 100);
        feed(
            &mut term,
            &format!(
                "{PROMPT}\r\x1b]133;A\x07\r\n{PROMPT}^C\r\n\x1b]133;D;130\x07{PROMPT}{EXECUTE}\x1b]133;D;130\x07{PROMPT}"
            ),
            1,
        );
        let frame = term.snapshot(None);
        assert_eq!(frame.command_blocks.len(), 4);
        for block in frame.command_blocks {
            assert!(block.command_line.is_none());
            assert!(!block.running);
            assert!(block.duration.is_none());
            assert!(block.exit_code.is_none());
            assert!(!block.can_copy_command);
            assert!(!block.can_copy_output);
        }
    }

    #[test]
    fn scrolled_empty_prompt_is_not_the_latest_block() {
        let mut term = terminal(40, 4, 100);
        for time in 0..12 {
            feed(&mut term, PROMPT, time);
            feed(&mut term, "\r\n\r\n", time);
        }
        let current = term.snapshot(None);
        assert!(current.command_blocks.last().unwrap().latest);
        term.scroll_display(100);
        let scrolled = term.snapshot(None);
        assert!(!scrolled.command_blocks.is_empty());
        assert!(scrolled.command_blocks.iter().all(|block| !block.latest));
    }

    #[test]
    fn missing_d_ends_output_at_next_prompt() {
        let mut term = terminal(40, 10, 100);
        feed(
            &mut term,
            &format!("{PROMPT}pwd{EXECUTE}/tmp\r\n{PROMPT}"),
            1,
        );
        let block = term.snapshot(None).command_blocks[0].clone();
        assert!(!block.running);
        assert_eq!(
            term.command_block_text(block.id, 1).as_deref(),
            Some("/tmp")
        );
    }

    #[test]
    fn plain_remote_and_alternate_screen_do_not_create_blocks() {
        let mut term = terminal(40, 10, 100);
        feed(&mut term, "$ echo plain\r\nplain\r\n$ ", 1);
        assert!(term.snapshot(None).command_blocks.is_empty());
        run(&mut term, "top", "", ";0");
        let before = term.snapshot(None).command_blocks;
        feed(
            &mut term,
            &format!("\x1b[?1049h{PROMPT}fake{EXECUTE}\x1b]133;D;3\x07"),
            2,
        );
        assert!(term.snapshot(None).command_blocks.is_empty());
        feed(&mut term, "\x1b[?1049l", 3);
        assert_eq!(term.snapshot(None).command_blocks.len(), before.len());
    }

    #[test]
    fn trimming_retains_only_remaining_output_and_disables_command() {
        let mut term = terminal(30, 4, 3);
        run(
            &mut term,
            "seq 12",
            &(1..=12)
                .map(|number| format!("{number}\r\n"))
                .collect::<String>(),
            ";0",
        );
        term.scroll_display(100);
        let block = term.snapshot(None).command_blocks[0].clone();
        assert!(!block.can_copy_command);
        assert_eq!(block.start_line, term.core.table.first_abs());
        assert!(term.command_block_text(block.id, 0).is_none());
        let output = term.command_block_text(block.id, 1).unwrap();
        assert!(output.ends_with("12"));
        assert!(!output.contains("\n1\n"));
        for number in 0..8 {
            run(&mut term, "echo x", &format!("x{number}\r\n"), ";0");
        }
        assert!(term.command_block_text(block.id, 2).is_none());
        assert!(term.core.blocks.entries.len() < 6);
    }

    #[test]
    fn clear_both_orders_and_explicit_clear_remove_old_blocks() {
        for clear in ["\x1b[2J\x1b[3J", "\x1b[3J\x1b[2J"] {
            let mut term = terminal(40, 10, 100);
            run(&mut term, "echo hello", "hello\r\n", ";0");
            feed(&mut term, clear, 500);
            assert!(term.snapshot(None).command_blocks.is_empty());
        }
        let mut term = terminal(40, 10, 100);
        run(&mut term, "echo hello", "hello\r\n", ";0");
        term.clear_buffer();
        assert!(term.snapshot(None).command_blocks.is_empty());
    }

    #[test]
    fn ed2_alone_preserves_blocks_in_history() {
        let mut term = terminal(40, 10, 100);
        run(&mut term, "echo hello", "hello\r\n", ";0");
        let id = term.snapshot(None).command_blocks[0].id;
        feed(&mut term, "\x1b[2J\x1b[Hredraw", 600);
        assert_eq!(
            term.command_block_text(id, 0).as_deref(),
            Some("echo hello")
        );
        term.scroll_display(100);
        assert_eq!(term.snapshot(None).command_blocks[0].id, id);
    }

    #[test]
    fn reflow_keeps_command_output_and_boundaries() {
        let mut term = terminal(32, 8, 100);
        run(
            &mut term,
            "printf abcdefghijklmnopqrstuvwxyz",
            "abcdefghijklmnopqrstuvwxyz\r\n",
            ";3",
        );
        let original = term.snapshot(None).command_blocks[0].clone();
        let text = term.command_block_text(original.id, 2).unwrap();
        for cols in [12, 48, 20, 32] {
            term.resize(GridSize { cols, rows: 8 });
            assert_eq!(
                term.command_block_text(original.id, 2).as_deref(),
                Some(text.as_str()),
                "cols={cols}"
            );
            term.scroll_display(100);
            let blocks = term.snapshot(None).command_blocks;
            let block = blocks.iter().find(|block| block.id == original.id).unwrap();
            assert!(block.command_line.unwrap() >= block.start_line);
            assert!(block.end_line >= block.command_line.unwrap());
        }
    }

    #[test]
    fn copy_merges_soft_wrap_unicode_and_trims_line_ends_without_touching_selection() {
        let mut term = terminal(12, 12, 100);
        run(
            &mut term,
            "echo 中文abcdef",
            "\x1b[31m中文e\u{301}abcdefgh\x1b[0m   \r\nsecond  \r\n",
            ";0",
        );
        let id = term.snapshot(None).command_blocks[0].id;
        term.select_all();
        let selection = term.selection_text();
        assert_eq!(
            term.command_block_text(id, 0).as_deref(),
            Some("echo 中文abcdef")
        );
        assert_eq!(
            term.command_block_text(id, 1).as_deref(),
            Some("中文e\u{301}abcdefgh\nsecond")
        );
        assert_eq!(term.selection_text(), selection);
    }

    #[test]
    fn jump_moves_viewport_only_and_stops_at_endpoints() {
        let mut term = terminal(40, 4, 100);
        for _ in 0..8 {
            run(&mut term, "echo ok", "ok\r\n", ";0");
        }
        term.select_all();
        let selection = term.selection_text();
        let input = term.core.term.grid().cursor.point;
        let target = term.jump_command_block(false).expect("存在上一个块");
        assert!(
            term.snapshot(None)
                .command_blocks
                .iter()
                .any(|block| block.id == target)
        );
        let first = term.snapshot(None).header().first_abs_line;
        assert!(term.jump_command_block(false).is_some());
        assert!(term.snapshot(None).header().first_abs_line < first);
        for _ in 0..12 {
            let _ = term.jump_command_block(false);
        }
        let top = term.snapshot(None).header().first_abs_line;
        assert_eq!(term.jump_command_block(false), None);
        assert_eq!(term.snapshot(None).header().first_abs_line, top);
        let target = term.jump_command_block(true).expect("存在下一个块");
        assert!(
            term.snapshot(None)
                .command_blocks
                .iter()
                .any(|block| block.id == target)
        );
        assert!(term.snapshot(None).header().first_abs_line > top);
        assert_eq!(term.selection_text(), selection);
        assert_eq!(term.core.term.grid().cursor.point, input);
    }

    #[test]
    fn durations_use_specified_units_and_precision() {
        assert_eq!(
            command_duration(999),
            Message::CommandDurationMilliseconds {
                value: "999".into()
            }
        );
        assert_eq!(
            command_duration(1_000),
            Message::CommandDurationSeconds {
                value: "1.0".into()
            }
        );
        assert_eq!(
            command_duration(4_200),
            Message::CommandDurationSeconds {
                value: "4.2".into()
            }
        );
        assert_eq!(
            command_duration(65_000),
            Message::CommandDurationMinutes {
                minutes: "1".into(),
                seconds: "05".into()
            }
        );
        assert_eq!(
            command_duration(3_720_000),
            Message::CommandDurationHours {
                hours: "1".into(),
                minutes: "02".into()
            }
        );
    }

    #[test]
    fn c_before_newline_at_right_edge_and_wide_character_reflow() {
        for command in ["1234567890", "abcdefghij中中文abcdefgh"] {
            let mut term = terminal(12, 10, 100);
            feed(
                &mut term,
                &format!("{PROMPT}{command}\x1b]133;C\x07\r\noutput\r\n\x1b]133;D;0\x07{PROMPT}"),
                10,
            );
            let id = term.snapshot(None).command_blocks[0].id;
            assert_eq!(term.command_block_text(id, 0).as_deref(), Some(command));
            for cols in [7, 17, 12] {
                term.resize(GridSize { cols, rows: 10 });
                assert_eq!(
                    term.command_block_text(id, 0).as_deref(),
                    Some(command),
                    "cols={cols}"
                );
                assert_eq!(
                    term.command_block_text(id, 1).as_deref(),
                    Some("output"),
                    "cols={cols}"
                );
            }
        }
    }

    #[test]
    fn resize_during_alternate_screen_restores_main_block_positions() {
        let mut term = terminal(16, 6, 100);
        run(&mut term, "echo 中中文abcdefg", "中中文abcdefg\r\n", ";3");
        let id = term.snapshot(None).command_blocks[0].id;
        let text = term.command_block_text(id, 2).unwrap();
        feed(&mut term, "\x1b[?1049h", 500);
        term.resize(GridSize { cols: 9, rows: 8 });
        assert!(term.snapshot(None).command_blocks.is_empty());
        feed(&mut term, "\x1b[?1049l", 600);
        assert_eq!(
            term.command_block_text(id, 2).as_deref(),
            Some(text.as_str())
        );
    }

    #[test]
    fn million_output_lines_keep_metadata_bounded_and_copy_retained_tail() {
        let mut term = terminal(30, 8, 16);
        feed(&mut term, &format!("{PROMPT}yes{EXECUTE}"), 1);
        let id = term.snapshot(None).command_blocks[0].id;
        feed(&mut term, &"y\r\n".repeat(1_000_000), 2);
        let frame = term.snapshot(None);
        assert_eq!(term.core.blocks.entries.len(), 1);
        assert_eq!(frame.command_blocks.len(), 1);
        assert!(frame.command_blocks[0].running);
        assert!(!frame.command_blocks[0].can_copy_command);
        assert!(term.command_block_text(id, 1).unwrap().len() < 50);
        term.resize(GridSize { cols: 20, rows: 8 });
        assert!(term.command_block_text(id, 1).unwrap().contains('y'));
    }

    #[test]
    fn folding_hides_output_preserves_copy_and_unfolds_on_search() {
        let mut term = terminal(40, 10, 100);
        // 块 1：有 3 行输出 → 可折叠；块 2：无输出 → 不可折叠。
        run(&mut term, "ls", "alpha\r\nbeta\r\ngamma\r\n", ";0");
        run(&mut term, "cd /tmp", "", ";0");
        let f0 = term.snapshot(None);
        assert!(f0.command_blocks[0].foldable);
        assert!(!f0.command_blocks[0].folded);
        assert!(!f0.command_blocks[1].foldable);
        let id1 = f0.command_blocks[0].id;
        let id2 = f0.command_blocks[1].id;
        assert!(!term.toggle_command_block_fold(id2));

        // 折叠块 1：视口行数拉紧（块 2 紧跟块 1 命令行），完整输出仍可复制。
        assert!(term.toggle_command_block_fold(id1));
        let f1 = term.snapshot(None);
        assert!(f1.command_blocks[0].folded);
        let view1 = f1.view().unwrap();
        assert_eq!(view1.line_meta(0).unwrap().abs_line, 0);
        assert_eq!(view1.line_meta(1).unwrap().abs_line, 4);
        assert_eq!(
            term.command_block_text(id1, 1).as_deref(),
            Some("alpha\nbeta\ngamma")
        );

        // 搜索命中的隐藏行会自动展开该块。
        let status = term.search(&crate::SearchQuery {
            text: "beta".into(),
            regex: false,
            case_sensitive: false,
        });
        assert_eq!(status.total, 1);
        let f2 = term.snapshot(None);
        assert!(!f2.command_blocks[0].folded);
        let view2 = f2.view().unwrap();
        assert_eq!(view2.line_meta(1).unwrap().abs_line, 1);
    }
}
