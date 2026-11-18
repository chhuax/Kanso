//! 右键菜单（上下文菜单）的**纯逻辑**：菜单模型、键盘 / 悬停导航状态机、尺寸估算与翻转定位。
//!
//! 视图在 `ui/widgets/menu.rs`（只在 Windows 编译），本文件不依赖窗口系统，三个平台都能测试。
//!
//! 约定：
//! - 坐标与尺寸一律是逻辑像素（`f32`），原点在窗口左上角；
//! - 菜单可以多级：`levels` 里第 0 级是根菜单，第 k 级是第 k-1 级当前高亮的子菜单项展开的内容；
//! - 禁用项与分隔线不可高亮、不可执行；悬停在它们上面时高亮清空（与 macOS 一致）。

use unicode_width::UnicodeWidthStr;

/// 行高。
pub const ITEM_H: f32 = 26.0;
/// 分隔线所占高度。
pub const SEP_H: f32 = 9.0;
/// 菜单上下内边距。
pub const PAD_Y: f32 = 5.0;
/// 菜单左右内边距（高亮条距菜单边缘）。
pub const PAD_X: f32 = 5.0;
/// 条目内部左侧留给勾选标记的宽度。
pub const CHECK_W: f32 = 22.0;
/// 条目内部右侧留给子菜单箭头的宽度。
pub const ARROW_W: f32 = 22.0;
/// 文字与右侧快捷键提示之间的最小间距。
pub const SHORTCUT_GAP: f32 = 28.0;
/// 菜单最小宽度。
pub const MIN_W: f32 = 168.0;
/// 估算文字宽度时每个显示列的像素数（13pt 界面字体：半角约 7，全角按 2 列即 14）。
pub const COL_W: f32 = 7.0;
/// 子菜单与父菜单重叠的宽度。
pub const SUBMENU_OVERLAP: f32 = 4.0;

/// 菜单项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// 可执行的普通项。
    Action {
        /// 选中时回传给调用方的标识。
        id: String,
        /// 显示文字。
        label: String,
        /// 右侧快捷键提示（例如 `Ctrl+C`），只做显示。
        shortcut: Option<String>,
        /// 是否可用。
        enabled: bool,
        /// 是否带勾选标记。
        checked: bool,
    },
    /// 分隔线。
    Separator,
    /// 一级子菜单。
    Submenu {
        /// 显示文字。
        label: String,
        /// 是否可用。
        enabled: bool,
        /// 子项。
        items: Vec<MenuItem>,
    },
}

impl MenuItem {
    /// 普通项。
    pub fn action(id: impl Into<String>, label: impl Into<String>) -> MenuItem {
        MenuItem::Action {
            id: id.into(),
            label: label.into(),
            shortcut: None,
            enabled: true,
            checked: false,
        }
    }

    /// 分隔线。
    pub fn separator() -> MenuItem {
        MenuItem::Separator
    }

    /// 子菜单。
    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> MenuItem {
        MenuItem::Submenu {
            label: label.into(),
            enabled: true,
            items,
        }
    }

    /// 设置快捷键提示（仅对普通项有效）。
    pub fn shortcut(mut self, text: impl Into<String>) -> MenuItem {
        if let MenuItem::Action { shortcut, .. } = &mut self {
            *shortcut = Some(text.into());
        }
        self
    }

    /// 设置勾选状态（仅对普通项有效）。
    pub fn checked(mut self, value: bool) -> MenuItem {
        if let MenuItem::Action { checked, .. } = &mut self {
            *checked = value;
        }
        self
    }

    /// 设为禁用。
    pub fn disabled(mut self) -> MenuItem {
        match &mut self {
            MenuItem::Action { enabled, .. } | MenuItem::Submenu { enabled, .. } => {
                *enabled = false;
            }
            MenuItem::Separator => {}
        }
        self
    }

    /// 能否被高亮 / 执行。
    pub fn is_selectable(&self) -> bool {
        match self {
            MenuItem::Action { enabled, .. } | MenuItem::Submenu { enabled, .. } => *enabled,
            MenuItem::Separator => false,
        }
    }

