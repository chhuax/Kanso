//! 下拉选择框的**纯逻辑**：选项模型、过滤、键盘导航、键入跳转与浮层定位。
//!
//! 约定：
//! - 对外的“值”是**选项序号**（只数 `Option`，不数分组标题与分隔线），与 mac 版 `KPopUpField` 的 `selectedIndex` 一致；
//! - 浮层里的行都是等高的（方便 `uniform_list` 虚拟化）：分隔线不占行，而是画在**下一行**的顶部；
//! - 过滤非空时只显示匹配的选项（标题与分隔线隐藏）。
//!
//! GPUI 视图在 `ui/widgets/dropdown.rs`（只在 Windows 编译）。

/// 浮层里一行的高度（pt）。
pub const ROW_HEIGHT: f32 = 26.0;
/// 浮层内边距（pt）。
pub const MENU_PADDING: f32 = 4.0;
/// 过滤输入框所在区域的高度（含上下间距，pt）。
pub const FILTER_AREA_HEIGHT: f32 = 30.0 + 6.0 + 4.0;
/// 浮层最大总高度（pt）。
pub const MAX_MENU_HEIGHT: f32 = 320.0;
/// 浮层与按钮之间的间隙（pt）。
pub const GAP: f32 = 4.0;
/// 浮层离窗口边缘的最小留白（pt）。
pub const WINDOW_MARGIN: f32 = 8.0;
/// 键入跳转的缓冲有效期（毫秒）。
pub const TYPE_AHEAD_MS: u64 = 1000;
/// 浮层宽度上限（pt）。
pub const MAX_MENU_WIDTH: f32 = 480.0;

/// 构造下拉框时给出的条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// 可选项。
    Option(String),
    /// 分组标题（不可选）。
    Header(String),
    /// 分隔线。
    Separator,
}

impl Item {
    /// 可选项的便捷构造。
    pub fn option(label: impl Into<String>) -> Item {
        Item::Option(label.into())
    }

    /// 分组标题的便捷构造。
    pub fn header(label: impl Into<String>) -> Item {
        Item::Header(label.into())
    }
}

/// 浮层里的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// 可选项（携带选项序号）。
    Option(usize),
    /// 分组标题（携带条目序号）。
    Header(usize),
}

/// 浮层里一行的描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// 行类型。
    pub kind: RowKind,
    /// 这一行顶部是否画分隔线。
    pub separator_before: bool,
}

/// 下拉框状态（不含任何 UI 对象）。
#[derive(Clone, Debug, Default)]
pub struct DropdownState {
    items: Vec<Item>,
    /// 选项序号 → 条目序号。
    option_items: Vec<usize>,
    selected: Option<usize>,
    open: bool,
    filter: String,
    rows: Vec<Row>,
    /// 高亮行（`rows` 的下标，一定是 Option 行）。
    highlight: Option<usize>,
    type_buf: String,
    type_at_ms: u64,
}

/// 大小写不敏感的子串匹配（空查询匹配一切）。
pub fn matches_filter(label: &str, query: &str) -> bool {
    query.is_empty() || label.to_lowercase().contains(&query.to_lowercase())
}

impl DropdownState {
    /// 以条目列表创建，初始无选中值。
    pub fn new(items: Vec<Item>) -> DropdownState {
        let mut s = DropdownState::default();
        s.set_items(items);
        s
    }

