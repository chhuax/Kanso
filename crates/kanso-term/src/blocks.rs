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
}

#[derive(Default)]
pub(crate) struct Blocks {
    entries: VecDeque<Block>,
    next_id: u64,
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
    fn block_point(&self) -> CellPoint {
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
        }
    }

    pub(crate) fn visible_blocks(&self, first: u64, rows: u16) -> Vec<CommandBlock> {
        if self.alt {
            return Vec::new();
        }
        let entries = &self.blocks.entries;
        let index =
            entries.partition_point(|block| block.end.is_some_and(|end| end.line.0 <= first));
        entries
            .iter()
            .skip(index)
            .take_while(|block| block.start.line.0 < first + u64::from(rows))
            .filter_map(|block| {
                let end = block.end.unwrap_or(self.block_point());
                let end_line = end
                    .line
                    .0
                    .saturating_sub(u64::from(end.col == 0 && end.line > block.start.line));
                let command_line = block
                    .execution
                    .filter(|_| block.has_command)
                    .map(|execution| {
                        execution.line.0.saturating_sub(u64::from(
                            execution.col == 0 && execution.line > block.start.line,
                        ))
                    });
                if end_line.max(block.start.line.0) < first {
                    return None;
                }
                Some(CommandBlock {
                    id: block.id,
                    start_line: block.start.line.0,
                    end_line: end_line.max(block.start.line.0),
                    command_line,
                    running: block.has_command && block.finish.is_none() && block.end.is_none(),
                    exit_code: block.has_command.then_some(block.exit_code).flatten(),
                    duration: block
                        .has_command
                        .then(|| block.elapsed_ms.map(command_duration))
                        .flatten(),
                    can_copy_command: block.has_command && !block.clipped && block.input.is_some(),
                    can_copy_output: block.has_command,
                })
            })
            .collect()
    }
}

impl Terminal {
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

    /// 将上一个或下一个完整块的提示符滚动到视口顶部附近。
    pub fn jump_command_block(&mut self, forward: bool) {
        if self.core.alt {
            return;
        }
        let top = self.core.table.screen_top_abs() - self.core.term.grid().display_offset() as u64;
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
            let offset = self
                .core
                .table
                .screen_top_abs()
                .saturating_sub(target.start.line.0) as usize;
            self.dirty |= self.core.scroll_display(Scroll::Delta(
                offset as i32 - self.core.term.grid().display_offset() as i32,
            ));
        }
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
        term.jump_command_block(false);
        let first = term.snapshot(None).header().first_abs_line;
        term.jump_command_block(false);
        assert!(term.snapshot(None).header().first_abs_line < first);
        for _ in 0..12 {
            term.jump_command_block(false);
        }
        let top = term.snapshot(None).header().first_abs_line;
        term.jump_command_block(false);
        assert_eq!(term.snapshot(None).header().first_abs_line, top);
        term.jump_command_block(true);
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
}