    /// 是不是（可用的）子菜单。
    pub fn is_open_submenu(&self) -> bool {
        matches!(self, MenuItem::Submenu { enabled: true, .. })
    }

    /// 显示文字（分隔线为空）。
    pub fn label(&self) -> &str {
        match self {
            MenuItem::Action { label, .. } | MenuItem::Submenu { label, .. } => label,
            MenuItem::Separator => "",
        }
    }

    fn height(&self) -> f32 {
        match self {
            MenuItem::Separator => SEP_H,
            _ => ITEM_H,
        }
    }

    /// 条目所需的内容宽度（不含菜单内边距）。
    fn content_width(&self) -> f32 {
        let text = |s: &str| UnicodeWidthStr::width(s) as f32 * COL_W;
        match self {
            MenuItem::Separator => 0.0,
            MenuItem::Action {
                label, shortcut, ..
            } => {
                let sc = shortcut.as_deref().map_or(0.0, |s| text(s) + SHORTCUT_GAP);
                CHECK_W + text(label) + sc + ARROW_W
            }
            MenuItem::Submenu { label, .. } => CHECK_W + text(label) + ARROW_W,
        }
    }
}

/// 键盘输入（视图把按键翻译成它）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    /// 上。
    Up,
    /// 下。
    Down,
    /// 左：退出当前子菜单。
    Left,
    /// 右：进入子菜单。
    Right,
    /// 回车。
    Enter,
    /// 空格。
    Space,
    /// Esc。
    Escape,
    /// Home。
    Home,
    /// End。
    End,
    /// 可打印字符，按首字母跳转。
    Char(char),
}

/// 一次操作的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuOutcome {
    /// 没有需要调用方处理的事（可能内部状态变了，需要重绘）。
    None,
    /// 关闭整个菜单。
    Close,
    /// 执行了某项（携带 `id`），调用方应关闭菜单并处理。
    Activate(String),
}

/// 矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
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

impl Rect {
    /// 右边界。
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    /// 下边界。
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    /// 是否包含点。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// 一级菜单的布局。
#[derive(Clone, Debug, PartialEq)]
pub struct LevelLayout {
    /// 菜单面板在窗口里的矩形。
    pub rect: Rect,
    /// 每个条目相对面板顶部的 `(上边, 高度)`。
    pub rows: Vec<(f32, f32)>,
}

/// 估算一级菜单的尺寸与各行位置（面板左上角为原点）。返回 `(宽, 高, 各行)`。
pub fn measure(items: &[MenuItem]) -> (f32, f32, Vec<(f32, f32)>) {
    let mut y = PAD_Y;
    let mut rows = Vec::with_capacity(items.len());
    let mut w: f32 = 0.0;
    for it in items {
        let h = it.height();
        rows.push((y, h));
        y += h;
        w = w.max(it.content_width());
    }
    let width = (w + PAD_X * 2.0).max(MIN_W).ceil();
    (width, y + PAD_Y, rows)
}

/// 把 `pos` 限制在 `[0, max(limit - size, 0)]`。
fn clamp_axis(pos: f32, size: f32, limit: f32) -> f32 {
    pos.min(limit - size).max(0.0)
}

/// 根菜单定位：默认左上角贴在鼠标处；右侧 / 下侧放不下时翻到鼠标的左侧 / 上侧；仍放不下则贴边。
pub fn place_root(anchor: (f32, f32), size: (f32, f32), window: (f32, f32)) -> (f32, f32) {
    let x = if anchor.0 + size.0 <= window.0 {
        anchor.0
    } else {
        anchor.0 - size.0
    };
    let y = if anchor.1 + size.1 <= window.1 {
        anchor.1
    } else {
        anchor.1 - size.1
    };
    (
        clamp_axis(x, size.0, window.0),
        clamp_axis(y, size.1, window.1),
    )
}

/// 子菜单定位：默认在父菜单右侧、与触发行顶部对齐；右侧放不下翻到父菜单左侧；
/// 下侧放不下则让底边与触发行底边对齐，仍放不下就上移到贴窗口边。
/// `row` 是触发行相对父面板顶部的 `(上边, 高度)`。
pub fn place_submenu(
    parent: Rect,
    row: (f32, f32),
    size: (f32, f32),
    window: (f32, f32),
) -> (f32, f32) {
    let right_x = parent.right() - SUBMENU_OVERLAP;
    let x = if right_x + size.0 <= window.0 {
        right_x
    } else {
        parent.x + SUBMENU_OVERLAP - size.0
    };
    let row_top = parent.y + row.0;
    let y = if row_top - PAD_Y + size.1 <= window.1 {
        row_top - PAD_Y
    } else {
        row_top + row.1 + PAD_Y - size.1
    };
    (
        clamp_axis(x, size.0, window.0),
        clamp_axis(y, size.1, window.1),
    )
}

/// 菜单状态：模型 + 展开的层级与各层高亮。
#[derive(Clone, Debug)]
pub struct MenuState {
    root: Vec<MenuItem>,
    /// 每一展开层的高亮下标；`levels[0]` 是根菜单。
    levels: Vec<Option<usize>>,
}

impl MenuState {
    /// 新建，只展开根菜单，没有高亮。
    pub fn new(items: Vec<MenuItem>) -> MenuState {
        MenuState {
            root: items,
            levels: vec![None],
        }
    }