    /// 替换条目；选中序号超出范围时清空，展开状态收起。
    pub fn set_items(&mut self, items: Vec<Item>) {
        self.option_items = items
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, Item::Option(_)))
            .map(|(i, _)| i)
            .collect();
        self.items = items;
        if self.selected.is_some_and(|s| s >= self.option_items.len()) {
            self.selected = None;
        }
        self.open = false;
        self.filter.clear();
        self.rebuild_rows();
        self.highlight = None;
    }

    /// 选项总数。
    pub fn option_count(&self) -> usize {
        self.option_items.len()
    }

    /// 第 `opt` 个选项的文字。
    pub fn option_label(&self, opt: usize) -> Option<&str> {
        match self.items.get(*self.option_items.get(opt)?)? {
            Item::Option(s) => Some(s),
            _ => None,
        }
    }

    /// 当前选中的选项序号。
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// 当前选中项的文字。
    pub fn selected_label(&self) -> Option<&str> {
        self.option_label(self.selected?)
    }

    /// 程序设置选中项（不触发任何通知；越界视为清空）。
    pub fn set_selected(&mut self, opt: Option<usize>) {
        self.selected = opt.filter(|&i| i < self.option_items.len());
    }

    /// 是否展开。
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 当前过滤词。
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// 浮层里当前显示的行。
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// 某一行的文字。
    pub fn row_label(&self, row: usize) -> &str {
        match self.rows.get(row).map(|r| &r.kind) {
            Some(RowKind::Option(o)) => self.option_label(*o).unwrap_or(""),
            Some(RowKind::Header(i)) => match &self.items[*i] {
                Item::Header(s) => s,
                _ => "",
            },
            None => "",
        }
    }

    /// 高亮行（`rows` 下标）。
    pub fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    /// 高亮行对应的选项序号。
    pub fn highlighted_option(&self) -> Option<usize> {
        match &self.rows.get(self.highlight?)?.kind {
            RowKind::Option(o) => Some(*o),
            _ => None,
        }
    }

    /// 选中项在当前行列表里的行号（被过滤掉则为 `None`）。
    pub fn selected_row(&self) -> Option<usize> {
        let sel = self.selected?;
        self.rows
            .iter()
            .position(|r| r.kind == RowKind::Option(sel))
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let filtering = !self.filter.is_empty();
        let mut sep = false;
        for (i, it) in self.items.iter().enumerate() {
            match it {
                Item::Separator => sep = !filtering && !self.rows.is_empty(),
                Item::Header(_) if filtering => {}
                Item::Header(_) => {
                    self.rows.push(Row {
                        kind: RowKind::Header(i),
                        separator_before: sep,
                    });
                    sep = false;
                }
                Item::Option(label) => {
                    if matches_filter(label, &self.filter) {
                        let opt = self.option_items.partition_point(|&x| x < i);
                        self.rows.push(Row {
                            kind: RowKind::Option(opt),
                            separator_before: sep,
                        });
                        sep = false;
                    }
                }
            }
        }
    }

    fn is_option_row(&self, row: usize) -> bool {
        matches!(
            self.rows.get(row).map(|r| &r.kind),
            Some(RowKind::Option(_))
        )
    }

    fn first_option_row(&self) -> Option<usize> {
        (0..self.rows.len()).find(|&r| self.is_option_row(r))
    }

    fn last_option_row(&self) -> Option<usize> {
        (0..self.rows.len()).rev().find(|&r| self.is_option_row(r))
    }

    // ---- 展开 / 收起 ----

    /// 展开：清空过滤与键入缓冲，高亮选中项（没有则第一项）。返回应滚到的行。
    pub fn open(&mut self) -> Option<usize> {
        self.open = true;
        self.filter.clear();
        self.type_buf.clear();
        self.rebuild_rows();
        self.highlight = self.selected_row().or_else(|| self.first_option_row());
        self.highlight
    }

    /// 收起（不改值）。
    pub fn close(&mut self) {
        self.open = false;
        self.type_buf.clear();
    }

    /// 设置过滤词并重算行；高亮优先留在选中项，否则第一个匹配。返回应滚到的行。
    pub fn set_filter(&mut self, query: &str) -> Option<usize> {
        if self.filter == query {
            return self.highlight;
        }
        self.filter = query.to_owned();
        self.rebuild_rows();
        self.highlight = if self.filter.is_empty() {
            self.selected_row()
        } else {
            None
        }
        .or_else(|| self.first_option_row());
        self.highlight
    }

    // ---- 浮层内导航（都返回新的高亮行，用来滚动）----

    /// 把高亮设为某一行（鼠标悬停）。不是选项行时忽略。返回是否变化。
    pub fn hover(&mut self, row: usize) -> bool {
        if self.is_option_row(row) && self.highlight != Some(row) {
            self.highlight = Some(row);
            true
        } else {
            false
        }
    }

    /// 上下移动 `delta` 个选项行（越过标题，不循环，到头停住）。
    pub fn move_highlight(&mut self, delta: i32) -> Option<usize> {
        let Some(cur) = self.highlight else {
            self.highlight = if delta >= 0 {
                self.first_option_row()
            } else {
                self.last_option_row()
            };
            return self.highlight;
        };
        let mut row = cur;
        let mut left = delta.unsigned_abs();
        while left > 0 {
            let next = if delta > 0 {
                (row + 1..self.rows.len()).find(|&r| self.is_option_row(r))
            } else {
                (0..row).rev().find(|&r| self.is_option_row(r))
            };
            match next {
                Some(r) => row = r,
                None => break,
            }
            left -= 1;
        }
        self.highlight = Some(row);
        self.highlight
    }

    /// 跳到第一个选项。
    pub fn highlight_first(&mut self) -> Option<usize> {
        self.highlight = self.first_option_row().or(self.highlight);
        self.highlight
    }

    /// 跳到最后一个选项。
    pub fn highlight_last(&mut self) -> Option<usize> {
        self.highlight = self.last_option_row().or(self.highlight);
        self.highlight
    }

    /// 翻页：移动 `page_rows - 1` 行（至少 1）。
    pub fn page(&mut self, down: bool, page_rows: usize) -> Option<usize> {
        let n = page_rows.saturating_sub(1).max(1) as i32;
        self.move_highlight(if down { n } else { -n })
    }

    /// 确认高亮项：写入选中值，收起，返回新值（高亮项已是当前值时仍返回，调用方决定要不要通知）。
    pub fn accept(&mut self) -> Option<usize> {
        let opt = self.highlighted_option()?;
        self.selected = Some(opt);
        self.close();
        Some(opt)
    }

    /// 收起态下上下键直接切换值（不循环）。返回新值，未变化返回 `None`。
    pub fn step_selected(&mut self, delta: i32) -> Option<usize> {
        let n = self.option_items.len();
        if n == 0 {
            return None;
        }
        let new = match self.selected {
            None => {
                if delta >= 0 {
                    0
                } else {
                    n - 1
                }
            }
            Some(cur) => (cur as i64 + delta as i64).clamp(0, n as i64 - 1) as usize,
        };
        if self.selected == Some(new) {
            return None;
        }
        self.selected = Some(new);
        Some(new)
    }

    // ---- 键入跳转（非过滤模式）----

    /// 键入一个字符：在 `TYPE_AHEAD_MS` 内的连续输入拼成前缀；同一个字符重复按则在以它开头的项之间轮换。
    /// 展开时移动高亮，返回新高亮行；收起时直接改选中值并通过 `changed` 返回。
    pub fn type_ahead(&mut self, ch: char, now_ms: u64) -> Option<usize> {
        if now_ms.saturating_sub(self.type_at_ms) > TYPE_AHEAD_MS {
            self.type_buf.clear();
        }
        self.type_at_ms = now_ms;
        self.type_buf.push(ch);
        let buf = self.type_buf.to_lowercase();
        let mut chars = buf.chars();
        let first = chars.next()?;
        let repeated = chars.all(|c| c == first);
        let (prefix, skip_current) = if repeated {
            (first.to_string(), true)
        } else {
            (buf, false)
        };
        let n = self.rows.len();
        if n == 0 {
            return None;
        }
        let start = self.highlight.unwrap_or(n - 1);
        // 从当前行之后（重复按键）或当前行起（多字符前缀）循环查找。
        let offset = if skip_current || self.highlight.is_none() {
            1
        } else {
            0
        };
        let found = (0..n).map(|k| (start + offset + k) % n).find(|&r| {
            self.is_option_row(r) && self.row_label(r).to_lowercase().starts_with(&prefix)
        });
        if let Some(r) = found {
            self.highlight = Some(r);
        }
        found
    }

    /// 收起态键入跳转：直接改选中值，返回新值（未变化或没匹配返回 `None`）。
    pub fn type_ahead_closed(&mut self, ch: char, now_ms: u64) -> Option<usize> {
        // 借用行列表做匹配：收起态没有浮层，但 rows 保持完整列表。
        self.filter.clear();
        self.rebuild_rows();
        self.highlight = self.selected_row();
        let row = self.type_ahead(ch, now_ms)?;
        let RowKind::Option(opt) = self.rows[row].kind else {
            return None;
        };
        if self.selected == Some(opt) {
            None
        } else {
            self.selected = Some(opt);
            Some(opt)
        }
    }
}

