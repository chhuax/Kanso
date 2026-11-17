//! 勾选列表的**纯逻辑**：每行一个复选框，外加键盘光标（上下移动、空格切换）和"全选 / 全不选"。
//!
//! 用于导入 OpenSSH 配置的候选主机列表（store-import.md §7.2）；与具体行内容无关，只管勾选状态。
//! GPUI 视图画复选框与行，点击 / 键盘事件翻译成这里的方法调用。

/// 表头复选框的三态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    /// 一个都没勾（含空列表）。
    None,
    /// 勾了一部分。
    Some,
    /// 全部勾选。
    All,
}

/// 一组复选框的状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checklist {
    checked: Vec<bool>,
    cursor: Option<usize>,
}

impl Checklist {
    /// `len` 行，初始全部为 `initial`。
    pub fn new(len: usize, initial: bool) -> Self {
        Checklist {
            checked: vec![initial; len],
            cursor: None,
        }
    }

    /// 行数。
    pub fn len(&self) -> usize {
        self.checked.len()
    }

    /// 是否没有行。
    pub fn is_empty(&self) -> bool {
        self.checked.is_empty()
    }

    /// 第 `i` 行是否勾选；越界为否。
    pub fn is_checked(&self, i: usize) -> bool {
        self.checked.get(i).copied().unwrap_or(false)
    }

    /// 设置第 `i` 行；越界忽略。
    pub fn set(&mut self, i: usize, on: bool) {
        if let Some(c) = self.checked.get_mut(i) {
            *c = on;
        }
    }

    /// 切换第 `i` 行；越界忽略。
    pub fn toggle(&mut self, i: usize) {
        if let Some(c) = self.checked.get_mut(i) {
            *c = !*c;
        }
    }

    /// 全选。
    pub fn select_all(&mut self) {
        self.checked.fill(true);
    }

    /// 全不选。
    pub fn select_none(&mut self) {
        self.checked.fill(false);
    }

    /// 表头复选框的动作：已经全选就清空，否则全选。
    pub fn toggle_all(&mut self) {
        if self.state() == Tri::All {
            self.select_none();
        } else {
            self.select_all();
        }
    }

    /// 已勾选的行数。
    pub fn count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }

    /// 已勾选行的下标（升序）。
    pub fn checked_indices(&self) -> Vec<usize> {
        self.checked
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.then_some(i))
            .collect()
    }

    /// 表头三态。
    pub fn state(&self) -> Tri {
        match self.count() {
            0 => Tri::None,
            n if n == self.len() => Tri::All,
            _ => Tri::Some,
        }
    }

    // ---- 键盘光标 ----

    /// 当前光标所在行。
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// 把光标放到第 `i` 行（越界夹到末行；空列表为 `None`）。
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = (!self.is_empty()).then(|| i.min(self.len() - 1));
    }

    /// 光标上下移动 `delta` 行，不循环、夹在首末行；还没有光标时 ↓ 落到首行、↑ 落到末行。
    pub fn move_cursor(&mut self, delta: isize) {
        if self.is_empty() {
            self.cursor = None;
            return;
        }
        let last = self.len() - 1;
        self.cursor = Some(match self.cursor {
            None if delta >= 0 => 0,
            None => last,
            Some(c) => c.saturating_add_signed(delta).min(last),
        });
    }

    /// 空格：切换光标所在行；返回是否真的切换了。
    pub fn toggle_cursor(&mut self) -> bool {
        match self.cursor {
            Some(i) if i < self.len() => {
                self.toggle(i);
                true
            }
            _ => false,
        }
    }

    /// 换了一批行：重建，勾选状态与光标都重置。
    pub fn reset(&mut self, len: usize, initial: bool) {
        *self = Checklist::new(len, initial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_and_counts() {
        let c = Checklist::new(3, true);
        assert_eq!(c.len(), 3);
        assert_eq!(c.count(), 3);
        assert_eq!(c.state(), Tri::All);
        assert_eq!(c.checked_indices(), vec![0, 1, 2]);
        let c = Checklist::new(3, false);
        assert_eq!(c.state(), Tri::None);
        assert!(Checklist::new(0, true).is_empty());
        assert_eq!(Checklist::new(0, true).state(), Tri::None);
    }

    #[test]
    fn toggle_set_and_out_of_range() {
        let mut c = Checklist::new(3, false);
        c.toggle(1);
        assert!(c.is_checked(1));
        assert_eq!(c.state(), Tri::Some);
        c.set(0, true);
        c.set(2, true);
        assert_eq!(c.state(), Tri::All);
        c.toggle(1);
        assert_eq!(c.checked_indices(), vec![0, 2]);
        // 越界不 panic。
        c.toggle(9);
        c.set(9, true);
        assert!(!c.is_checked(9));
    }

    #[test]
    fn header_toggle_cycles_all_and_none() {
        let mut c = Checklist::new(3, false);
        c.toggle(0);
        // 部分勾选：先全选。
        c.toggle_all();
        assert_eq!(c.state(), Tri::All);
        c.toggle_all();
        assert_eq!(c.state(), Tri::None);
        c.toggle_all();
        assert_eq!(c.count(), 3);
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut c = Checklist::new(3, false);
        assert_eq!(c.cursor(), None);
        c.move_cursor(1);
        assert_eq!(c.cursor(), Some(0));
        c.move_cursor(1);
        c.move_cursor(1);
        c.move_cursor(1);
        assert_eq!(c.cursor(), Some(2));
        c.move_cursor(-5);
        assert_eq!(c.cursor(), Some(0));
        let mut c = Checklist::new(3, false);
        c.move_cursor(-1);
        assert_eq!(c.cursor(), Some(2));
    }

    #[test]
    fn space_toggles_cursor_row() {
        let mut c = Checklist::new(2, false);
        assert!(!c.toggle_cursor());
        c.set_cursor(1);
        assert!(c.toggle_cursor());
        assert_eq!(c.checked_indices(), vec![1]);
        c.set_cursor(99);
        assert_eq!(c.cursor(), Some(1));
    }

    #[test]
    fn empty_list_has_no_cursor() {
        let mut c = Checklist::new(0, false);
        c.move_cursor(1);
        assert_eq!(c.cursor(), None);
        c.set_cursor(0);
        assert_eq!(c.cursor(), None);
        assert!(!c.toggle_cursor());
    }

    #[test]
    fn reset_replaces_rows() {
        let mut c = Checklist::new(2, false);
        c.set_cursor(1);
        c.reset(4, true);
        assert_eq!(c.len(), 4);
        assert_eq!(c.count(), 4);
        assert_eq!(c.cursor(), None);
    }
}