    /// 已展开的层数（至少 1）。
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// 第 `level` 层的条目。
    pub fn items_at(&self, level: usize) -> &[MenuItem] {
        let mut items: &[MenuItem] = &self.root;
        for l in 0..level {
            let Some(i) = self.levels.get(l).copied().flatten() else {
                return &[];
            };
            match items.get(i) {
                Some(MenuItem::Submenu { items: sub, .. }) => items = sub,
                _ => return &[],
            }
        }
        items
    }

    /// 第 `level` 层高亮的条目下标。
    pub fn highlighted(&self, level: usize) -> Option<usize> {
        self.levels.get(level).copied().flatten()
    }

    /// 计算所有展开层的位置。`anchor` 是鼠标位置，`window` 是窗口尺寸。
    pub fn layout(&self, anchor: (f32, f32), window: (f32, f32)) -> Vec<LevelLayout> {
        let mut out: Vec<LevelLayout> = Vec::with_capacity(self.levels.len());
        for level in 0..self.levels.len() {
            let (w, h, rows) = measure(self.items_at(level));
            let (x, y) = match out.last() {
                None => place_root(anchor, (w, h), window),
                Some(parent) => {
                    let row = self
                        .highlighted(level - 1)
                        .and_then(|i| parent.rows.get(i).copied())
                        .unwrap_or((PAD_Y, ITEM_H));
                    place_submenu(parent.rect, row, (w, h), window)
                }
            };
            out.push(LevelLayout {
                rect: Rect { x, y, w, h },
                rows,
            });
        }
        out
    }

    /// 点落在第几层（最上层优先）与第几行；不在任何菜单内返回 `None`。
    pub fn hit_test(layouts: &[LevelLayout], x: f32, y: f32) -> Option<(usize, Option<usize>)> {
        for (level, l) in layouts.iter().enumerate().rev() {
            if l.rect.contains(x, y) {
                let ry = y - l.rect.y;
                let row = l
                    .rows
                    .iter()
                    .position(|&(top, h)| ry >= top && ry < top + h);
                return Some((level, row));
            }
        }
        None
    }

    fn selectable(&self, level: usize, idx: usize) -> bool {
        self.items_at(level)
            .get(idx)
            .is_some_and(MenuItem::is_selectable)
    }

    fn truncate_to(&mut self, level: usize) {
        self.levels.truncate(level + 1);
    }

    /// 展开 `level` 层高亮项的子菜单（若它是可用子菜单），`select_first` 时高亮其中第一项。
    fn open_submenu(&mut self, level: usize, select_first: bool) -> bool {
        let Some(i) = self.highlighted(level) else {
            return false;
        };
        if !self
            .items_at(level)
            .get(i)
            .is_some_and(MenuItem::is_open_submenu)
        {
            return false;
        }
        self.truncate_to(level);
        self.levels.push(None);
        if select_first {
            self.levels[level + 1] = self.first_selectable(level + 1);
        }
        true
    }