/// 估算浮层需要的宽度（pt）：最长文字的显示列数 × 字号 × 0.55，加两侧留白与打勾列，封顶 [`MAX_MENU_WIDTH`]。
pub fn estimate_menu_width(state: &DropdownState, font_size: f32) -> f32 {
    use unicode_width::UnicodeWidthStr;
    let cols = state
        .items
        .iter()
        .map(|it| match it {
            Item::Option(s) | Item::Header(s) => UnicodeWidthStr::width(s.as_str()),
            Item::Separator => 0,
        })
        .max()
        .unwrap_or(0);
    // 2 × 内边距 + 打勾列 + 文字右侧留白。
    (cols as f32 * font_size * 0.55 + MENU_PADDING * 2.0 + 28.0 + 12.0).min(MAX_MENU_WIDTH)
}

/// 矩形（窗口坐标，pt）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// 左。
    pub x: f32,
    /// 上。
    pub y: f32,
    /// 宽。
    pub w: f32,
    /// 高。
    pub h: f32,
}

/// 浮层的定位结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// 浮层矩形。
    pub rect: Rect,
    /// 是否翻到了按钮上方。
    pub above: bool,
}

/// 浮层内容的自然高度：行数 × 行高 + 内边距（+ 过滤区）。空列表按一行（“无匹配项”）算。
pub fn content_height(row_count: usize, filterable: bool) -> f32 {
    row_count.max(1) as f32 * ROW_HEIGHT
        + MENU_PADDING * 2.0
        + if filterable { FILTER_AREA_HEIGHT } else { 0.0 }
}

