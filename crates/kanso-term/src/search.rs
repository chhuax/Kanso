//! 缓冲区搜索（terminal.md §11）。
//!
//! # 匹配方式
//!
//! 按**逻辑行**匹配：软换行连在一起的若干网格行拼成一段文本再交给 `regex`，所以跨越换行点的
//! 匹配也能找到。匹配结果记成绝对行号 + 列的闭区间（[`CellRange`]），绝对行号不随滚动变化，
//! 因此匹配项在内容滚动时不需要更新。
//!
//! 不用 alacritty 自带的 `RegexSearch`：它固定“有大写字母才区分大小写”（智能大小写），
//! 与 `SearchQuery::case_sensitive` 的语义不符；它还是逐单元格走 DFA，在 10 万行回滚上
//! 达不到 50 ms（见 `tests::search_perf_100k_lines`）。
//!
//! # 新输出到来时的更新（增量）
//!
//! `Terminal::feed` 之后只重搜“可能变了的行”：上次搜索时屏幕顶行所在逻辑行及其之后的全部行。
//! 在它之前的回滚行内容不会再变（终端只会往回滚追加行），已有匹配原样保留；回滚被
//! 裁掉的最老的行对应的匹配丢弃。有“非追加”改动（ED 3、⌘K、RIS、resize、调回滚上限、
//! 切换全屏）时 `Core::epoch` / 全屏标志会变，此时全量重搜。
//!
//! 匹配数达到上限后保留已定型的前缀；需要补齐时从最后保留的逻辑行继续搜索。
//!
//! # 已知限制
//!
//! - 单个逻辑行最多拼 4096 个网格行（防止没有换行的超长输出让一次匹配占用过多内存），
//!   更长的逻辑行会被截成多段，跨段的匹配找不到。
//! - 正则用 Rust `regex` 语法（线性时间，不会灾难回溯），不支持环视与反向引用。
//! - 只匹配字符，不匹配样式；全屏程序期间只搜索当前全屏画面，没有回滚。

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Flags, LineLength};
use kanso_types::geometry::{AbsLine, CellPoint};
use regex::{Regex, RegexBuilder};

use crate::Terminal;
use crate::handler::Core;
use crate::types::{CellRange, SearchQuery, SearchStatus};

/// 最多记录的匹配数。`.` 之类的宽泛正则在 10 万行回滚上会产生上千万个匹配，必须设上限。
pub const MAX_MATCHES: usize = 100_000;
/// 一个逻辑行最多拼接的网格行数；同时保证 `行内序号 * 列数 + 列` 放得进 u32。
const MAX_GROUP_ROWS: usize = 4096;

/// 搜索状态（`Terminal::search` 为 `Some` 即“搜索处于打开状态”）。
pub(crate) struct SearchState {
    regex: Option<Regex>,
    invalid: bool,
    /// 按位置排序、互不重叠的匹配。
    matches: Vec<CellRange>,
    current: Option<usize>,
    truncated: bool,
    /// 上次更新时屏幕第 0 行的绝对行号：它之前的回滚行是“已定型”的。
    top_abs: u64,
    /// 上次更新时的 `Core::epoch`、是否全屏、列数；任何一个变了就全量重搜。
    epoch: u64,
    alt: bool,
    cols: usize,
}

impl SearchState {
    fn status(&self) -> SearchStatus {
        SearchStatus {
            total: self.matches.len() as u32,
            current: self.current.map(|i| i as u32 + 1),
            truncated: self.truncated,
            invalid_regex: self.invalid,
        }
    }
}

fn build_regex(q: &SearchQuery) -> Result<Option<Regex>, regex::Error> {
    if q.text.is_empty() {
        return Ok(None);
    }
    let pattern = if q.regex {
        q.text.clone()
    } else {
        regex::escape(&q.text)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!q.case_sensitive)
        // 用户输入的正则可能很大；编译后的程序超过上限就当作无效，而不是吃掉内存。
        .size_limit(8 << 20)
        .build()
        .map(Some)
}

