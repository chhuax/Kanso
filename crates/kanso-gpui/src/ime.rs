//! 输入法（IME）预编辑状态与预编辑文字的网格布局。
//!
//! 终端没有“文档”，所以交给 GPUI 的文本模型很简单：**文档就是当前的预编辑串**
//! （没有组字时是空串）。GPUI 的 Windows 后端会按 IME 消息依次调用：
//!
//! | IME 消息 | 输入处理器调用 | 本模块 |
//! |---|---|---|
//! | `WM_IME_COMPOSITION` + `GCS_COMPSTR` | `replace_and_mark_text_in_range` | [`ImeState::replace_and_mark`] |
//! | `WM_IME_COMPOSITION` + `GCS_RESULTSTR` / `WM_CHAR` | `replace_text_in_range` | [`ImeState::commit`] |
//! | `WM_IME_COMPOSITION` 且 lparam 为 0（取消） | `replace_text_in_range(.., "")` | [`ImeState::commit`] 返回 `None` |
//!
//! 所有偏移都是 UTF-16 码元（GPUI / Windows 的约定）。

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 预编辑状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImeState {
    marked: Option<String>,
    /// 预编辑串内的光标位置（UTF-16 码元）。
    caret: usize,
    /// 最近一次组字结束（上屏 / 取消 / 清空）的时刻，用来识别“结束组字的那个按键”。
    ended_at: Option<Instant>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 偏移换成 UTF-8 字节偏移；落在代理对中间时向前取整。
fn byte_offset(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units + c.len_utf16() > utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

impl ImeState {
    /// 是否正在组字。
    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    /// 当前预编辑串。
    pub fn marked_text(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    /// 预编辑串在“文档”里的范围（UTF-16）。
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..utf16_len(m))
    }

    /// 当前选区（UTF-16）：没有组字时是空范围 0..0，组字时是预编辑串内的光标。
    pub fn selected_range(&self) -> Range<usize> {
        self.caret..self.caret
    }

    /// 取“文档”的一段文字，并返回实际取到的范围（越界部分被截掉）。
    pub fn text_for_range(&self, range: Range<usize>) -> (String, Range<usize>) {
        let text = self.marked.as_deref().unwrap_or("");
        let len = utf16_len(text);
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let (b0, b1) = (byte_offset(text, start), byte_offset(text, end));
        (text[b0..b1].to_owned(), start..end)
    }

    /// 更新预编辑串。`caret` 是输入法给出的光标位置（相对**新串**，UTF-16），缺省放到末尾。
    /// 空串等于结束组字。
    pub fn replace_and_mark(&mut self, text: &str, caret: Option<Range<usize>>) {
        if text.is_empty() {
            self.end();
            return;
        }
        let len = utf16_len(text);
        self.caret = caret.map_or(len, |r| r.start.min(len));
        self.marked = Some(text.to_owned());
    }

    /// 上屏。结束组字，返回需要发给会话的文字；上屏空串（输入法取消组字）返回 `None`。
    pub fn commit(&mut self, text: &str) -> Option<String> {
        self.end();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// 丢弃组字（窗口失焦等）。**不**上屏：避免输入法随后又上屏一次造成重复（checklist 场景 5）。
    pub fn cancel(&mut self) {
        self.end();
    }

    fn end(&mut self) {
        if self.marked.take().is_some() {
            self.ended_at = Some(Instant::now());
        }
        self.caret = 0;
    }

    /// 组字是否刚刚结束（`within` 以内）。
    ///
    /// 用途：Windows 输入法处理 Esc 取消组字时，会**先**发出“组字清空”，**然后**才把这个 Esc 的
    /// `KeyDown` 交给应用；此时 `marked_text_range` 已经是空，应用无法再靠“正在组字”识别，
    /// 结果 Esc 漏给 shell（实测）。按键处理里用本函数把“紧跟在组字结束之后的无修饰按键”吞掉。
    pub fn ended_within(&self, within: Duration) -> bool {
        self.ended_at.is_some_and(|t| t.elapsed() <= within)
    }

    /// 预编辑串的显示宽度（列数）。
    pub fn total_cells(&self) -> usize {
        self.marked.as_deref().map_or(0, UnicodeWidthStr::width)
    }

    /// 预编辑串里光标之前文字的显示宽度（列数）。
    pub fn caret_cells(&self) -> usize {
        self.cells_before(self.caret)
    }

    /// 预编辑串里某个 UTF-16 偏移之前文字的显示宽度（列数）。
    pub fn cells_before(&self, utf16: usize) -> usize {
        match self.marked.as_deref() {
            Some(m) => m[..byte_offset(m, utf16)].width(),
            None => 0,
        }
    }
}

/// 预编辑里的一个绘制单元：一个字符（或字符加上它后面的组合符号）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreeditCell {
    /// 起始列。
    pub col: u16,
    /// 文字。
    pub text: String,
    /// 占几列（1 或 2）。
    pub cells: u16,
}

/// 预编辑文字在网格里的布局。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreeditLayout {
    /// 起始列。
    pub start_col: u16,
    /// 逐字符的绘制单元。
    pub cells: Vec<PreeditCell>,
    /// 总列数。
    pub total_cells: u16,
    /// 输入法光标所在列（绝对列，可能等于 `cols`，使用方自行夹取）。
    pub caret_col: u16,
}

