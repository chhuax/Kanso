//! 命令块（command-blocks.md）的纯逻辑：命中测试、可见行范围、选中状态机、右键菜单项。
//!
//! 块边界由核心随输出增量维护，经帧里的 [`CommandBlock`] 列表（只含可见范围内的块）传过来；
//! 块只是叠加在终端上的装饰，这里不碰网格内容。对照 mac 版 `TerminalKit/TerminalView+CommandBlocks`。

use kanso_frame::CommandBlock;

/// 复制块文本的部分（核心 `command_block_text` 的 `part` 参数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPart {
    /// 命令行。
    Command = 0,
    /// 输出。
    Output = 1,
    /// 整块（提示符、命令及输出）。
    All = 2,
}

/// 左侧色条的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stripe {
    /// 运行中：强调色细条。
    Running,
    /// 失败（退出码非 0）：红色。
    Failed,
}

/// 块是否“有命令”：没执行命令就重新出现提示符（空回车、Ctrl+C）的块不算，不可选中、不显示菜单项。
pub fn has_command(block: &CommandBlock) -> bool {
    block.can_copy_output
}

/// 块的色条；成功的块不画。
pub fn stripe(block: &CommandBlock) -> Option<Stripe> {
    if block.running {
        Some(Stripe::Running)
    } else if block.exit_code.is_some_and(|c| c != 0) {
        Some(Stripe::Failed)
    } else {
        None
    }
}

/// 包含绝对行 `abs_line` 的块。
pub fn block_at_line(blocks: &[CommandBlock], abs_line: u64) -> Option<&CommandBlock> {
    blocks
        .iter()
        .find(|b| b.start_line <= abs_line && abs_line <= b.end_line)
}

/// 块在视口内覆盖的行范围 `(首行, 末行)`（含）；视口是绝对行 `first_abs ..= first_abs + rows - 1`。
/// 不与视口相交返回 `None`。
pub fn visible_rows(block: &CommandBlock, first_abs: u64, rows: u16) -> Option<(u16, u16)> {
    if rows == 0 {
        return None;
    }
    let last_abs = first_abs + u64::from(rows) - 1;
    if block.end_line < first_abs || block.start_line > last_abs {
        return None;
    }
    let start = block.start_line.max(first_abs) - first_abs;
    let end = block.end_line.min(last_abs) - first_abs;
    Some((start as u16, end as u16))
}

/// 右键菜单里的块项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockMenu {
    /// “复制命令”是否可用（块起点被回滚裁掉后不可用）。
    pub copy_command: bool,
}

/// 右键点在块内时增加“复制命令 / 复制输出”；没有命令的块不加。
pub fn menu_for(block: Option<&CommandBlock>) -> Option<BlockMenu> {
    let b = block.filter(|b| has_command(b))?;
    Some(BlockMenu {
        copy_command: b.can_copy_command,
    })
}

/// 选中块的状态机。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockSelection {
    selected: Option<u64>,
}

impl BlockSelection {
    /// 当前选中的块 id。
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    /// 取消选中；返回是否有变化。
    pub fn clear(&mut self) -> bool {
        self.selected.take().is_some()
    }

    /// 直接选中（⌘↑ / ⌘↓ 跳转的目标）。
    pub fn select(&mut self, id: u64) {
        self.selected = Some(id);
    }

    /// 一次鼠标单击（`click_count` 为 1）落在 `hit` 上（`None` 为块外 / 全屏程序）。
    /// - 有命令的块：再次点击同一块取消，点击另一块改选；
    /// - 块外或无命令的块：取消；
    /// - 双击 / 三击不触发块选中（仍按文本选词 / 选行），状态不变。
    ///
    /// 返回是否有变化。
    pub fn click(&mut self, hit: Option<&CommandBlock>, click_count: usize) -> bool {
        if click_count != 1 {
            return false;
        }
        let before = self.selected;
        self.selected = match hit.filter(|b| has_command(b)) {
            Some(b) if before == Some(b.id) => None,
            Some(b) => Some(b.id),
            None => None,
        };
        before != self.selected
    }