impl Core {
    /// 在 `from_abs`（必须是逻辑行的第一行）起到屏幕末尾的所有行里搜索，匹配追加到 `out`。
    /// `out` 总长达到 `MAX_MATCHES` 时停止并返回 `true`（被截断）。
    fn search_from(&self, re: &Regex, from_abs: u64, out: &mut Vec<CellRange>) -> bool {
        let grid = self.term.grid();
        let hist = grid.history_size() as i64;
        let top = self.table.screen_top_abs() as i64;
        let cols = self.cols;
        let last_col = Column(cols - 1);
        let last_line = self.rows as i64 - 1;
        // 内容文本与“字节 → 单元格线性序号”的对照表，跨逻辑行复用以免反复分配。
        let mut text = String::new();
        let mut pos: Vec<u32> = Vec::new();
        let mut line = (from_abs as i64 - top).max(-hist);
        while line <= last_line {
            let group_start = line;
            text.clear();
            pos.clear();
            // 逻辑行 = 连续的、除最后一行外都带 WRAPLINE 的网格行。
            let mut k = 0usize;
            loop {
                #[cfg(test)]
                self.search_rows.set(self.search_rows.get() + 1);
                let row = &grid[Line(line as i32)];
                let wrapped = row[last_col].flags.contains(Flags::WRAPLINE);
                let last_of_group = !wrapped || line == last_line || k + 1 >= MAX_GROUP_ROWS;
                // 折行处的行尾空格是内容，必须整行取；只有逻辑行的最后一行才能去掉尾部空白。
                let n = if last_of_group {
                    row.line_length().0
                } else {
                    cols
                };
                for c in 0..n {
                    let cell = &row[Column(c)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let idx = (k * cols + c) as u32;
                    let before = text.len();
                    text.push(if cell.c == '\0' { ' ' } else { cell.c });
                    // 组合字符跟在基字符后面，和它共用同一个单元格序号。
                    for z in cell.zerowidth().into_iter().flatten() {
                        text.push(*z);
                    }
                    pos.resize(pos.len() + (text.len() - before), idx);
                }
                k += 1;
                line += 1;
                if last_of_group {
                    break;
                }
            }
            if text.is_empty() {
                continue;
            }
            for m in re.find_iter(&text) {
                // 空匹配（如 `x*`）没有可高亮的内容，忽略。
                if m.is_empty() {
                    continue;
                }
                if out.len() >= MAX_MATCHES {
                    return true;
                }
                let s = pos[m.start()] as usize;
                let mut e = pos[m.end() - 1] as usize;
                let (er, ec) = (e / cols, e % cols);
                // 匹配以宽字符结尾时，选中范围要包含它的后半格。
                if ec + 1 < cols
                    && grid[Line((group_start + er as i64) as i32)][Column(ec)]
                        .flags
                        .contains(Flags::WIDE_CHAR)
                {
                    e += 1;
                }
                let point = |i: usize| CellPoint {
                    line: AbsLine((top + group_start + (i / cols) as i64) as u64),
                    col: (i % cols) as u16,
                };
                out.push(CellRange {
                    start: point(s),
                    end: point(e),
                });
            }
        }
        false
    }

    /// 逻辑行起点：从屏幕/回滚里的绝对行 `abs` 向上找到它所在逻辑行的第一行，返回其绝对行号。
    fn logical_line_start(&self, abs: u64) -> u64 {
        let grid = self.term.grid();
        let hist = grid.history_size() as i64;
        let top = self.table.screen_top_abs() as i64;
        let last_col = Column(self.cols - 1);
        let mut line = (abs as i64 - top).clamp(-hist, self.rows as i64 - 1);
        let mut steps = 0;
        while line > -hist
            && steps < MAX_GROUP_ROWS
            && grid[Line((line - 1) as i32)][last_col]
                .flags
                .contains(Flags::WRAPLINE)
        {
            line -= 1;
            steps += 1;
        }
        (top + line) as u64
    }
}

impl Terminal {
    /// 开始或更新搜索，返回状态。
    ///
    /// 增量搜索（terminal.md §11）：查询文本改变时**保持当前匹配项**——当前匹配项在新查询下
    /// 仍然匹配（同一起点）就不动，否则移到它之后最近的一个，不会跳到下一个。
    /// 第一次搜索时选视口顶部之后的第一个匹配（没有则选最后一个）。当前匹配项变化时会滚动视口
    /// 使它可见；保持不变时不动视口。
    ///
    /// 范围是全部回滚加屏幕。`regex` 为真但表达式无效时没有匹配，状态里 `invalid_regex` 为真。
    /// 之后每次 [`Terminal::feed`] 都会自动更新匹配，直到 [`Terminal::search_close`]。
    pub fn search(&mut self, query: &SearchQuery) -> SearchStatus {
        let prev_key = self
            .search
            .as_ref()
            .and_then(|s| s.current.map(|i| s.matches[i].start));
        let (regex, invalid) = match build_regex(query) {
            Ok(r) => (r, false),
            Err(_) => (None, true),
        };
        let mut st = SearchState {
            regex,
            invalid,
            matches: Vec::new(),
            current: None,
            truncated: false,
            top_abs: self.core.table.screen_top_abs(),
            epoch: self.core.epoch,
            alt: self.core.alt,
            cols: self.core.cols,
        };
        if let Some(re) = &st.regex {
            let first = self.first_abs();
            st.truncated = self.core.search_from(re, first, &mut st.matches);
        }
        st.current = self.pick_current(&st.matches, prev_key);
        let changed = st.current.map(|i| st.matches[i].start) != prev_key;
        let reveal = st.current.map(|i| st.matches[i].start.line.0);
        let status = st.status();
        self.search = Some(st);
        self.dirty = true;
        if changed && let Some(abs) = reveal {
            self.reveal_line(abs);
        }
        status
    }

