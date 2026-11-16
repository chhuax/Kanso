//! 单行文本编辑状态：缓冲、选区、移动、编辑、预编辑（输入法）、撤销、密码掩码与最大长度。
//!
//! 不依赖任何窗口系统，三个平台都能测试。视图层（`ui/widgets/text_input.rs`）只负责把按键、
//! 鼠标与输入法回调翻译成这里的方法调用，再把状态画出来。
//!
//! 约定：
//! - 文本偏移一律是 **UTF-8 字节偏移**，并且总落在字符边界上；光标左右移动按**字素簇**（emoji、
//!   组合字符作为整体）。只有输入法接口（`*_utf16`）使用 UTF-16 码元偏移，这是 GPUI / Windows 的约定。
//! - 选区由 `anchor`（锚点）与 `head`（光标）表示，`head` 可以在 `anchor` 之前。
//! - 单行：粘贴 / 输入里的换行与制表符变成空格，其他控制字符被丢弃。
//! - 组字期间（[`EditState::is_composing`]）文本里含预编辑串，范围见 [`EditState::marked_range`]；
//!   组字被取消时恢复到组字前的内容与选区。

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use super::undo::{EditKind, Snapshot, UndoStack};
use super::word;

/// 密码模式下显示的掩码字符。
pub const MASK_CHAR: char = '\u{2022}';

/// 展示用文本及其与内容的偏移映射（密码掩码时两者不同）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    /// 要绘制的文本。
    pub text: String,
    /// `(内容偏移, 展示偏移)` 对，按升序，覆盖每个可落光标的边界与末尾；`None` 表示两者相同。
    map: Option<Vec<(usize, usize)>>,
}

impl Display {
    /// 内容偏移 → 展示偏移。
    pub fn to_display(&self, content: usize) -> usize {
        match &self.map {
            None => content.min(self.text.len()),
            Some(m) => {
                let i = m.partition_point(|&(c, _)| c <= content);
                if i == 0 { 0 } else { m[i - 1].1 }
            }
        }
    }

    /// 展示偏移 → 内容偏移（落在边界之间时取前一个边界）。
    pub fn to_content(&self, display: usize) -> usize {
        match &self.map {
            None => display,
            Some(m) => {
                let i = m.partition_point(|&(_, d)| d <= display);
                if i == 0 { 0 } else { m[i - 1].0 }
            }
        }
    }

    /// 展示串是否做了掩码。
    pub fn is_masked(&self) -> bool {
        self.map.is_some()
    }
}

