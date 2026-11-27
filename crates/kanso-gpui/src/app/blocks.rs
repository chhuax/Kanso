//! 命令块（command-blocks.md）的纯逻辑：命中测试、可见行范围、右键菜单项。
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
            foldable: end > start,
            folded: false,
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