    /// 跳到下一个（`backwards` 为真则上一个）匹配项，在匹配项之间循环，并滚动视口使它可见。
    /// 搜索没有打开或没有匹配时只返回当前状态。
    pub fn search_next(&mut self, backwards: bool) -> SearchStatus {
        let Some(st) = self.search.as_mut() else {
            return SearchStatus::default();
        };
        let n = st.matches.len();
        if n == 0 {
            return st.status();
        }
        let next = match (st.current, backwards) {
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
            (None, false) => 0,
            (None, true) => n - 1,
        };
        st.current = Some(next);
        let abs = st.matches[next].start.line.0;
        let status = st.status();
        self.dirty = true;
        self.reveal_line(abs);
        status
    }

    /// 关闭搜索：清除全部高亮，并且之后的输出不再触发重新搜索。
    pub fn search_close(&mut self) {
        if self.search.take().is_some() {
            self.dirty = true;
        }
    }

    /// 当前搜索状态；搜索没有打开时全为默认值。
    pub fn search_status(&self) -> SearchStatus {
        self.search
            .as_ref()
            .map_or_else(SearchStatus::default, SearchState::status)
    }

    /// 可回溯到的最老一行的绝对行号。
    fn first_abs(&self) -> u64 {
        self.core.table.screen_top_abs() - self.core.term.grid().history_size() as u64
    }