    /// 进入备用屏幕时取消（退出后不恢复）。返回是否有变化。
    pub fn on_alt_screen(&mut self, alt: bool) -> bool {
        if alt { self.clear() } else { false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64, start: u64, end: u64) -> CommandBlock {
        CommandBlock {
            id,
            latest: false,
            start_line: start,
            end_line: end,
            command_line: Some(start),
            running: false,
            exit_code: Some(0),
            duration: None,
            can_copy_command: true,
            can_copy_output: true,
        }
    }

    #[test]
    fn stripe_kinds() {
        let mut b = block(1, 0, 3);
        assert_eq!(stripe(&b), None);
        b.exit_code = Some(2);
        assert_eq!(stripe(&b), Some(Stripe::Failed));
        b.running = true;
        b.exit_code = None;
        assert_eq!(stripe(&b), Some(Stripe::Running));
        // 没有退出码也没在运行（空回车）：不画。
        b.running = false;
        assert_eq!(stripe(&b), None);
    }

    #[test]
    fn hit_test_by_absolute_line() {
        let bs = [block(1, 10, 14), block(2, 15, 20)];
        assert_eq!(block_at_line(&bs, 9).map(|b| b.id), None);
        assert_eq!(block_at_line(&bs, 10).map(|b| b.id), Some(1));
        assert_eq!(block_at_line(&bs, 14).map(|b| b.id), Some(1));
        assert_eq!(block_at_line(&bs, 15).map(|b| b.id), Some(2));
        assert_eq!(block_at_line(&bs, 21).map(|b| b.id), None);
    }

    #[test]
    fn visible_rows_clip_to_viewport() {
        let b = block(1, 10, 14);
        // 视口 12..=21（10 行）：块的前两行已滚出。
        assert_eq!(visible_rows(&b, 12, 10), Some((0, 2)));
        // 完全在视口内。
        assert_eq!(visible_rows(&b, 5, 20), Some((5, 9)));
        // 块延伸到视口下方。
        assert_eq!(visible_rows(&b, 0, 12), Some((10, 11)));
        // 不相交。
        assert_eq!(visible_rows(&b, 15, 5), None);
        assert_eq!(visible_rows(&b, 0, 10), None);
        assert_eq!(visible_rows(&b, 0, 0), None);
    }

    #[test]
    fn click_selects_toggles_and_reselects() {
        let (a, b) = (block(1, 0, 3), block(2, 4, 8));
        let mut s = BlockSelection::default();
        assert!(s.click(Some(&a), 1));
        assert_eq!(s.selected(), Some(1));
        // 再次单击同一块：取消。
        assert!(s.click(Some(&a), 1));
        assert_eq!(s.selected(), None);
        // 选中后单击另一块：改选。
        s.click(Some(&a), 1);
        assert!(s.click(Some(&b), 1));
        assert_eq!(s.selected(), Some(2));
        // 块外：取消。
        assert!(s.click(None, 1));
        assert_eq!(s.selected(), None);
        // 已经没选中，块外点击不算变化。
        assert!(!s.click(None, 1));
    }

    #[test]
    fn empty_prompt_blocks_cannot_be_selected() {
        let mut empty = block(3, 9, 9);
        empty.command_line = None;
        empty.can_copy_output = false;
        empty.can_copy_command = false;
        let mut s = BlockSelection::default();
        assert!(!s.click(Some(&empty), 1));
        assert_eq!(s.selected(), None);
        // 选中了别的块后点无命令块：取消。
        s.select(1);
        assert!(s.click(Some(&empty), 1));
        assert_eq!(s.selected(), None);
    }

    #[test]
    fn double_and_triple_click_do_not_touch_selection() {
        let a = block(1, 0, 3);
        let mut s = BlockSelection::default();
        s.select(1);
        assert!(!s.click(Some(&a), 2));
        assert!(!s.click(None, 3));
        assert_eq!(s.selected(), Some(1));
    }

    #[test]
    fn alt_screen_clears_only_on_enter() {
        let mut s = BlockSelection::default();
        s.select(7);
        assert!(!s.on_alt_screen(false));
        assert_eq!(s.selected(), Some(7));
        assert!(s.on_alt_screen(true));
        assert_eq!(s.selected(), None);
        assert!(!s.on_alt_screen(true));
    }

    #[test]
    fn context_menu_items_only_inside_blocks_with_a_command() {
        let b = block(1, 0, 3);
        assert_eq!(menu_for(Some(&b)), Some(BlockMenu { copy_command: true }));
        let mut trimmed = block(2, 0, 3);
        trimmed.can_copy_command = false;
        assert_eq!(
            menu_for(Some(&trimmed)),
            Some(BlockMenu {
                copy_command: false
            })
        );
        let mut empty = block(3, 0, 0);
        empty.can_copy_output = false;
        assert_eq!(menu_for(Some(&empty)), None);
        assert_eq!(menu_for(None), None);
    }
}
