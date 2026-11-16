//! 行元数据侧表：按绝对行号保存每行的首次写出时间与提示符标记。
//!
//! 侧表是一个与 alacritty 网格“逐行平行”的环形队列：下标 0 是最老的回滚行，
//! 最后 `screen_rows` 项是当前屏幕（主屏幕）。网格里每发生一次行移动，`Core`（handler.rs）就在
//! 这里做完全相同的移动，所以任何时刻都有 `len == 历史行数 + 屏幕行数`。
//!
//! 绝对行号**不存放在每一项里**，而由 `first_abs + 下标` 推出。丢弃最老的行时从队首弹出并把
//! `first_abs` 加一，所以行号在回滚满了以后仍然单调增长（terminal.md §2 的旧坑：缓冲区满后底部换行
//! 复用顶部的行，行号就停住）。整个结构的所有操作都是摊还 O(1)（屏幕内的局部滚动除外，
//! 它们只搬动屏幕那几十行，与回滚长度无关）。

use std::collections::VecDeque;

/// 该行写过内容（没有这一位就是 `LINE_BLANK`）。
pub(crate) const ROW_WRITTEN: u8 = 1 << 0;
/// OSC 133 A 所在行。
pub(crate) const ROW_PROMPT: u8 = 1 << 1;
/// 会话第一个提示符（`kanso=first`）。
pub(crate) const ROW_FIRST: u8 = 1 << 2;
/// 该行软换行到下一行。**只在 `sync_wrapped` 之后、resize 重排之前有效**，平时不维护：
/// 网格里的 `WRAPLINE` 才是事实来源，这一位只是为了在“主屏幕被全屏程序盖住”时还能知道逻辑行分组。
pub(crate) const ROW_WRAPPED_HINT: u8 = 1 << 3;

/// 逻辑行元数据继承时保留的位（重排后续行只保留 WRITTEN）。
const ROW_CARRY: u8 = ROW_WRITTEN | ROW_PROMPT | ROW_FIRST;

/// 一行的元数据。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowMeta {
    /// 首次写出时的 Unix 毫秒；只在 `ROW_WRITTEN` 置位时有意义。
    pub ts_ms: i64,
    /// `ROW_*` 标志。
    pub flags: u8,
}

/// 被软换行连起来的若干行（一个逻辑行）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Group {
    /// 第一行在表里的下标。
    pub start: usize,
    /// 逻辑行元数据（各行合并结果）。
    pub meta: RowMeta,
}

/// 行元数据侧表。
#[derive(Debug)]
pub(crate) struct LineTable {
    rows: VecDeque<RowMeta>,
    /// `rows[0]` 的绝对行号。
    first_abs: u64,
    /// 屏幕行数（队尾这么多项是屏幕）。
    screen_rows: usize,
}