    fn first_selectable(&self, level: usize) -> Option<usize> {
        self.items_at(level)
            .iter()
            .position(MenuItem::is_selectable)
    }

    fn last_selectable(&self, level: usize) -> Option<usize> {
        self.items_at(level)
            .iter()
            .rposition(MenuItem::is_selectable)
    }

    /// 鼠标悬停在第 `level` 层第 `idx` 行（`None` 表示悬停在面板空白处）。
    /// 返回状态是否变化（需要重绘）。
    pub fn hover(&mut self, level: usize, idx: Option<usize>) -> bool {
        if level >= self.levels.len() {
            return false;
        }
        let target = idx.filter(|&i| self.selectable(level, i));
        if self.levels[level] == target {
            // 同一行：已展开（或更深层展开）就保持；用键盘高亮了子菜单行但没展开时补展开。
            return target.is_some()
                && self.levels.len() == level + 1
                && self.open_submenu(level, false);
        }
        self.truncate_to(level);
        self.levels[level] = target;
        if target.is_some() {
            self.open_submenu(level, false);
        }
        true
    }

    /// 鼠标点击第 `level` 层第 `idx` 行。
    pub fn click(&mut self, level: usize, idx: usize) -> MenuOutcome {
        if level >= self.levels.len() || !self.selectable(level, idx) {
            return MenuOutcome::None;
        }
        self.truncate_to(level);
        self.levels[level] = Some(idx);
        match self.items_at(level).get(idx) {
            Some(MenuItem::Action { id, .. }) => MenuOutcome::Activate(id.clone()),
            Some(MenuItem::Submenu { .. }) => {
                self.open_submenu(level, false);
                MenuOutcome::None
            }
            _ => MenuOutcome::None,
        }
    }

    /// 键盘输入，作用于最深一层。
    pub fn key(&mut self, key: MenuKey) -> MenuOutcome {
        let level = self.levels.len() - 1;
        match key {
            MenuKey::Down => self.step(level, true),
            MenuKey::Up => self.step(level, false),
            MenuKey::Home => self.levels[level] = self.first_selectable(level),
            MenuKey::End => self.levels[level] = self.last_selectable(level),
            MenuKey::Right => {
                self.open_submenu(level, true);
            }
            MenuKey::Left => {
                if level > 0 {
                    self.truncate_to(level - 1);
                }
            }
            MenuKey::Escape => {
                if level > 0 {
                    self.truncate_to(level - 1);
                } else {
                    return MenuOutcome::Close;
                }
            }
            MenuKey::Enter | MenuKey::Space => {
                let Some(i) = self.highlighted(level) else {
                    return MenuOutcome::None;
                };
                return match self.items_at(level).get(i) {
                    Some(MenuItem::Action {
                        id, enabled: true, ..
                    }) => MenuOutcome::Activate(id.clone()),
                    Some(MenuItem::Submenu { enabled: true, .. }) => {
                        self.open_submenu(level, true);
                        MenuOutcome::None
                    }
                    _ => MenuOutcome::None,
                };
            }
            MenuKey::Char(c) => self.type_ahead(level, c),
        }
        MenuOutcome::None
    }

    /// 上下移动，跳过分隔线与禁用项，首尾循环。
    fn step(&mut self, level: usize, down: bool) {
        let items = self.items_at(level);
        let n = items.len();
        if !items.iter().any(MenuItem::is_selectable) {
            return;
        }
        // 从“第一项之前 / 最后一项之后”起步，使无高亮时 Down 选第一个、Up 选最后一个。
        let mut i = match (self.levels[level], down) {
            (Some(i), _) => i,
            (None, true) => n - 1,
            (None, false) => 0,
        };
        loop {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if items[i].is_selectable() {
                break;
            }
        }
        self.levels[level] = Some(i);
    }

