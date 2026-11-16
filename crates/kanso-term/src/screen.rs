//! 屏幕文本读取：给命令跟踪（`track` 模块）用的只读查询。
//!
//! 全部查询都用**绝对行号**定位，所以输出滚动之后仍然有效；行已被回滚裁掉、或全屏程序
//! （alternate screen）正在占用屏幕时返回 `None` / `false`：全屏期间侧表属于被盖住的主屏幕，
//! 网格里读到的是全屏程序的画面，对命令跟踪没有意义，也不能拿来当提示符或命令文本。

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use kanso_types::geometry::{AbsLine, CellPoint};

use crate::Terminal;
use crate::handler::Core;

/// 一个逻辑行最多向上 / 向下追溯的网格行数，防止没有换行的超长输出让一次查询占用过多时间。
const MAX_LOGICAL_ROWS: usize = 64;

impl Core {
    /// 绝对行号 → 网格行；全屏期间、或行不在当前网格（被裁掉 / 还没产生）时为 `None`。
    fn grid_line_of(&self, abs: AbsLine) -> Option<Line> {
        if self.alt {
            return None;
        }
        let grid = self.term.grid();
        let line = i64::try_from(abs.0).ok()? - i64::try_from(self.table.screen_top_abs()).ok()?;
        let hist = grid.history_size() as i64;
        if line < -hist || line >= grid.screen_lines() as i64 {
            return None;
        }
        Some(Line(line as i32))
    }

    /// 该网格行是否以软换行结束（下一行是同一逻辑行的续行）。
    fn row_wraps(&self, line: Line) -> bool {
        let grid = self.term.grid();
        grid[line][Column(self.cols - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    /// 把 `[from, to)` 列的单元格文本追加到 `out`（宽字符占位格跳过，组合字符跟在基字符后）。
    fn push_cells(&self, line: Line, from: usize, to: usize, out: &mut String) {
        let row = &self.term.grid()[line];
        for c in from..to.min(self.cols) {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            out.push(if cell.c == '\0' { ' ' } else { cell.c });
            for z in cell.zerowidth().into_iter().flatten() {
                out.push(*z);
            }
        }
    }

    /// 光标位置；全屏期间为 `None`。
    pub(crate) fn cursor_point(&self) -> Option<CellPoint> {
        if self.alt {
            return None;
        }
        Some(CellPoint {
            line: AbsLine(self.cursor_abs()),
            col: self.term.grid().cursor.point.column.0 as u16,
        })
    }

    /// 包含 `abs` 的逻辑行的第一个网格行（向上追溯软换行）。
    fn logical_start(&self, abs: AbsLine) -> Option<(AbsLine, Line)> {
        let mut line = self.grid_line_of(abs)?;
        let mut abs = abs;
        for _ in 0..MAX_LOGICAL_ROWS {
            let Some(prev_abs) = abs.0.checked_sub(1) else {
                break;
            };
            let Some(prev) = self.grid_line_of(AbsLine(prev_abs)) else {
                break;
            };
            if !self.row_wraps(prev) {
                break;
            }
            abs = AbsLine(prev_abs);
            line = prev;
        }
        Some((abs, line))
    }

    pub(crate) fn same_logical_line(&self, a: AbsLine, b: AbsLine) -> bool {
        match (self.logical_start(a), self.logical_start(b)) {
            (Some(x), Some(y)) => x.0 == y.0,
            _ => false,
        }
    }

    /// 从 `p` 所在逻辑行的行首读到 `p`（不含）。
    pub(crate) fn text_before(&self, p: CellPoint) -> Option<String> {
        let (start_abs, start_line) = self.logical_start(p.line)?;
        let end_line = self.grid_line_of(p.line)?;
        let mut out = String::new();
        let mut line = start_line;
        let mut abs = start_abs.0;
        loop {
            if abs == p.line.0 {
                self.push_cells(end_line, 0, usize::from(p.col), &mut out);
                break;
            }
            // 软换行处的行尾空格是内容，必须整行取。
            self.push_cells(line, 0, self.cols, &mut out);
            line = Line(line.0 + 1);
            abs += 1;
        }
        Some(out)
    }

    /// 从 `p` 读到它所在逻辑行的末尾（含软换行续行）。行尾空白保留给调用方处理。
    pub(crate) fn text_from(&self, p: CellPoint) -> Option<String> {
        let mut line = self.grid_line_of(p.line)?;
        let mut abs = p.line.0;
        let mut out = String::new();
        let mut first = true;
        for _ in 0..MAX_LOGICAL_ROWS {
            let from = if first { usize::from(p.col) } else { 0 };
            first = false;
            let wraps = self.row_wraps(line);
            self.push_cells(line, from, self.cols, &mut out);
            if !wraps {
                break;
            }
            abs += 1;
            let Some(next) = self.grid_line_of(AbsLine(abs)) else {
                break;
            };
            line = next;
        }
        Some(out)
    }
}

impl Terminal {
    /// 光标位置（绝对行号 + 列）；全屏期间为 `None`。
    ///
    /// 命令跟踪用它记下"输入起点"：新提示符出现后、用户第一次按键时光标正好停在提示符末尾。
    pub fn cursor_point(&self) -> Option<CellPoint> {
        self.core.cursor_point()
    }

    /// `a`、`b` 两个绝对行是否属于同一个逻辑行（软换行连起来的行）。任一行已不在网格里、
    /// 或处于全屏期间返回 `false`。
    pub fn same_logical_line(&self, a: AbsLine, b: AbsLine) -> bool {
        self.core.same_logical_line(a, b)
    }

    /// `p` 所在逻辑行从行首到 `p`（不含）的文本，即"提示符"。行已不在网格里、或全屏期间为 `None`。
    pub fn text_before(&self, p: CellPoint) -> Option<String> {
        self.core.text_before(p)
    }

    /// 从 `p` 到它所在逻辑行末尾的文本（含软换行续行），即"用户键入的命令行"；
    /// 末尾的空白原样保留。行已不在网格里、或全屏期间为 `None`。
    pub fn text_from(&self, p: CellPoint) -> Option<String> {
        self.core.text_from(p)
    }
}