impl LineTable {
    pub fn new(screen_rows: usize) -> Self {
        let mut rows = VecDeque::new();
        rows.resize(screen_rows, RowMeta::default());
        Self {
            rows,
            first_abs: 0,
            screen_rows,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn screen_rows(&self) -> usize {
        self.screen_rows
    }

    /// 回滚（历史）行数。
    pub fn hist(&self) -> usize {
        self.rows.len().saturating_sub(self.screen_rows)
    }

    pub fn first_abs(&self) -> u64 {
        self.first_abs
    }

    /// 屏幕第 0 行的绝对行号。
    #[inline]
    pub fn screen_top_abs(&self) -> u64 {
        self.first_abs + self.hist() as u64
    }

    pub fn resize_rows(&mut self, rows: usize, cursor: usize, cap: usize) {
        if rows > self.screen_rows {
            let blanks = (rows - self.screen_rows).saturating_sub(self.hist());
            self.rows
                .resize(self.rows.len() + blanks, RowMeta::default());
        } else {
            let scrolling = (cursor + 1).saturating_sub(rows);
            self.scroll_up(0, self.screen_rows, scrolling, cap);
            self.rows
                .truncate(self.rows.len() - (self.screen_rows - rows));
        }
        self.screen_rows = rows;
    }

    pub fn get(&self, abs: u64) -> Option<&RowMeta> {
        let idx = abs.checked_sub(self.first_abs)?;
        self.rows.get(usize::try_from(idx).ok()?)
    }

    pub fn get_mut(&mut self, abs: u64) -> Option<&mut RowMeta> {
        let idx = abs.checked_sub(self.first_abs)?;
        self.rows.get_mut(usize::try_from(idx).ok()?)
    }

    /// 回滚超过上限时从队首丢弃最老的行。丢弃的行数会计入 `first_abs`，行号不回退也不停住。
    pub fn trim_history(&mut self, cap: usize) {
        let hist = self.hist();
        if hist > cap {
            let drop = hist - cap;
            self.rows.drain(..drop);
            self.first_abs += drop as u64;
        }
    }

    fn insert_blank(&mut self, idx: usize, n: usize) {
        for _ in 0..n {
            self.rows.insert(idx, RowMeta::default());
        }
    }

    fn remove_at(&mut self, idx: usize, n: usize) {
        for _ in 0..n {
            self.rows.remove(idx);
        }
    }

    /// 重置屏幕内 `[start, end)` 行（相对屏幕顶）为“从未写过”。
    pub fn reset_screen_rows(&mut self, start: usize, end: usize) {
        let top = self.hist();
        for i in start..end.min(self.screen_rows) {
            self.rows[top + i] = RowMeta::default();
        }
    }

    /// `scroll_up(0, screen_rows, 1, cap)` 的快速路径：整屏上滚一行。每个换行都会走到这里。
    #[inline]
    pub fn push_line(&mut self, cap: usize) {
        self.rows.push_back(RowMeta::default());
        if self.hist() > cap {
            self.rows.pop_front();
            self.first_abs += 1;
        }
    }

    /// 镜像 `Grid::scroll_up(region, n)`：`start == 0` 的区域会把顶部 n 行推进回滚，
    /// 否则只在区域内移动、不进回滚。`n` 必须已经按 alacritty 的方式夹到区域大小以内。
    pub fn scroll_up(&mut self, start: usize, end: usize, n: usize, cap: usize) {
        if n == 0 {
            return;
        }
        let top = self.hist();
        if end - start <= n && start != 0 {
            self.reset_screen_rows(start, end);
            return;
        }
        if start == 0 {
            // 区域顶部 n 行成为回滚（下标不变），区域底部补 n 个新行。
            // 区域下方的固定行（end..screen）仍在表尾，所以新行插在 `top + end`。
            self.insert_blank(top + end, n);
            self.trim_history(cap);
        } else {
            self.remove_at(top + start, n);
            self.insert_blank(top + end - n, n);
        }
    }

    /// 镜像 `Grid::scroll_down(region, n)`。
    pub fn scroll_down(&mut self, start: usize, end: usize, n: usize) {
        if n == 0 {
            return;
        }
        if end - start <= n {
            self.reset_screen_rows(start, end);
            return;
        }
        let top = self.hist();
        self.remove_at(top + end - n, n);
        self.insert_blank(top + start, n);
    }

    /// ED 2（非全屏、无滚动区域）：屏幕前 `positions` 行推进回滚，整个屏幕换成新行。
    /// `positions` 是 alacritty `clear_viewport` 算出的“到最后一个非空行为止”的行数。
    pub fn push_screen(&mut self, positions: usize, cap: usize) {
        let keep = self.screen_rows - positions.min(self.screen_rows);
        let new_len = self.rows.len() - keep;
        self.rows.truncate(new_len);
        for _ in 0..self.screen_rows {
            self.rows.push_back(RowMeta::default());
        }
        self.trim_history(cap);
    }

    /// 丢弃全部回滚行（ED 3）。
    pub fn clear_history(&mut self) {
        let hist = self.hist();
        self.rows.drain(..hist);
        self.first_abs += hist as u64;
    }

    /// 回滚与屏幕全部重置（RIS、⌘K）；`screen_rows` 为重置后的屏幕行数。
    pub fn reset_all(&mut self, screen_rows: usize) {
        // 行号继续累加：已经发出去的绝对行号不能再被新行复用。
        self.first_abs += self.rows.len() as u64;
        self.rows.clear();
        self.screen_rows = screen_rows;
        self.rows.resize(screen_rows, RowMeta::default());
    }

    /// 把屏幕所有行标记为已写（DECALN 会往每个单元格写字符）。
    pub fn mark_screen_written(&mut self, now_ms: i64) {
        let top = self.hist();
        for m in self.rows.iter_mut().skip(top) {
            if m.flags & ROW_WRITTEN == 0 {
                m.flags |= ROW_WRITTEN;
                m.ts_ms = now_ms;
            }
        }
    }

    /// 用网格里的 `WRAPLINE` 刷新每行的 `ROW_WRAPPED_HINT`。`wrapped(i)` 返回表中第 i 行是否软换行。
    pub fn sync_wrapped(&mut self, mut wrapped: impl FnMut(usize) -> bool) {
        for (i, m) in self.rows.iter_mut().enumerate() {
            if wrapped(i) {
                m.flags |= ROW_WRAPPED_HINT;
            } else {
                m.flags &= !ROW_WRAPPED_HINT;
            }
        }
    }

    /// 按 `ROW_WRAPPED_HINT` 把行分成逻辑行（调用前必须先 `sync_wrapped`）。
    pub fn groups(&self) -> Vec<Group> {
        let mut out = Vec::new();
        let mut cur: Option<Group> = None;
        let mut prev_wrapped = false;
        for (i, m) in self.rows.iter().enumerate() {
            match (&mut cur, prev_wrapped) {
                (Some(g), true) => merge_meta(&mut g.meta, m),
                _ => {
                    if let Some(g) = cur.take() {
                        out.push(g);
                    }
                    let mut meta = RowMeta::default();
                    merge_meta(&mut meta, m);
                    cur = Some(Group { start: i, meta });
                }
            }
            prev_wrapped = m.flags & ROW_WRAPPED_HINT != 0;
        }
        out.extend(cur);
        out
    }

    /// resize 重排后按新的逻辑行结构重建整张表。
    ///
    /// - `old`：重排前的逻辑行（`groups()` 的结果）。
    /// - `new_counts`：重排后每个逻辑行占几行（从最老到最新）。
    /// - `map`：新逻辑行 k 对应旧逻辑行 `map[k]`；`None` 是重排中新增的行，元数据为空。
    /// - `new_first_abs` / `screen_rows`：重建后队首的绝对行号与屏幕行数。
    pub fn remap(
        &mut self,
        old: &[Group],
        new_counts: &[usize],
        map: &[Option<usize>],
        has_text: &[bool],
        new_first_abs: u64,
        screen_rows: usize,
    ) {
        self.rows.clear();
        let mut last_ts = 0;
        for (k, &count) in new_counts.iter().enumerate() {
            let mut meta = map
                .get(k)
                .copied()
                .flatten()
                .and_then(|o| old.get(o))
                .map(|g| g.meta);
            // 对不上旧逻辑行、但确实有文字的行（重排时被合并或截断的行）：不能显示成“从未写过”，
            // 时间沿用前一个逻辑行，宁可略早也不留空。
            if meta.is_none() && has_text.get(k).copied().unwrap_or(false) {
                meta = Some(RowMeta {
                    ts_ms: last_ts,
                    flags: ROW_WRITTEN,
                });
            }
            if let Some(m) = meta
                && m.flags & ROW_WRITTEN != 0
            {
                last_ts = m.ts_ms;
            }
            for r in 0..count {
                self.rows.push_back(match meta {
                    Some(m) if r == 0 => RowMeta {
                        ts_ms: m.ts_ms,
                        flags: m.flags & ROW_CARRY,
                    },
                    // 续行：时间沿用整个逻辑行，但提示符标记只属于第一行。
                    Some(m) => RowMeta {
                        ts_ms: m.ts_ms,
                        flags: m.flags & ROW_WRITTEN,
                    },
                    None => RowMeta::default(),
                });
            }
        }
        self.first_abs = new_first_abs;
        self.screen_rows = screen_rows;
    }

    /// 与网格行数对不上时的兜底修复：从队首丢、从队尾补。返回是否做了修复。
    pub fn heal(&mut self, total_lines: usize) -> bool {
        if self.rows.len() == total_lines {
            return false;
        }
        if self.rows.len() > total_lines {
            let drop = self.rows.len() - total_lines;
            self.rows.drain(..drop);
            self.first_abs += drop as u64;
        } else {
            self.rows.resize(total_lines, RowMeta::default());
        }
        true
    }
}

/// 把一行并入逻辑行元数据：标志取并集，时间取第一个写过的行。
fn merge_meta(into: &mut RowMeta, row: &RowMeta) {
    if into.flags & ROW_WRITTEN == 0 && row.flags & ROW_WRITTEN != 0 {
        into.ts_ms = row.ts_ms;
    }
    into.flags |= row.flags & ROW_CARRY;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_keeps_growing_when_history_full() {
        let mut t = LineTable::new(3);
        for _ in 0..100 {
            t.scroll_up(0, 3, 1, 5);
            assert!(t.len() <= 8);
        }
        assert_eq!(t.hist(), 5);
        assert_eq!(t.first_abs(), 95);
        assert_eq!(t.screen_top_abs(), 100);
    }

    #[test]
    fn region_scroll_does_not_touch_history() {
        let mut t = LineTable::new(4);
        for i in 0..4 {
            t.rows[i] = RowMeta {
                ts_ms: i as i64 + 1,
                flags: ROW_WRITTEN,
            };
        }
        t.scroll_up(1, 3, 1, 10);
        let ts: Vec<i64> = t.rows.iter().map(|m| m.ts_ms).collect();
        assert_eq!(ts, vec![1, 3, 0, 4]);
        t.scroll_down(1, 3, 1);
        let ts: Vec<i64> = t.rows.iter().map(|m| m.ts_ms).collect();
        assert_eq!(ts, vec![1, 0, 3, 4]);
    }

    #[test]
    fn fixed_rows_below_region_stay_at_bottom() {
        let mut t = LineTable::new(4);
        for i in 0..4 {
            t.rows[i] = RowMeta {
                ts_ms: i as i64 + 1,
                flags: ROW_WRITTEN,
            };
        }
        // 区域 0..3，第 3 行固定；顶部一行进回滚。
        t.scroll_up(0, 3, 1, 10);
        let ts: Vec<i64> = t.rows.iter().map(|m| m.ts_ms).collect();
        assert_eq!(ts, vec![1, 2, 3, 0, 4]);
        assert_eq!(t.hist(), 1);
    }
}