/// 单行文本编辑状态。
#[derive(Clone, Debug, Default)]
pub struct EditState {
    text: String,
    anchor: usize,
    head: usize,
    marked: Option<Range<usize>>,
    compose_base: Option<Snapshot>,
    undo: UndoStack,
    cancelled_at: Option<Instant>,
    /// 最大字符数（按 Unicode 标量值计）；`None` 不限制。
    pub max_len: Option<usize>,
    /// 密码模式：显示掩码、禁止复制 / 剪切、按词移动退化为跳到两端。
    pub masked: bool,
    /// 只读（含禁用）：可选择、可复制，不可修改。
    pub read_only: bool,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 偏移 → UTF-8 字节偏移；落在代理对中间时向前取整，越界取末尾。
fn utf16_to_byte(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units + c.len_utf16() > utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

fn byte_to_utf16(s: &str, byte: usize) -> usize {
    utf16_len(&s[..floor_boundary(s, byte)])
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

impl EditState {
    /// 以初始文本创建，光标在末尾。
    pub fn new(text: &str) -> EditState {
        let mut s = EditState::default();
        s.set_text(text);
        s
    }

    // ---- 读取 ----

    /// 当前全部文本（组字期间含预编辑串）。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 选区（字节范围，起点 ≤ 终点）。
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// 光标位置（选区的活动端）。
    pub fn cursor(&self) -> usize {
        self.head
    }

    /// 是否有非空选区。
    pub fn has_selection(&self) -> bool {
        self.anchor != self.head
    }

    /// 选区是否反向（光标在锚点之前）。
    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// 选中的文本。
    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    /// 预编辑串的字节范围。
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// 是否正在组字。
    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    /// 能否撤销。
    pub fn can_undo(&self) -> bool {
        self.undo.can_undo() && !self.is_composing()
    }

    /// 能否重做。
    pub fn can_redo(&self) -> bool {
        self.undo.can_redo() && !self.is_composing()
    }

    /// 展示用文本与偏移映射。密码模式下，预编辑串保持明文（用户需要看到正在组的字）。
    pub fn display(&self) -> Display {
        if !self.masked {
            return Display {
                text: self.text.clone(),
                map: None,
            };
        }
        let mut text = String::new();
        let mut map = Vec::new();
        for (ci, g) in self.text.grapheme_indices(true) {
            let in_marked = self
                .marked
                .as_ref()
                .is_some_and(|m| ci < m.end && ci + g.len() > m.start);
            if in_marked {
                for (off, c) in g.char_indices() {
                    map.push((ci + off, text.len()));
                    text.push(c);
                }
            } else {
                map.push((ci, text.len()));
                text.push(MASK_CHAR);
            }
        }
        map.push((self.text.len(), text.len()));
        Display {
            text,
            map: Some(map),
        }
    }

    // ---- 整体替换 ----

    /// 整体替换内容（外部设置值）：清空撤销历史与组字，光标放到末尾。
    pub fn set_text(&mut self, text: &str) {
        let range = 0..self.text.len();
        let clean = self.prepare(text, &range, true);
        self.text = clean;
        self.anchor = self.text.len();
        self.head = self.text.len();
        self.marked = None;
        self.compose_base = None;
        self.undo.clear();
    }

    // ---- 移动 ----

    fn clamp_offset(&self, offset: usize) -> usize {
        floor_boundary(&self.text, offset)
    }

    /// 把光标移到 `offset`；`extend` 为真时保留锚点（扩展选区）。
    pub fn set_cursor(&mut self, offset: usize, extend: bool) {
        let o = self.clamp_offset(offset);
        self.head = o;
        if !extend {
            self.anchor = o;
        }
        self.undo.break_group();
    }

    fn prev_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.text.len())
    }

    /// ←：有选区且不扩展时折叠到选区起点，否则左移一个字素簇。
    pub fn move_left(&mut self, extend: bool) {
        if self.is_composing() {
            return;
        }
        if self.has_selection() && !extend {
            let s = self.selection().start;
            self.set_cursor(s, false);
        } else {
            self.set_cursor(self.prev_boundary(self.head), extend);
        }
    }

    /// →：有选区且不扩展时折叠到选区终点，否则右移一个字素簇。
    pub fn move_right(&mut self, extend: bool) {
        if self.is_composing() {
            return;
        }
        if self.has_selection() && !extend {
            let e = self.selection().end;
            self.set_cursor(e, false);
        } else {
            self.set_cursor(self.next_boundary(self.head), extend);
        }
    }

    fn word_left_target(&self) -> usize {
        if self.masked {
            0
        } else {
            word::prev_word_start(&self.text, self.head)
        }
    }

    fn word_right_target(&self) -> usize {
        if self.masked {
            self.text.len()
        } else {
            word::next_word_start(&self.text, self.head)
        }
    }

    /// Ctrl+←：移到上一个词的开头（密码模式移到行首）。
    pub fn move_word_left(&mut self, extend: bool) {
        if !self.is_composing() {
            self.set_cursor(self.word_left_target(), extend);
        }
    }

    /// Ctrl+→：移到下一个词的开头（密码模式移到行尾）。
    pub fn move_word_right(&mut self, extend: bool) {
        if !self.is_composing() {
            self.set_cursor(self.word_right_target(), extend);
        }
    }

    /// Home。
    pub fn move_home(&mut self, extend: bool) {
        if !self.is_composing() {
            self.set_cursor(0, extend);
        }
    }

    /// End。
    pub fn move_end(&mut self, extend: bool) {
        if !self.is_composing() {
            self.set_cursor(self.text.len(), extend);
        }
    }

    /// Ctrl+A。
    pub fn select_all(&mut self) {
        if self.is_composing() {
            return;
        }
        self.anchor = 0;
        self.head = self.text.len();
        self.undo.break_group();
    }

    /// 双击：选中 `offset` 所在的词（密码模式选中全部）。
    pub fn select_word_at(&mut self, offset: usize) {
        if self.is_composing() {
            return;
        }
        if self.masked {
            return self.select_all();
        }
        let r = word::word_range_at(&self.text, self.clamp_offset(offset));
        self.anchor = r.start;
        self.head = r.end;
        self.undo.break_group();
    }

    // ---- 编辑 ----

    /// 过滤 / 截断将要插入的文本：换行与制表符变空格、其他控制字符丢弃、按 `max_len` 截断。
    /// `whole` 为真表示替换整个内容（`range` 之外没有其他文本）。
    fn prepare(&self, text: &str, range: &Range<usize>, whole: bool) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    out.push(' ');
                }
                '\n' | '\t' => out.push(' '),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
        if let Some(max) = self.max_len {
            let existing = if whole {
                0
            } else {
                self.text.chars().count() - self.text[range.clone()].chars().count()
            };
            let room = max.saturating_sub(existing);
            if out.chars().count() > room {
                out = out.chars().take(room).collect();
            }
        }
        out
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            anchor: self.anchor,
            head: self.head,
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.text = s.text;
        self.anchor = self.clamp_offset(s.anchor);
        self.head = self.clamp_offset(s.head);
        self.marked = None;
    }

    /// 用 `new` 替换 `range`，光标放到插入内容之后；记入撤销栈。
    fn edit(&mut self, range: Range<usize>, new: &str, kind: EditKind) -> bool {
        if self.read_only {
            return false;
        }
        let new = self.prepare(new, &range, false);
        if range.is_empty() && new.is_empty() {
            return false;
        }
        self.undo.record(self.snapshot(), kind);
        self.text.replace_range(range.clone(), &new);
        let end = range.start + new.len();
        self.anchor = end;
        self.head = end;
        true
    }

    /// 输入文字（键盘输入）：替换选区。返回内容是否变化。
    pub fn insert(&mut self, text: &str) -> bool {
        if self.is_composing() {
            return false;
        }
        let sel = self.selection();
        let kind = if sel.is_empty() {
            EditKind::Insert
        } else {
            EditKind::Other
        };
        // 在词尾后输入空白时另起一步，使撤销按“词”回退。
        let after_non_space = self.text[..sel.start]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
        if !text.is_empty() && text.chars().all(char::is_whitespace) && after_non_space {
            self.undo.break_group();
        }
        self.edit(sel, text, kind)
    }

    /// 粘贴：替换选区，单独成一个撤销步骤。
    pub fn paste(&mut self, text: &str) -> bool {
        if self.is_composing() {
            return false;
        }
        let sel = self.selection();
        self.edit(sel, text, EditKind::Other)
    }

    /// Backspace：删选区，否则删光标前一个字素簇。
    pub fn backspace(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        if self.has_selection() {
            let sel = self.selection();
            return self.edit(sel, "", EditKind::Other);
        }
        if self.head == 0 {
            return false;
        }
        let start = self.prev_boundary(self.head);
        self.edit(start..self.head, "", EditKind::Backspace)
    }

    /// Delete：删选区，否则删光标后一个字素簇。
    pub fn delete(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        if self.has_selection() {
            let sel = self.selection();
            return self.edit(sel, "", EditKind::Other);
        }
        if self.head >= self.text.len() {
            return false;
        }
        let end = self.next_boundary(self.head);
        self.edit(self.head..end, "", EditKind::Delete)
    }

    /// Ctrl+Backspace：删到上一个词的开头。
    pub fn backspace_word(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        if self.has_selection() {
            return self.backspace();
        }
        let start = self.word_left_target();
        self.edit(start..self.head, "", EditKind::Other)
    }

    /// Ctrl+Delete：删到下一个词的开头。
    pub fn delete_word(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        if self.has_selection() {
            return self.delete();
        }
        let end = self.word_right_target();
        self.edit(self.head..end, "", EditKind::Other)
    }

    // ---- 剪贴板 ----

    /// 复制：返回应写入剪贴板的文字。密码模式、空选区、组字期间返回 `None`。
    pub fn copy_text(&self) -> Option<String> {
        (!self.masked && !self.is_composing() && self.has_selection())
            .then(|| self.selected_text().to_owned())
    }

    /// 剪切：返回应写入剪贴板的文字并删除选区。密码模式、只读、空选区返回 `None`（什么都不做）。
    pub fn cut(&mut self) -> Option<String> {
        if self.read_only {
            return None;
        }
        let t = self.copy_text()?;
        let sel = self.selection();
        self.edit(sel, "", EditKind::Other);
        Some(t)
    }

    // ---- 撤销 / 重做 ----

    /// Ctrl+Z。
    pub fn undo(&mut self) -> bool {
        if self.read_only || self.is_composing() {
            return false;
        }
        match self.undo.undo(self.snapshot()) {
            Some(s) => {
                self.restore(s);
                true
            }
            None => false,
        }
    }

    /// Ctrl+Y / Ctrl+Shift+Z。
    pub fn redo(&mut self) -> bool {
        if self.read_only || self.is_composing() {
            return false;
        }
        match self.undo.redo(self.snapshot()) {
            Some(s) => {
                self.restore(s);
                true
            }
            None => false,
        }
    }

    // ---- 输入法（UTF-16 偏移） ----

    /// 选区（UTF-16）。
    pub fn selected_range_utf16(&self) -> Range<usize> {
        let r = self.selection();
        byte_to_utf16(&self.text, r.start)..byte_to_utf16(&self.text, r.end)
    }

    /// 预编辑范围（UTF-16）。
    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| byte_to_utf16(&self.text, r.start)..byte_to_utf16(&self.text, r.end))
    }

    /// 取一段文字（UTF-16 范围），返回文字与实际取到的范围。
    pub fn text_for_range_utf16(&self, range: Range<usize>) -> (String, Range<usize>) {
        let total = utf16_len(&self.text);
        let start = range.start.min(total);
        let end = range.end.clamp(start, total);
        let (b0, b1) = (
            utf16_to_byte(&self.text, start),
            utf16_to_byte(&self.text, end),
        );
        (self.text[b0..b1].to_owned(), start..end)
    }

    /// UTF-16 范围 → 字节范围。
    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        utf16_to_byte(&self.text, r.start)..utf16_to_byte(&self.text, r.end)
    }

    /// 字节范围 → UTF-16 范围。
    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        byte_to_utf16(&self.text, r.start)..byte_to_utf16(&self.text, r.end)
    }

    fn target_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|r| self.range_from_utf16(&r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection())
    }

    /// `replace_text_in_range`：输入法上屏 / 普通字符输入。
    ///
    /// - 组字期间：非空文字 = 上屏（预编辑串被替换为结果，作为一个独立撤销步骤）；
    ///   空串 = 输入法取消组字，恢复组字前的内容，并记下取消时刻（见 [`Self::take_cancel_escape`]）。
    /// - 非组字：按普通输入处理。
    ///
    /// 返回内容是否变化。
    pub fn ime_replace(&mut self, range_utf16: Option<Range<usize>>, text: &str) -> bool {
        if self.read_only {
            // 只读时仍要结束可能残留的组字状态。
            if self.is_composing() {
                self.cancel_composition();
            }
            return false;
        }
        if self.is_composing() {
            let base = self.compose_base.take();
            let range = self.target_range(range_utf16);
            let cleaned = self.prepare(text, &range, false);
            if cleaned.is_empty() {
                // 取消组字：恢复到组字前。
                if let Some(base) = base {
                    let changed = base.text != self.text;
                    self.restore(base);
                    self.cancelled_at = Some(Instant::now());
                    return changed;
                }
                self.text.replace_range(range.clone(), "");
                self.anchor = range.start;
                self.head = range.start;
                self.marked = None;
                self.cancelled_at = Some(Instant::now());
                return true;
            }
            if let Some(base) = base {
                self.undo.record(base, EditKind::Other);
            }
            self.text.replace_range(range.clone(), &cleaned);
            let end = range.start + cleaned.len();
            self.anchor = end;
            self.head = end;
            self.marked = None;
            return true;
        }
        if !text.is_empty() {
            // 输入法有时先发一个空的组字更新、再发上屏结果：那个“取消”其实是上屏的前奏，不是真取消。
            self.cancelled_at = None;
        }
        let range = self.target_range(range_utf16);
        let kind = if range.is_empty() {
            EditKind::Insert
        } else {
            EditKind::Other
        };
        self.edit(range, text, kind)
    }

    /// `replace_and_mark_text_in_range`：更新预编辑串。`sel_utf16` 是输入法给出的光标 / 选区，
    /// 相对**新预编辑串**的起点；缺省放到预编辑串末尾。空串等于取消组字。返回内容是否变化。
    pub fn ime_mark(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        sel_utf16: Option<Range<usize>>,
    ) -> bool {
        if self.read_only {
            return false;
        }
        if text.is_empty() {
            return if self.is_composing() {
                self.ime_replace(range_utf16, "")
            } else {
                false
            };
        }
        if !self.is_composing() {
            self.compose_base = Some(self.snapshot());
        }
        let range = self.target_range(range_utf16);
        let cleaned = self.prepare(text, &range, false);
        if cleaned.is_empty() {
            // 全被过滤 / 超长：当作没发生，保持现状。
            if !self.is_composing() {
                self.compose_base = None;
            }
            return false;
        }
        self.text.replace_range(range.clone(), &cleaned);
        let marked = range.start..range.start + cleaned.len();
        let (a, h) = match sel_utf16 {
            Some(r) => (
                marked.start + utf16_to_byte(&cleaned, r.start),
                marked.start + utf16_to_byte(&cleaned, r.end),
            ),
            None => (marked.end, marked.end),
        };
        self.anchor = a;
        self.head = h;
        self.marked = Some(marked);
        true
    }

    /// 丢弃组字（`unmark_text`、窗口失焦）：恢复到组字前，**不**上屏，避免输入法随后又上屏一次造成重复。
    pub fn cancel_composition(&mut self) {
        if !self.is_composing() {
            return;
        }
        match self.compose_base.take() {
            Some(base) => self.restore(base),
            None => {
                if let Some(m) = self.marked.take() {
                    self.text.replace_range(m.clone(), "");
                    self.anchor = m.start;
                    self.head = m.start;
                }
            }
        }
        self.marked = None;
        self.cancelled_at = Some(Instant::now());
    }

    /// 这个 Esc 是否就是刚刚取消组字的那个按键；是则返回 `true` 并消耗标记（只吞一次）。
    ///
    /// Windows 输入法处理 Esc 取消组字时，会**先**发出“组字清空”，**然后**才把这个 Esc 的 `KeyDown`
    /// 交给应用；此时已经不在组字了，应用无法再靠“正在组字”识别，Esc 就会漏给输入框的 Esc 处理
    /// （例如关闭对话框）。上屏之后的按键都是正常按键，不能吞，所以只有“取消”才记录。
    pub fn take_cancel_escape(&mut self, within: Duration) -> bool {
        let hit = self.cancelled_at.is_some_and(|t| t.elapsed() <= within);
        self.cancelled_at = None;
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(text: &str) -> EditState {
        EditState::new(text)
    }

    #[test]
    fn typing_and_cursor() {
        let mut s = st("");
        assert!(s.insert("ab"));
        assert!(s.insert("c"));
        assert_eq!(s.text(), "abc");
        assert_eq!(s.cursor(), 3);
        s.move_left(false);
        s.insert("X");
        assert_eq!(s.text(), "abXc");
        assert_eq!(s.cursor(), 3);
    }

    #[test]
    fn backspace_and_delete_by_grapheme() {
        let mut s = st("a👨‍👩‍👧e\u{301}");
        assert!(s.backspace());
        assert_eq!(s.text(), "a👨‍👩‍👧");
        assert!(s.backspace());
        assert_eq!(s.text(), "a");
        s.move_home(false);
        assert!(s.delete());
        assert_eq!(s.text(), "");
        assert!(!s.delete());
        assert!(!s.backspace());
    }

    #[test]
    fn selection_by_shift_arrows() {
        let mut s = st("hello");
        s.move_left(true);
        s.move_left(true);
        assert_eq!(s.selection(), 3..5);
        assert!(s.is_reversed());
        assert_eq!(s.selected_text(), "lo");
        s.move_right(true);
        assert_eq!(s.selection(), 4..5);
        // 无扩展的左移折叠到选区起点。
        s.move_left(false);
        assert_eq!(s.cursor(), 4);
        assert!(!s.has_selection());
    }

    #[test]
    fn collapse_right_goes_to_selection_end() {
        let mut s = st("hello");
        s.move_home(false);
        s.move_right(true);
        s.move_right(true);
        s.move_right(false);
        assert_eq!((s.cursor(), s.has_selection()), (2, false));
    }

    #[test]
    fn typing_replaces_selection() {
        let mut s = st("hello world");
        s.select_all();
        s.insert("x");
        assert_eq!(s.text(), "x");
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn word_movement_and_delete() {
        let mut s = st("foo bar baz");
        s.move_word_left(false);
        assert_eq!(s.cursor(), 8);
        s.move_word_left(true);
        assert_eq!(s.selection(), 4..8);
        s.move_home(false);
        s.move_word_right(false);
        assert_eq!(s.cursor(), 4);
        assert!(s.delete_word());
        assert_eq!(s.text(), "foo baz");
        assert!(s.backspace_word());
        assert_eq!(s.text(), "baz");
    }

    #[test]
    fn home_end_select_all() {
        let mut s = st("abc");
        s.move_home(true);
        assert_eq!(s.selection(), 0..3);
        s.move_end(false);
        s.select_all();
        assert_eq!(s.selection(), 0..3);
        assert!(!s.is_reversed());
    }

    #[test]
    fn double_click_word() {
        let mut s = st("hello world");
        s.select_word_at(8);
        assert_eq!(s.selected_text(), "world");
        s.select_word_at(0);
        assert_eq!(s.selected_text(), "hello");
    }

    #[test]
    fn clipboard_semantics() {
        let mut s = st("hello world");
        s.set_cursor(0, false);
        s.move_word_right(true);
        assert_eq!(s.copy_text().as_deref(), Some("hello "));
        assert_eq!(s.cut().as_deref(), Some("hello "));
        assert_eq!(s.text(), "world");
        s.paste("a\r\nb\tc");
        assert_eq!(s.text(), "a b cworld");
        assert_eq!(s.copy_text(), None); // 空选区
    }

    #[test]
    fn paste_strips_control_chars() {
        let mut s = st("");
        s.paste("x\u{0}\u{7}y\nz");
        assert_eq!(s.text(), "xy z");
    }

    #[test]
    fn max_len_truncates_input_and_paste() {
        let mut s = st("");
        s.max_len = Some(5);
        s.insert("abc");
        s.paste("defgh");
        assert_eq!(s.text(), "abcde");
        assert!(!s.insert("z"));
        // 选中后替换仍受限：给替换让出空间。
        s.set_cursor(1, false);
        s.set_cursor(3, true);
        s.paste("123456");
        assert_eq!(s.text(), "a12de");
        let mut c = st("");
        c.max_len = Some(3);
        c.paste("你好世界");
        assert_eq!(c.text(), "你好世");
        c.set_text("1234567");
        assert_eq!(c.text(), "123");
    }

    #[test]
    fn read_only_blocks_edits_but_allows_copy() {
        let mut s = st("abc");
        s.read_only = true;
        assert!(!s.insert("x"));
        assert!(!s.backspace());
        assert!(!s.paste("x"));
        s.select_all();
        assert_eq!(s.copy_text().as_deref(), Some("abc"));
        assert_eq!(s.cut(), None);
        assert_eq!(s.text(), "abc");
        assert!(!s.ime_replace(None, "z"));
    }

    #[test]
    fn masked_blocks_copy_and_cut_and_hides_text() {
        let mut s = st("secret");
        s.masked = true;
        s.select_all();
        assert_eq!(s.copy_text(), None);
        assert_eq!(s.cut(), None);
        assert_eq!(s.text(), "secret");
        let d = s.display();
        assert!(d.is_masked());
        assert_eq!(d.text.chars().count(), 6);
        assert!(d.text.chars().all(|c| c == MASK_CHAR));
        // 偏移映射往返。
        assert_eq!(d.to_display(3), 3 * MASK_CHAR.len_utf8());
        assert_eq!(d.to_content(3 * MASK_CHAR.len_utf8()), 3);
        assert_eq!(d.to_content(d.text.len()), 6);
    }

    #[test]
    fn masked_word_movement_jumps_to_ends() {
        let mut s = st("ab cd");
        s.masked = true;
        s.move_word_left(false);
        assert_eq!(s.cursor(), 0);
        s.move_word_right(false);
        assert_eq!(s.cursor(), 5);
    }

    #[test]
    fn masked_cjk_and_emoji_one_dot_per_grapheme() {
        let mut s = st("密码👨‍👩‍👧");
        s.masked = true;
        assert_eq!(s.display().text.chars().count(), 3);
        let d = s.display();
        let after_first = "密".len();
        assert_eq!(d.to_display(after_first), MASK_CHAR.len_utf8());
    }

    #[test]
    fn undo_redo_basic_and_grouping() {
        let mut s = st("");
        for c in ["h", "e", "l", "l", "o"] {
            s.insert(c);
        }
        s.insert(" ");
        s.insert("w");
        s.insert("x");
        assert_eq!(s.text(), "hello wx");
        assert!(s.undo());
        assert_eq!(s.text(), "hello");
        assert!(s.undo());
        assert_eq!(s.text(), "");
        assert!(!s.undo());
        assert!(s.redo());
        assert_eq!(s.text(), "hello");
        assert!(s.redo());
        assert_eq!(s.text(), "hello wx");
        assert!(!s.redo());
    }

    #[test]
    fn undo_restores_selection_and_paste_is_one_step() {
        let mut s = st("abc");
        s.select_all();
        s.paste("xyz");
        assert_eq!(s.text(), "xyz");
        assert!(s.undo());
        assert_eq!(s.text(), "abc");
        assert_eq!(s.selection(), 0..3);
    }

    #[test]
    fn cursor_move_breaks_typing_group() {
        let mut s = st("");
        s.insert("a");
        s.insert("b");
        s.move_left(false);
        s.insert("c");
        assert_eq!(s.text(), "acb");
        s.undo();
        assert_eq!(s.text(), "ab");
        s.undo();
        assert_eq!(s.text(), "");
    }

    #[test]
    fn set_text_clears_history() {
        let mut s = st("a");
        s.insert("b");
        s.set_text("zzz");
        assert!(!s.undo());
        assert_eq!(s.cursor(), 3);
    }

    #[test]
    fn utf16_conversions() {
        let s = st("a😀你");
        assert_eq!(s.text_for_range_utf16(0..3), ("a😀".to_owned(), 0..3));
        assert_eq!(s.text_for_range_utf16(1..100).1, 1..4);
        assert_eq!(s.range_to_utf16(&(1..5)), 1..3);
        assert_eq!(s.range_from_utf16(&(1..3)), 1..5);
        // 落在代理对中间向前取整。
        assert_eq!(s.range_from_utf16(&(2..2)), 1..1);
        assert_eq!(s.selected_range_utf16(), 4..4);
    }

    #[test]
    fn ime_compose_then_commit() {
        let mut s = st("ab");
        s.set_cursor(1, false);
        assert!(s.ime_mark(None, "ni", None));
        assert!(s.is_composing());
        assert_eq!(s.text(), "anib");
        assert_eq!(s.marked_range_utf16(), Some(1..3));
        assert!(s.ime_mark(None, "你", Some(1..1)));
        assert_eq!(s.text(), "a你b");
        assert_eq!(s.marked_range(), Some(1..4));
        assert_eq!(s.cursor(), 4);
        assert!(s.ime_replace(None, "你好"));
        assert!(!s.is_composing());
        assert_eq!(s.text(), "a你好b");
        assert_eq!(s.cursor(), 1 + "你好".len());
        // 上屏是一个撤销步骤，回到组字前。
        assert!(s.undo());
        assert_eq!(s.text(), "ab");
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn ime_caret_inside_preedit() {
        let mut s = st("");
        s.ime_mark(None, "nihao", Some(2..2));
        assert_eq!(s.cursor(), 2);
        s.ime_mark(None, "你好", Some(1..1));
        assert_eq!(s.cursor(), "你".len());
    }

    #[test]
    fn ime_cancel_restores_selection_and_arms_escape_guard() {
        let mut s = st("hello");
        s.select_all();
        s.ime_mark(None, "x", None);
        assert_eq!(s.text(), "x");
        assert!(s.ime_replace(None, ""));
        assert_eq!(s.text(), "hello");
        assert_eq!(s.selection(), 0..5);
        assert!(!s.is_composing());
        assert!(s.take_cancel_escape(Duration::from_secs(1)));
        // 只吞一次。
        assert!(!s.take_cancel_escape(Duration::from_secs(1)));
    }

    #[test]
    fn ime_commit_does_not_arm_escape_guard() {
        let mut s = st("");
        s.ime_mark(None, "ni", None);
        s.ime_replace(None, "你");
        assert!(!s.take_cancel_escape(Duration::from_secs(1)));
    }

    #[test]
    fn empty_mark_before_result_is_not_a_real_cancel() {
        // Windows 上屏时可能先 mark(\"\") 再 replace(\"你\")。
        let mut s = st("");
        s.ime_mark(None, "ni", None);
        s.ime_mark(None, "", None);
        s.ime_replace(None, "你");
        assert_eq!(s.text(), "你");
        assert!(!s.take_cancel_escape(Duration::from_secs(1)));
    }

    #[test]
    fn ime_mark_empty_cancels() {
        let mut s = st("a");
        s.ime_mark(None, "ni", None);
        assert!(s.ime_mark(None, "", None));
        assert_eq!(s.text(), "a");
        assert!(s.take_cancel_escape(Duration::from_secs(1)));
    }

    #[test]
    fn cancel_composition_never_commits() {
        let mut s = st("x");
        s.ime_mark(None, "ni", None);
        s.cancel_composition();
        assert_eq!(s.text(), "x");
        assert!(!s.is_composing());
        // 之后输入法再来一次空的上屏也无害。
        assert!(!s.ime_replace(None, ""));
    }

    #[test]
    fn keys_are_inert_while_composing() {
        let mut s = st("ab");
        s.ime_mark(None, "ni", None);
        assert!(!s.backspace());
        assert!(!s.insert("z"));
        assert!(!s.undo());
        s.move_left(false);
        assert_eq!(s.text(), "abni");
    }

    #[test]
    fn ime_respects_max_len() {
        let mut s = st("");
        s.max_len = Some(3);
        s.ime_mark(None, "abcdef", None);
        assert_eq!(s.text(), "abc");
        s.ime_replace(None, "你好世界");
        assert_eq!(s.text(), "你好世");
    }

    #[test]
    fn ime_replace_with_explicit_range_outside_composition() {
        let mut s = st("hello");
        assert!(s.ime_replace(Some(1..3), "EY"));
        assert_eq!(s.text(), "hEYlo");
    }

    #[test]
    fn display_without_mask_is_identity() {
        let s = st("你好");
        let d = s.display();
        assert!(!d.is_masked());
        assert_eq!(d.text, "你好");
        assert_eq!(d.to_display(3), 3);
        assert_eq!(d.to_content(3), 3);
    }

    #[test]
    fn masked_display_keeps_preedit_plain() {
        let mut s = st("ab");
        s.masked = true;
        s.ime_mark(None, "你", None);
        let d = s.display();
        assert_eq!(d.text, format!("{0}{0}你", MASK_CHAR));
        let m = s.marked_range().unwrap();
        assert_eq!(d.to_display(m.end), d.text.len());
    }
}