/// 计算浮层位置：默认在按钮下方；下方放不下且上方更宽裕时翻到上方；
/// 宽度至少与按钮同宽，且不超出窗口；高度不超过 `MAX_MENU_HEIGHT` 与可用空间。
pub fn place(button: Rect, viewport: (f32, f32), content_h: f32, desired_w: f32) -> Placement {
    let (vw, vh) = viewport;
    let max_w = (vw - WINDOW_MARGIN * 2.0).max(button.w.min(vw));
    let w = desired_w.max(button.w).min(max_w);
    let x = button
        .x
        .min(vw - WINDOW_MARGIN - w)
        .max(WINDOW_MARGIN.min((vw - w).max(0.0)));

    let want = content_h.min(MAX_MENU_HEIGHT);
    let below_space = (vh - WINDOW_MARGIN - (button.y + button.h + GAP)).max(0.0);
    let above_space = (button.y - GAP - WINDOW_MARGIN).max(0.0);
    let min_h = ROW_HEIGHT + MENU_PADDING * 2.0;
    let above = want > below_space && above_space > below_space;
    if above {
        let h = want.min(above_space).max(min_h.min(above_space));
        Placement {
            rect: Rect {
                x,
                y: button.y - GAP - h,
                w,
                h,
            },
            above: true,
        }
    } else {
        let h = want.min(below_space).max(min_h.min(below_space));
        Placement {
            rect: Rect {
                x,
                y: button.y + button.h + GAP,
                w,
                h,
            },
            above: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item::option(*n)).collect()
    }

    fn grouped() -> DropdownState {
        DropdownState::new(vec![
            Item::header("Unicode"),
            Item::option("UTF-8"),
            Item::option("UTF-16"),
            Item::Separator,
            Item::header("中文"),
            Item::option("GBK"),
            Item::option("GB18030"),
            Item::Separator,
            Item::option("Big5"),
        ])
    }

    #[test]
    fn option_indices_skip_headers() {
        let s = grouped();
        assert_eq!(s.option_count(), 5);
        assert_eq!(s.option_label(0), Some("UTF-8"));
        assert_eq!(s.option_label(4), Some("Big5"));
        assert_eq!(s.option_label(5), None);
    }

    #[test]
    fn rows_carry_separators_on_next_row() {
        let s = grouped();
        let r = s.rows();
        assert_eq!(r.len(), 7);
        assert!(!r[0].separator_before);
        // “中文”标题行带分隔线，Big5 行带分隔线。
        assert_eq!(r[3].kind, RowKind::Header(4));
        assert!(r[3].separator_before);
        assert_eq!(r[6].kind, RowKind::Option(4));
        assert!(r[6].separator_before);
    }

    #[test]
    fn open_highlights_selected_else_first_option() {
        let mut s = grouped();
        assert_eq!(s.open(), Some(1)); // 跳过标题
        s.close();
        s.set_selected(Some(3));
        assert_eq!(s.open(), Some(5));
        assert_eq!(s.highlighted_option(), Some(3));
    }

    #[test]
    fn move_skips_headers_and_clamps() {
        let mut s = grouped();
        s.open();
        assert_eq!(s.move_highlight(1), Some(2));
        assert_eq!(s.move_highlight(1), Some(4)); // 越过“中文”标题
        assert_eq!(s.move_highlight(-1), Some(2));
        assert_eq!(s.move_highlight(100), Some(6));
        assert_eq!(s.move_highlight(1), Some(6));
        assert_eq!(s.highlight_first(), Some(1));
        assert_eq!(s.move_highlight(-1), Some(1));
        assert_eq!(s.highlight_last(), Some(6));
    }

    #[test]
    fn page_moves_by_visible_rows() {
        let names: Vec<String> = (0..50).map(|i| format!("f{i}")).collect();
        let mut s = DropdownState::new(names.iter().map(|n| Item::option(n.as_str())).collect());
        s.open();
        assert_eq!(s.page(true, 10), Some(9));
        assert_eq!(s.page(true, 10), Some(18));
        assert_eq!(s.page(false, 10), Some(9));
        assert_eq!(s.page(true, 1), Some(10)); // 至少 1 行
    }

    #[test]
    fn accept_sets_value_and_closes() {
        let mut s = grouped();
        s.open();
        s.move_highlight(2);
        assert_eq!(s.accept(), Some(2));
        assert_eq!(s.selected(), Some(2));
        assert!(!s.is_open());
        assert_eq!(s.selected_label(), Some("GBK"));
    }

    #[test]
    fn escape_path_keeps_value() {
        let mut s = grouped();
        s.set_selected(Some(1));
        s.open();
        s.move_highlight(1);
        s.close();
        assert_eq!(s.selected(), Some(1));
    }

    #[test]
    fn filter_is_case_insensitive_substring_and_hides_headers() {
        let mut s = grouped();
        s.open();
        let h = s.set_filter("gb");
        assert_eq!(s.rows().len(), 2);
        assert_eq!(h, Some(0));
        assert_eq!(s.row_label(0), "GBK");
        assert!(s.rows().iter().all(|r| !r.separator_before));
        s.set_filter("");
        assert_eq!(s.rows().len(), 7);
        s.set_filter("zzz");
        assert!(s.rows().is_empty());
        assert_eq!(s.highlight(), None);
        assert_eq!(s.accept(), None);
    }

    #[test]
    fn filter_supports_chinese_and_unicode_case() {
        let mut s = DropdownState::new(opts(&["微软雅黑", "宋体", "Ünïcode Sans", "思源黑体"]));
        s.open();
        s.set_filter("黑");
        assert_eq!(s.rows().len(), 2);
        s.set_filter("ÜN");
        assert_eq!(s.rows().len(), 1);
    }

    #[test]
    fn filter_keeps_highlight_on_selected_when_cleared() {
        let mut s = DropdownState::new(opts(&["a", "bb", "bc", "d"]));
        s.set_selected(Some(2));
        s.open();
        s.set_filter("b");
        assert_eq!(s.highlighted_option(), Some(1)); // 过滤时指向第一个匹配
        let h = s.set_filter("");
        assert_eq!(h, Some(2)); // 清空后回到选中项
    }

    #[test]
    fn option_index_is_stable_under_filter() {
        let mut s = grouped();
        s.open();
        s.set_filter("big");
        s.highlight_first();
        assert_eq!(s.accept(), Some(4));
    }

    #[test]
    fn step_selected_clamps_and_reports_change() {
        let mut s = DropdownState::new(opts(&["a", "b", "c"]));
        assert_eq!(s.step_selected(1), Some(0)); // 无值时下键选第一项
        assert_eq!(s.step_selected(1), Some(1));
        assert_eq!(s.step_selected(1), Some(2));
        assert_eq!(s.step_selected(1), None);
        assert_eq!(s.step_selected(-1), Some(1));
        let mut e = DropdownState::new(vec![]);
        assert_eq!(e.step_selected(1), None);
    }

    #[test]
    fn type_ahead_prefix_and_cycle() {
        let mut s =
            DropdownState::new(opts(&["Apple", "Avocado", "Banana", "Blueberry", "Cherry"]));
        s.open();
        assert_eq!(s.type_ahead('b', 0), Some(2));
        assert_eq!(s.type_ahead('l', 100), Some(3)); // "bl"
        // 超时后重新开始。
        assert_eq!(s.type_ahead('a', 5000), Some(0));
        // 同字符重复：轮换。
        assert_eq!(s.type_ahead('a', 5100), Some(1));
        assert_eq!(s.type_ahead('a', 5200), Some(0));
        // 无匹配：高亮不动。
        assert_eq!(s.type_ahead('z', 9000), None);
        assert_eq!(s.highlight(), Some(0));
    }

    #[test]
    fn type_ahead_is_case_insensitive_and_skips_headers() {
        let mut s = grouped();
        s.open();
        assert_eq!(s.type_ahead('G', 0), Some(4));
        // “中文”标题不会被当作匹配。
        assert_eq!(s.type_ahead('中', 5000), None);
    }

    #[test]
    fn type_ahead_closed_changes_value() {
        let mut s = DropdownState::new(opts(&["UTF-8", "GBK", "GB18030", "Big5"]));
        assert_eq!(s.type_ahead_closed('g', 0), Some(1));
        assert_eq!(s.selected(), Some(1));
        assert_eq!(s.type_ahead_closed('g', 100), Some(2)); // 轮换
        assert_eq!(s.type_ahead_closed('b', 5000), Some(3));
        assert_eq!(s.type_ahead_closed('b', 6000), None); // 只有一个以 b 开头，不变
    }

    #[test]
    fn set_items_resets_out_of_range_selection() {
        let mut s = DropdownState::new(opts(&["a", "b", "c"]));
        s.set_selected(Some(2));
        s.set_items(opts(&["a"]));
        assert_eq!(s.selected(), None);
        s.set_selected(Some(7));
        assert_eq!(s.selected(), None);
    }

    fn btn(x: f32, y: f32, w: f32) -> Rect {
        Rect { x, y, w, h: 30.0 }
    }

    #[test]
    fn place_below_by_default() {
        let p = place(btn(20.0, 100.0, 200.0), (600.0, 700.0), 150.0, 100.0);
        assert!(!p.above);
        assert_eq!(p.rect.y, 100.0 + 30.0 + GAP);
        assert_eq!(p.rect.w, 200.0); // 至少与按钮同宽
        assert_eq!(p.rect.h, 150.0);
        assert_eq!(p.rect.x, 20.0);
    }

    #[test]
    fn place_flips_up_near_bottom() {
        let p = place(btn(20.0, 600.0, 200.0), (600.0, 700.0), 300.0, 100.0);
        assert!(p.above);
        assert_eq!(p.rect.h, 300.0);
        assert_eq!(p.rect.y + p.rect.h, 600.0 - GAP);
    }

    #[test]
    fn place_stays_below_when_both_cramped_but_below_larger() {
        let p = place(btn(20.0, 10.0, 200.0), (600.0, 300.0), 400.0, 100.0);
        assert!(!p.above);
        assert!(p.rect.y + p.rect.h <= 300.0 - WINDOW_MARGIN + 0.01);
    }

    #[test]
    fn place_caps_height_and_shrinks_to_space() {
        let p = place(btn(20.0, 50.0, 200.0), (600.0, 900.0), 5000.0, 100.0);
        assert_eq!(p.rect.h, MAX_MENU_HEIGHT);
        // 上下都不够时取较大的一侧并收缩。
        let q = place(btn(20.0, 100.0, 200.0), (600.0, 260.0), 300.0, 100.0);
        assert!(q.rect.y >= 0.0 && q.rect.y + q.rect.h <= 260.0);
    }

    #[test]
    fn place_keeps_inside_window_horizontally() {
        let p = place(btn(450.0, 100.0, 120.0), (600.0, 700.0), 100.0, 300.0);
        assert!(p.rect.x + p.rect.w <= 600.0 - WINDOW_MARGIN + 0.01);
        assert!(p.rect.x >= WINDOW_MARGIN);
        assert_eq!(p.rect.w, 300.0);
        // 比窗口还宽时收缩到窗口内。
        let q = place(btn(10.0, 100.0, 120.0), (300.0, 700.0), 100.0, 900.0);
        assert!(q.rect.w <= 300.0 - 2.0 * WINDOW_MARGIN + 0.01);
    }

    #[test]
    fn content_height_counts_filter_area_and_empty_row() {
        assert_eq!(content_height(3, false), 3.0 * ROW_HEIGHT + 8.0);
        assert_eq!(content_height(0, false), ROW_HEIGHT + 8.0);
        assert_eq!(
            content_height(3, true),
            3.0 * ROW_HEIGHT + 8.0 + FILTER_AREA_HEIGHT
        );
    }

    #[test]
    fn estimated_width_counts_wide_chars_double_and_caps() {
        let a = DropdownState::new(opts(&["abcd"]));
        let b = DropdownState::new(opts(&["中文字体"]));
        assert!(estimate_menu_width(&b, 13.0) > estimate_menu_width(&a, 13.0));
        let long = DropdownState::new(vec![Item::option("x".repeat(500))]);
        assert_eq!(estimate_menu_width(&long, 13.0), MAX_MENU_WIDTH);
    }
}