    /// 按首字母跳转：从当前高亮的下一项开始循环找标签以该字母开头的可选项。
    fn type_ahead(&mut self, level: usize, c: char) {
        let items = self.items_at(level);
        let n = items.len();
        if n == 0 {
            return;
        }
        let want = c.to_lowercase().next().unwrap_or(c);
        let start = self.levels[level].map_or(0, |i| i + 1);
        let found = (0..n).map(|k| (start + k) % n).find(|&i| {
            items[i].is_selectable()
                && items[i]
                    .label()
                    .chars()
                    .find(|ch| ch.is_alphanumeric())
                    .is_some_and(|ch| ch.to_lowercase().next() == Some(want))
        });
        if let Some(i) = found {
            self.levels[level] = Some(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<MenuItem> {
        vec![
            MenuItem::action("copy", "Copy").shortcut("Ctrl+C"),
            MenuItem::action("paste", "Paste").shortcut("Ctrl+V"),
            MenuItem::separator(),
            MenuItem::action("clear", "Clear").disabled(),
            MenuItem::action("wrap", "Wrap").checked(true),
            MenuItem::submenu(
                "Split",
                vec![
                    MenuItem::action("right", "Right"),
                    MenuItem::action("down", "Down").disabled(),
                    MenuItem::action("left", "Left"),
                ],
            ),
        ]
    }

    #[test]
    fn down_skips_separator_and_disabled_and_wraps() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::Down);
        assert_eq!(m.highlighted(0), Some(0));
        m.key(MenuKey::Down);
        assert_eq!(m.highlighted(0), Some(1));
        m.key(MenuKey::Down); // 跳过分隔线(2)与禁用(3)
        assert_eq!(m.highlighted(0), Some(4));
        m.key(MenuKey::Down);
        assert_eq!(m.highlighted(0), Some(5));
        m.key(MenuKey::Down); // 循环
        assert_eq!(m.highlighted(0), Some(0));
        m.key(MenuKey::Up);
        assert_eq!(m.highlighted(0), Some(5));
    }

    #[test]
    fn up_from_nothing_selects_last_and_home_end() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::Up);
        assert_eq!(m.highlighted(0), Some(5));
        m.key(MenuKey::Home);
        assert_eq!(m.highlighted(0), Some(0));
        m.key(MenuKey::End);
        assert_eq!(m.highlighted(0), Some(5));
    }

    #[test]
    fn enter_and_space_activate() {
        let mut m = MenuState::new(sample());
        assert_eq!(m.key(MenuKey::Enter), MenuOutcome::None);
        m.key(MenuKey::Down);
        assert_eq!(m.key(MenuKey::Enter), MenuOutcome::Activate("copy".into()));
        m.key(MenuKey::Down);
        assert_eq!(m.key(MenuKey::Space), MenuOutcome::Activate("paste".into()));
    }

    #[test]
    fn checked_item_activates_normally() {
        let mut m = MenuState::new(sample());
        assert_eq!(m.click(0, 4), MenuOutcome::Activate("wrap".into()));
    }

