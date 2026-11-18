//! 撤销 / 重做栈。
//!
//! 输入框的文本很短，所以直接存**整串快照**（内容 + 选区），实现简单且不会出错。
//! 连续输入 / 连续删除按 [`EditKind`] 合并成一步，光标移动或换种操作就另起一步。

/// 一次快照：内容与选区（`anchor` / `head` 是字节偏移，光标在 `head`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// 文本内容。
    pub text: String,
    /// 选区锚点。
    pub anchor: usize,
    /// 光标。
    pub head: usize,
}

/// 编辑种类，决定相邻编辑能否合并成一个撤销步骤。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// 连续输入字符。
    Insert,
    /// 连续向后删除（Backspace）。
    Backspace,
    /// 连续向前删除（Delete）。
    Delete,
    /// 其他（粘贴、剪切、按词删除、输入法上屏等），永远独立成步。
    Other,
}

/// 撤销栈最多保留的步数。
pub const MAX_STEPS: usize = 200;

/// 撤销 / 重做栈。
#[derive(Clone, Debug, Default)]
pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<EditKind>,
}

impl UndoStack {
    /// 在**修改之前**调用，传入修改前的快照。返回是否真的压栈（合并时不压）。
    pub fn record(&mut self, before: Snapshot, kind: EditKind) -> bool {
        self.redo.clear();
        let merge = kind != EditKind::Other && self.last == Some(kind) && !self.undo.is_empty();
        self.last = Some(kind);
        if merge {
            return false;
        }
        self.undo.push(before);
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
        }
        true
    }

    /// 打断合并（光标移动、失焦等）。
    pub fn break_group(&mut self) {
        self.last = None;
    }

    /// 撤销：传入当前快照（压入重做栈），返回要恢复的快照。
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.last = None;
        Some(prev)
    }

    /// 重做：传入当前快照（压入撤销栈），返回要恢复的快照。
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.last = None;
        Some(next)
    }

    /// 清空历史（外部整体替换内容时）。
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }

    /// 能否撤销。
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// 能否重做。
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(t: &str) -> Snapshot {
        Snapshot {
            text: t.into(),
            anchor: t.len(),
            head: t.len(),
        }
    }

    #[test]
    fn consecutive_inserts_merge() {
        let mut u = UndoStack::default();
        assert!(u.record(snap(""), EditKind::Insert));
        assert!(!u.record(snap("a"), EditKind::Insert));
        assert!(!u.record(snap("ab"), EditKind::Insert));
        assert_eq!(u.undo(snap("abc")), Some(snap("")));
        assert!(!u.can_undo());
        assert!(u.can_redo());
    }

    #[test]
    fn kind_change_and_break_start_new_step() {
        let mut u = UndoStack::default();
        u.record(snap(""), EditKind::Insert);
        assert!(u.record(snap("a"), EditKind::Backspace));
        u.break_group();
        assert!(u.record(snap("b"), EditKind::Backspace));
        assert!(u.record(snap("c"), EditKind::Other));
        assert!(u.record(snap("d"), EditKind::Other));
    }

    #[test]
    fn redo_restores_and_new_edit_clears_redo() {
        let mut u = UndoStack::default();
        u.record(snap("a"), EditKind::Other);
        let back = u.undo(snap("ab")).unwrap();
        assert_eq!(back, snap("a"));
        assert_eq!(u.redo(snap("a")), Some(snap("ab")));
        assert!(u.undo(snap("ab")).is_some());
        u.record(snap("a"), EditKind::Other);
        assert!(!u.can_redo());
    }

    #[test]
    fn history_is_capped() {
        let mut u = UndoStack::default();
        for i in 0..MAX_STEPS + 50 {
            u.record(snap(&i.to_string()), EditKind::Other);
        }
        let mut n = 0;
        while u.undo(snap("x")).is_some() {
            n += 1;
        }
        assert_eq!(n, MAX_STEPS);
    }

    #[test]
    fn undo_on_empty_is_none() {
        let mut u = UndoStack::default();
        assert!(u.undo(snap("")).is_none());
        assert!(u.redo(snap("")).is_none());
    }
}