/// 把预编辑串排到终端光标处：
/// - 默认从光标列开始；
/// - 放不下时整体左移，贴住右边缘（checklist 场景 7.5）；
/// - 比整行还宽时只保留末尾能放下的部分。
pub fn layout_preedit(text: &str, caret_utf16: usize, cursor_col: u16, cols: u16) -> PreeditLayout {
    // 逐字符分解：零宽字符并入前一个单元。
    let mut items: Vec<(usize, String, u16)> = Vec::new(); // (UTF-16 起点, 文本, 宽度)
    let mut units = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        match (w, items.last_mut()) {
            (0, Some(last)) => last.1.push(ch),
            (0, None) => {}
            _ => items.push((units, ch.to_string(), w.min(2))),
        }
        units += ch.len_utf16();
    }

    let total: u16 = items.iter().map(|i| i.2).sum();
    // 太长：从头部丢字符，直到放得下。
    let mut skip = 0;
    let mut kept = total;
    while kept > cols && skip < items.len() {
        kept -= items[skip].2;
        skip += 1;
    }
    let start_col = cursor_col.min(cols.saturating_sub(kept));

    let mut col = start_col;
    let mut cells = Vec::with_capacity(items.len() - skip);
    let mut caret_col = start_col;
    for (unit_start, t, w) in items.into_iter().skip(skip) {
        if unit_start < caret_utf16 {
            caret_col = col + w;
        }
        cells.push(PreeditCell {
            col,
            text: t,
            cells: w,
        });
        col += w;
    }
    PreeditLayout {
        start_col,
        cells,
        total_cells: kept,
        caret_col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_then_commit_sends_text_once() {
        let mut ime = ImeState::default();
        assert!(!ime.is_composing());
        ime.replace_and_mark("n", None);
        ime.replace_and_mark("ni", None);
        assert_eq!(ime.marked_text(), Some("ni"));
        assert_eq!(ime.marked_range(), Some(0..2));
        assert_eq!(ime.commit("你"), Some("你".to_owned()));
        assert!(!ime.is_composing());
        assert_eq!(ime.marked_range(), None);
        // 之后再来一个空的上屏不会重复发送。
        assert_eq!(ime.commit(""), None);
    }

    #[test]
    fn cancel_via_empty_commit_does_not_send() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("nihao", None);
        assert_eq!(ime.commit(""), None);
        assert!(!ime.is_composing());
    }

    #[test]
    fn empty_mark_ends_composition() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("a", None);
        ime.replace_and_mark("", None);
        assert!(!ime.is_composing());
        assert_eq!(ime.selected_range(), 0..0);
    }

    #[test]
    fn ended_within_tracks_only_real_compositions() {
        let mut ime = ImeState::default();
        assert!(!ime.ended_within(Duration::from_secs(10)));
        ime.commit(""); // 没有组字：不算结束
        assert!(!ime.ended_within(Duration::from_secs(10)));
        ime.replace_and_mark("zhong", None);
        assert!(
            !ime.ended_within(Duration::from_secs(10)),
            "组字中不算已结束"
        );
        ime.replace_and_mark("", None); // Esc 取消
        assert!(ime.ended_within(Duration::from_secs(10)));
    }

    #[test]
    fn cancel_discards_without_text() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("zhongwen", None);
        ime.cancel();
        assert!(!ime.is_composing());
        // 取消后输入法的后续上屏仍然正常发送（由输入法决定，不是我们去重）。
        assert_eq!(ime.commit("中文"), Some("中文".to_owned()));
    }

    #[test]
    fn two_quick_identical_commits_are_both_sent() {
        let mut ime = ImeState::default();
        let mut sent = Vec::new();
        for _ in 0..2 {
            ime.replace_and_mark("wo", None);
            sent.extend(ime.commit("我"));
        }
        assert_eq!(sent, vec!["我", "我"]);
    }

    #[test]
    fn caret_is_relative_to_new_text_and_clamped() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("にほんご", Some(2..2));
        assert_eq!(ime.selected_range(), 2..2);
        assert_eq!(ime.caret_cells(), 4); // 两个全角字符 = 4 列
        ime.replace_and_mark("ab", Some(99..99));
        assert_eq!(ime.selected_range(), 2..2);
        ime.replace_and_mark("ab", None);
        assert_eq!(ime.selected_range(), 2..2);
    }

    #[test]
    fn text_for_range_handles_utf16_and_bounds() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("a😀b", None); // 😀 是代理对：UTF-16 长度 4
        assert_eq!(ime.marked_range(), Some(0..4));
        assert_eq!(ime.text_for_range(0..4), ("a😀b".to_owned(), 0..4));
        assert_eq!(ime.text_for_range(1..3), ("😀".to_owned(), 1..3));
        assert_eq!(ime.text_for_range(3..99), ("b".to_owned(), 3..4));
        assert_eq!(ime.text_for_range(10..20), (String::new(), 4..4));
        let empty = ImeState::default();
        assert_eq!(empty.text_for_range(0..5), (String::new(), 0..0));
    }

    #[test]
    fn display_width_counts_wide_chars() {
        let mut ime = ImeState::default();
        ime.replace_and_mark("ni你", None);
        assert_eq!(ime.total_cells(), 4);
        assert_eq!(ime.cells_before(2), 2);
        assert_eq!(ime.cells_before(3), 4);
        assert_eq!(ImeState::default().total_cells(), 0);
    }

    #[test]
    fn layout_starts_at_cursor() {
        let l = layout_preedit("ni你", 3, 5, 80);
        assert_eq!(l.start_col, 5);
        assert_eq!(l.total_cells, 4);
        assert_eq!(
            l.cells,
            vec![
                PreeditCell {
                    col: 5,
                    text: "n".into(),
                    cells: 1
                },
                PreeditCell {
                    col: 6,
                    text: "i".into(),
                    cells: 1
                },
                PreeditCell {
                    col: 7,
                    text: "你".into(),
                    cells: 2
                },
            ]
        );
        assert_eq!(l.caret_col, 9);
    }

    #[test]
    fn layout_shifts_left_to_fit_right_edge() {
        // 光标在第 78 列，预编辑 6 列：整体左移到第 74 列。
        let l = layout_preedit("nihaoa", 6, 78, 80);
        assert_eq!(l.start_col, 74);
        assert_eq!(l.cells.last().unwrap().col, 79);
        assert_eq!(l.caret_col, 80);
    }

    #[test]
    fn layout_longer_than_row_keeps_tail() {
        let text = "a".repeat(50);
        let l = layout_preedit(&text, 50, 3, 40);
        assert_eq!(l.start_col, 0);
        assert_eq!(l.total_cells, 40);
        assert_eq!(l.cells.len(), 40);
    }

    #[test]
    fn layout_wide_char_dropped_whole_when_truncating() {
        // 3 个全角字符 = 6 列，行宽 5：丢掉首个字符后剩 4 列。
        let l = layout_preedit("你好吗", 3, 0, 5);
        assert_eq!(l.total_cells, 4);
        assert_eq!(l.cells.len(), 2);
        assert_eq!(l.cells[0].text, "好");
    }

    #[test]
    fn layout_combining_mark_joins_previous_cell() {
        let l = layout_preedit("e\u{301}x", 3, 0, 10);
        assert_eq!(l.cells.len(), 2);
        assert_eq!(l.cells[0].text, "e\u{301}");
        assert_eq!(l.cells[1].col, 1);
    }

    #[test]
    fn layout_empty_text() {
        let l = layout_preedit("", 0, 4, 10);
        assert!(l.cells.is_empty());
        assert_eq!((l.start_col, l.total_cells, l.caret_col), (4, 0, 4));
    }
}