    #[test]
    fn submenu_open_close_by_keys() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::End);
        assert_eq!(m.depth(), 1);
        m.key(MenuKey::Right);
        assert_eq!(m.depth(), 2);
        assert_eq!(m.highlighted(1), Some(0));
        m.key(MenuKey::Down); // 跳过禁用的 Down
        assert_eq!(m.highlighted(1), Some(2));
        m.key(MenuKey::Left);
        assert_eq!(m.depth(), 1);
        assert_eq!(m.highlighted(0), Some(5));
        // Enter 在子菜单项上展开子菜单
        m.key(MenuKey::Enter);
        assert_eq!(m.depth(), 2);
        assert_eq!(m.key(MenuKey::Enter), MenuOutcome::Activate("right".into()));
    }

    #[test]
    fn right_on_normal_item_and_left_at_root_do_nothing() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::Down);
        m.key(MenuKey::Right);
        assert_eq!(m.depth(), 1);
        assert_eq!(m.key(MenuKey::Left), MenuOutcome::None);
        assert_eq!(m.depth(), 1);
    }

    #[test]
    fn escape_closes_one_level_at_a_time() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::End);
        m.key(MenuKey::Right);
        assert_eq!(m.key(MenuKey::Escape), MenuOutcome::None);
        assert_eq!(m.depth(), 1);
        assert_eq!(m.key(MenuKey::Escape), MenuOutcome::Close);
    }

    #[test]
    fn type_ahead_jumps_and_cycles() {
        let mut m = MenuState::new(vec![
            MenuItem::action("a1", "Apple"),
            MenuItem::action("b", "Banana"),
            MenuItem::action("a2", "Avocado").disabled(),
            MenuItem::action("a3", "Apricot"),
        ]);
        m.key(MenuKey::Char('b'));
        assert_eq!(m.highlighted(0), Some(1));
        m.key(MenuKey::Char('A')); // 跳过禁用的 Avocado
        assert_eq!(m.highlighted(0), Some(3));
        m.key(MenuKey::Char('a'));
        assert_eq!(m.highlighted(0), Some(0));
        m.key(MenuKey::Char('z'));
        assert_eq!(m.highlighted(0), Some(0));
    }

    #[test]
    fn hover_highlights_and_opens_submenu() {
        let mut m = MenuState::new(sample());
        assert!(m.hover(0, Some(0)));
        assert_eq!(m.highlighted(0), Some(0));
        assert!(!m.hover(0, Some(0)));
        // 悬停禁用项 / 分隔线：高亮清空
        assert!(m.hover(0, Some(3)));
        assert_eq!(m.highlighted(0), None);
        assert!(!m.hover(0, Some(2)));
        assert_eq!(m.highlighted(0), None);
        // 悬停子菜单父行展开
        assert!(m.hover(0, Some(5)));
        assert_eq!(m.depth(), 2);
        assert!(!m.hover(0, Some(5)));
        assert_eq!(m.depth(), 2);
        // 在子菜单里悬停
        assert!(m.hover(1, Some(2)));
        assert_eq!(m.highlighted(1), Some(2));
        assert_eq!(m.depth(), 2);
        // 回到父菜单其它行：收起子菜单
        assert!(m.hover(0, Some(1)));
        assert_eq!(m.depth(), 1);
    }

    #[test]
    fn hover_reopens_keyboard_highlighted_submenu_row() {
        let mut m = MenuState::new(sample());
        m.key(MenuKey::End);
        assert_eq!(m.depth(), 1);
        assert!(m.hover(0, Some(5)));
        assert_eq!(m.depth(), 2);
    }

    #[test]
    fn click_activates_enabled_only() {
        let mut m = MenuState::new(sample());
        assert_eq!(m.click(0, 0), MenuOutcome::Activate("copy".into()));
        assert_eq!(m.click(0, 3), MenuOutcome::None);
        assert_eq!(m.click(0, 2), MenuOutcome::None);
        assert_eq!(m.click(0, 5), MenuOutcome::None);
        assert_eq!(m.depth(), 2);
        assert_eq!(m.click(1, 1), MenuOutcome::None); // 禁用
        assert_eq!(m.click(1, 2), MenuOutcome::Activate("left".into()));
    }

    #[test]
    fn empty_and_all_disabled_menus_do_not_panic() {
        let mut m = MenuState::new(vec![]);
        assert_eq!(m.key(MenuKey::Down), MenuOutcome::None);
        assert_eq!(m.key(MenuKey::Up), MenuOutcome::None);
        assert_eq!(m.key(MenuKey::Char('a')), MenuOutcome::None);
        let mut m = MenuState::new(vec![MenuItem::action("x", "X").disabled()]);
        assert_eq!(m.key(MenuKey::Down), MenuOutcome::None);
        assert_eq!(m.highlighted(0), None);
    }

    #[test]
    fn measure_accounts_for_shortcut_cjk_and_rows() {
        let items = vec![
            MenuItem::action("a", "复制").shortcut("Ctrl+C"),
            MenuItem::separator(),
            MenuItem::action("b", "一个相当长的菜单项标题文字文字文字"),
        ];
        let (w, h, rows) = measure(&items);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (PAD_Y, ITEM_H));
        assert_eq!(rows[1], (PAD_Y + ITEM_H, SEP_H));
        assert_eq!(h, PAD_Y * 2.0 + ITEM_H * 2.0 + SEP_H);
        assert!(w > MIN_W, "长标题应撑宽：{w}");
        let (w2, _, _) = measure(&[MenuItem::action("a", "A")]);
        assert_eq!(w2, MIN_W);
    }

    #[test]
    fn root_placement_flips_and_clamps() {
        let win = (800.0, 600.0);
        let size = (200.0, 300.0);
        assert_eq!(place_root((100.0, 100.0), size, win), (100.0, 100.0));
        // 右侧放不下：翻到鼠标左侧
        assert_eq!(place_root((700.0, 100.0), size, win), (500.0, 100.0));
        // 下侧放不下：翻到鼠标上侧
        assert_eq!(place_root((100.0, 500.0), size, win), (100.0, 200.0));
        // 右下角
        assert_eq!(place_root((790.0, 590.0), size, win), (590.0, 290.0));
        // 窗口比菜单还小：贴左上
        assert_eq!(place_root((50.0, 50.0), size, (150.0, 100.0)), (0.0, 0.0));
        // 翻转后仍越过左侧：钳回 0
        assert_eq!(place_root((120.0, 10.0), size, (250.0, 600.0)), (0.0, 10.0));
    }

    #[test]
    fn submenu_placement_flips_and_aligns() {
        let win = (800.0, 600.0);
        let parent = Rect {
            x: 100.0,
            y: 100.0,
            w: 180.0,
            h: 200.0,
        };
        let row = (PAD_Y + ITEM_H * 2.0, ITEM_H);
        let size = (160.0, 100.0);
        let (x, y) = place_submenu(parent, row, size, win);
        assert_eq!(x, parent.right() - SUBMENU_OVERLAP);
        assert_eq!(y, parent.y + row.0 - PAD_Y);
        // 右侧放不下：在父菜单左侧
        let parent_r = Rect { x: 600.0, ..parent };
        let (x, _) = place_submenu(parent_r, row, size, win);
        assert_eq!(x, parent_r.x + SUBMENU_OVERLAP - size.0);
        // 下侧放不下：底边与触发行底边对齐
        let parent_b = Rect {
            y: 480.0,
            h: 100.0,
            ..parent
        };
        let row_b = (PAD_Y + ITEM_H * 3.0, ITEM_H);
        let (_, y) = place_submenu(parent_b, row_b, (160.0, 200.0), win);
        assert_eq!(y, 480.0 + row_b.0 + ITEM_H + PAD_Y - 200.0);
        // 极端：整体钳进窗口
        let (x, y) = place_submenu(parent_b, row_b, (160.0, 700.0), win);
        assert_eq!((x >= 0.0, y), (true, 0.0));
    }

    #[test]
    fn layout_places_all_levels_inside_window() {
        let mut m = MenuState::new(sample());
        m.hover(0, Some(5));
        let win = (400.0, 300.0);
        for anchor in [(5.0, 5.0), (395.0, 5.0), (5.0, 295.0), (395.0, 295.0)] {
            let ls = m.layout(anchor, win);
            assert_eq!(ls.len(), 2);
            for l in &ls {
                assert!(
                    l.rect.x >= 0.0 && l.rect.right() <= win.0,
                    "{anchor:?} {l:?}"
                );
                assert!(
                    l.rect.y >= 0.0 && l.rect.bottom() <= win.1,
                    "{anchor:?} {l:?}"
                );
            }
        }
    }

    #[test]
    fn hit_test_finds_topmost_level_and_row() {
        let mut m = MenuState::new(sample());
        m.hover(0, Some(5));
        let ls = m.layout((10.0, 10.0), (800.0, 600.0));
        let r0 = ls[0].rect;
        assert_eq!(
            MenuState::hit_test(&ls, r0.x + 10.0, r0.y + PAD_Y + 1.0),
            Some((0, Some(0)))
        );
        // 内边距处：命中面板但没有行
        assert_eq!(
            MenuState::hit_test(&ls, r0.x + 10.0, r0.y + 1.0),
            Some((0, None))
        );
        // 重叠区域优先子菜单
        let r1 = ls[1].rect;
        assert_eq!(
            MenuState::hit_test(&ls, r1.x + 1.0, r1.y + PAD_Y + 1.0).map(|h| h.0),
            Some(1)
        );
        assert_eq!(MenuState::hit_test(&ls, 790.0, 590.0), None);
    }
}