    /// 选当前匹配项：优先保持 `prev`（同一起点，否则它之后最近的一个，越过末尾则回到最后一个）；
    /// 没有 `prev` 时选视口顶部之后的第一个，没有则最后一个。
    fn pick_current(&self, matches: &[CellRange], prev: Option<CellPoint>) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }
        let key = prev.unwrap_or_else(|| {
            let grid = self.core.term.grid();
            let first_visible = self.core.table.screen_top_abs() - grid.display_offset() as u64;
            CellPoint {
                line: AbsLine(first_visible),
                col: 0,
            }
        });
        let i = matches.partition_point(|m| m.start < key);
        Some(i.min(matches.len() - 1))
    }

    /// 让绝对行 `abs` 出现在视口里：已经可见就不动，否则先展开所在折叠块再尽量放在视口中间。
    fn reveal_line(&mut self, abs: u64) {
        if self.core.alt {
            return;
        }
        if self.core.unfold_line(abs) {
            self.dirty = true;
        }
        if self.core.has_folded() {
            let (viewport, off, max_off) = self.core.viewport_map();
            if viewport.contains(&Some(abs)) {
                return;
            }
            let hidden = self.core.hidden_ranges();
            let idx = self.core.visible_index_of(abs, &hidden);
            let half = self.core.rows / 2;
            let desired_top = idx.saturating_sub(half);
            let target = max_off.saturating_sub(desired_top);
            if self
                .core
                .scroll_display(Scroll::Delta(target as i32 - off as i32))
            {
                self.dirty = true;
            }
            return;
        }
        let grid = self.core.term.grid();
        let off = grid.display_offset() as i64;
        let hist = grid.history_size() as i64;
        let rows = self.core.rows as i64;
        let g = abs as i64 - self.core.table.screen_top_abs() as i64;
        if g >= -off && g < rows - off {
            return;
        }
        let target = if g >= 0 {
            0
        } else {
            (rows / 2 - g).clamp(0, hist)
        };
        if self
            .core
            .scroll_display(Scroll::Delta((target - off) as i32))
        {
            self.dirty = true;
        }
    }

    /// 内容变化后（`feed`、`resize` 等）更新匹配；搜索没打开时什么也不做。
    pub(crate) fn refresh_search(&mut self) {
        let Some(mut st) = self.search.take() else {
            return;
        };
        let core = &self.core;
        let top = core.table.screen_top_abs();
        let full = st.epoch != core.epoch || st.alt != core.alt || st.cols != core.cols;
        let prev_key = st.current.map(|i| st.matches[i].start);
        if let Some(re) = &st.regex {
            if full {
                st.matches.clear();
                st.truncated = core.search_from(re, self.first_abs(), &mut st.matches);
            } else {
                // 已定型的部分：上次屏幕顶行所在逻辑行之前的行。被回滚上限挤掉的最老的行一并丢弃。
                let mut boundary = core.logical_line_start(st.top_abs.min(top));
                if st.truncated
                    && let Some(last) = st.matches.last()
                {
                    boundary = boundary.min(core.logical_line_start(last.start.line.0));
                }
                let first = self.first_abs();
                let lo = st.matches.partition_point(|m| m.start.line.0 < first);
                let hi = st.matches.partition_point(|m| m.start.line.0 < boundary);
                st.matches.truncate(hi);
                let lo = lo.min(st.matches.len());
                st.matches.drain(..lo);
                st.truncated = core.search_from(re, boundary, &mut st.matches);
            }
        }
        st.top_abs = top;
        st.epoch = core.epoch;
        st.alt = core.alt;
        st.cols = core.cols;
        st.current = self.pick_current(&st.matches, prev_key);
        self.search = Some(st);
    }

    /// 当前视口内的匹配（供帧快照标 `MATCH`）以及当前匹配项（标 `MATCH_CURRENT`）。
    pub(crate) fn visible_matches(&self) -> (Vec<CellRange>, Option<CellRange>) {
        let Some(st) = &self.search else {
            return (Vec::new(), None);
        };
        let grid = self.core.term.grid();
        let first = self.core.table.screen_top_abs() - grid.display_offset() as u64;
        let last = first + self.core.rows as u64 - 1;
        // 匹配互不重叠，起点与终点都单调，可以二分。
        let lo = st.matches.partition_point(|m| m.end.line.0 < first);
        let visible = st.matches[lo..]
            .iter()
            .take_while(|m| m.start.line.0 <= last)
            .copied()
            .collect();
        (visible, st.current.map(|i| st.matches[i]))
    }
}
