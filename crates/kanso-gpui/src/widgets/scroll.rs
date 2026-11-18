//! 单行输入框的水平滚动：文本比可视宽度长时，让光标始终可见。
//!
//! 所有量都是像素（f32）。`scroll` 是文本相对可视区左边缘向左移动的距离，范围 `0..=max_scroll`。

/// 光标距可视区边缘至少保留的余量，避免光标贴边被裁掉。
pub const MARGIN: f32 = 2.0;

/// 滚动上限：文本宽度（含末尾给光标留的 `caret_w`）超出可视宽度的部分。
pub fn max_scroll(text_w: f32, view_w: f32, caret_w: f32) -> f32 {
    (text_w + caret_w - view_w).max(0.0)
}

/// 返回保持光标可见的新滚动量。
///
/// - 光标在可视区右侧之外：向左滚到光标贴右缘；
/// - 光标在左侧之外：滚到光标贴左缘；
/// - 文本放得下时回到 0；
/// - 删除内容后滚动量超过上限时被收回。
pub fn follow_cursor(scroll: f32, cursor_x: f32, text_w: f32, view_w: f32, caret_w: f32) -> f32 {
    let max = max_scroll(text_w, view_w, caret_w);
    let mut s = scroll.clamp(0.0, max);
    if cursor_x - s < MARGIN {
        s = (cursor_x - MARGIN).max(0.0);
    } else if cursor_x + caret_w - s > view_w - MARGIN {
        s = cursor_x + caret_w - view_w + MARGIN;
    }
    s.clamp(0.0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_never_scrolls() {
        assert_eq!(follow_cursor(0.0, 50.0, 50.0, 200.0, 1.0), 0.0);
        assert_eq!(follow_cursor(30.0, 50.0, 50.0, 200.0, 1.0), 0.0);
    }

    #[test]
    fn scrolls_right_to_keep_cursor_visible() {
        let s = follow_cursor(0.0, 300.0, 300.0, 200.0, 1.0);
        assert!(s > 0.0);
        let x = 300.0 - s;
        assert!((0.0..=200.0).contains(&x));
        assert!(x + 1.0 <= 200.0 + 0.001);
    }

    #[test]
    fn scrolls_left_when_cursor_moves_back() {
        let s = follow_cursor(100.0, 40.0, 300.0, 200.0, 1.0);
        assert_eq!(s, 40.0 - MARGIN);
        assert_eq!(follow_cursor(100.0, 0.0, 300.0, 200.0, 1.0), 0.0);
    }

    #[test]
    fn scroll_is_clamped_after_shrink() {
        let s = follow_cursor(500.0, 120.0, 120.0, 200.0, 1.0);
        assert_eq!(s, 0.0);
    }

    #[test]
    fn cursor_inside_keeps_scroll() {
        assert_eq!(follow_cursor(60.0, 120.0, 400.0, 200.0, 1.0), 60.0);
    }
}
