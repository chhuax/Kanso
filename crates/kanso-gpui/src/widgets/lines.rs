//! 多行文本框的行几何：按 `\n` 切行、偏移 ↔ 行号、纵向滚动（保持光标行可见、点击取行）。
//!
//! 所有偏移都是 UTF-8 字节偏移；所有像素量是 f32。视图层（`ui/widgets/text_area.rs`）只负责整形与绘制。

use std::ops::Range;

/// 每一行的字节范围（不含换行符）。空文本是一行空行；末尾的换行符后还有一行空行。
pub fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            out.push(start..i);
            start = i + 1;
        }
    }
    out.push(start..text.len());
    out
}

/// `offset` 所在的行号（换行符本身算在它结束的那一行）。
pub fn line_of(ranges: &[Range<usize>], offset: usize) -> usize {
    ranges
        .iter()
        .rposition(|r| r.start <= offset)
        .unwrap_or(0)
        .min(ranges.len().saturating_sub(1))
}

/// 纵向滚动量的合法范围上限。
pub fn max_scroll(total_h: f32, view_h: f32) -> f32 {
    (total_h - view_h).max(0.0)
}

/// 夹进合法范围。
pub fn clamp_scroll(scroll: f32, total_h: f32, view_h: f32) -> f32 {
    scroll.clamp(0.0, max_scroll(total_h, view_h))
}

/// 返回让第 `line` 行完整可见的滚动量（已经可见则不变）。
pub fn follow_line(scroll: f32, line: usize, line_h: f32, line_count: usize, view_h: f32) -> f32 {
    let total = line_count as f32 * line_h;
    let mut s = clamp_scroll(scroll, total, view_h);
    let top = line as f32 * line_h;
    let bottom = top + line_h;
    if top < s {
        s = top;
    } else if bottom > s + view_h {
        s = bottom - view_h;
    }
    clamp_scroll(s, total, view_h)
}

/// 控件内纵坐标 `y`（已加上滚动量）落在哪一行；越界取首 / 末行。
pub fn line_at_y(y: f32, line_h: f32, line_count: usize) -> usize {
    if y <= 0.0 || line_count == 0 {
        return 0;
    }
    ((y / line_h) as usize).min(line_count - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_every_line() {
        assert_eq!(line_ranges(""), vec![0..0]);
        assert_eq!(line_ranges("ab\ncd"), vec![0..2, 3..5]);
        assert_eq!(line_ranges("ab\n"), vec![0..2, 3..3]);
        assert_eq!(line_ranges("你\n好"), vec![0..3, 4..7]);
    }

    #[test]
    fn offsets_map_to_lines() {
        let r = line_ranges("ab\ncd\n");
        assert_eq!(line_of(&r, 0), 0);
        assert_eq!(line_of(&r, 2), 0);
        assert_eq!(line_of(&r, 3), 1);
        assert_eq!(line_of(&r, 6), 2);
        assert_eq!(line_of(&r, 99), 2);
    }

    #[test]
    fn follow_keeps_the_cursor_line_visible() {
        assert_eq!(follow_line(0.0, 2, 10.0, 10, 30.0), 0.0);
        assert_eq!(follow_line(0.0, 3, 10.0, 10, 30.0), 10.0);
        assert_eq!(follow_line(0.0, 9, 10.0, 10, 30.0), 70.0);
        assert_eq!(follow_line(70.0, 1, 10.0, 10, 30.0), 10.0);
        assert_eq!(follow_line(70.0, 0, 10.0, 2, 30.0), 0.0);
    }

    #[test]
    fn click_rows_clamp() {
        assert_eq!(line_at_y(-5.0, 10.0, 3), 0);
        assert_eq!(line_at_y(15.0, 10.0, 3), 1);
        assert_eq!(line_at_y(500.0, 10.0, 3), 2);
        assert_eq!(line_at_y(5.0, 10.0, 0), 0);
    }
}
