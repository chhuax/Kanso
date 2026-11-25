//! 顶部标签条（ui-shell.md §2.5）：标签栏位置为顶部时，每个分屏顶部各一条，只显示本分屏的标签。
//!
//! 标签与左侧竖栏共用 `render_row`（传入定宽），内容、状态样式与右键菜单都一致。标签等宽：
//! 放得下时取最大宽度，放不下先一起缩到最小宽度，再不够就横向滚动。等宽也让横向拖拽排序可以沿用
//! `TabReorder` 的"固定间距中点"模型。末尾固定一个 ＋ 按钮。

use gpui::{Context, MouseButton, MouseDownEvent, div, prelude::*, px};
use kanso_gpui::app::layout::{PaneId, Rect};
use kanso_gpui::app::shortcuts::AppCommand;
use kanso_gpui::app::tab::{TabPresentation, truncate_keep_tail};

use super::shell::{ShellView, Theme};
use super::sidebar::CELL_PX;
use super::widgets::icons::icon;

/// 标签条高度（含底部 1px 分隔线）。分屏里的终端从它下方开始。
pub(super) const HEIGHT: f32 = 34.0;
/// 单个标签的高度。
pub(super) const CHIP_HEIGHT: f32 = 26.0;
/// 相邻标签的间隔。拖拽排序按"标签宽 + 间隔"推算其他标签的中点，改这里两边会一起变。
pub(super) const CHIP_GAP: f32 = 4.0;
const CHIP_MIN_WIDTH: f32 = 110.0;
const CHIP_MAX_WIDTH: f32 = 200.0;
const INSET_X: f32 = 6.0;
const PLUS_WIDTH: f32 = 28.0;
/// 标签里除主标题外占掉的宽度：左右内边距、图标、间隔、快捷键序号与关闭按钮。用来估算标题能放几个字符。
const CHIP_CHROME: f32 = 12.0 * 2.0 + 24.0 + 8.0 * 3.0 + 20.0 + 10.0;

/// 标签条里 `count` 个标签的统一宽度。`strip_width` 是整条标签条的宽度（含 ＋ 按钮）。
fn chip_width(strip_width: f32, count: usize) -> f32 {
    if count == 0 {
        return CHIP_MAX_WIDTH;
    }
    let n = count as f32;
    let available = strip_width - PLUS_WIDTH - INSET_X * 2.0 - CHIP_GAP * (n - 1.0);
    (available / n)
        .clamp(CHIP_MIN_WIDTH, CHIP_MAX_WIDTH)
        .floor()
}

impl ShellView {
    /// 所有分屏的标签条（窗口坐标）。不显示标签条，或分屏没有标签时，对应分屏不画。
    pub(super) fn render_tab_strips(
        &self,
        area: Rect,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        if !self.tab_strips_shown() {
            return Vec::new();
        }
        let frames = self
            .ctl
            .workspace()
            .layout()
            .frames(area, kanso_gpui::app::shell::DIVIDER_THICKNESS);
        let mut panes: Vec<_> = frames.into_iter().collect();
        // 固定顺序，避免 HashMap 的遍历顺序让元素 id 的出现次序每帧不同。
        panes.sort_by_key(|(pane, _)| pane.0);
        panes
            .into_iter()
            .filter(|(pane, _)| !self.ctl.workspace().tabs_in(*pane).is_empty())
            .map(|(pane, r)| self.render_tab_strip(pane, r, th, cx))
            .collect()
    }

    fn render_tab_strip(
        &self,
        pane: PaneId,
        r: Rect,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ws = self.ctl.workspace();
        let home = self.ctl.home_dir();
        let active_pane = ws.active_pane();
        let active_session = ws.active_session();
        let pane_current = ws.active_tab_in(pane);
        let order = ws.sidebar_order();
        let mut tabs = ws.tabs_in(pane);
        // 拖拽预览：被拖的标签挪到插入位置，其他标签随之让位；松开时才真正改顺序。
        if let Some(drag) = &self.tab_reorder
            && let Some(index) = drag.insertion
            && let Some(from) = tabs.iter().position(|item| item.id == drag.session)
        {
            let row = tabs.remove(from);
            tabs.insert(index.min(tabs.len()), row);
        }
        let width = chip_width(r.width as f32, tabs.len());
        let title_cells = (((width - CHIP_CHROME) / CELL_PX) as usize).max(4);

        let mut list = div()
            .id(("tab-strip", pane.0 as usize))
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .flex()
            .items_center()
            .gap(px(CHIP_GAP))
            .px(px(INSET_X))
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.cancel_tab_reorder(cx)))
            // 空白处：单击切到本分屏，双击复制当前标签（先切分屏，复制的才是这个分屏的标签）。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.cancel_tab_reorder(cx);
                    this.activate_pane(pane, window, cx);
                    if ev.click_count == 2 {
                        this.duplicate_active_tab_clicked(window, cx);
                    }
                }),
            )
            // 空白处右键：会话启动器。先切分屏，从启动器打开的会话才会落在这里。
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.activate_pane(pane, window, cx);
                    this.run_command(AppCommand::OpenLauncher, window, cx)
                }),
            );
        for item in &tabs {
            // 快捷键序号按完整侧栏顺序编号，与 Alt+N 的实际目标一致。
            let index = order.iter().position(|id| *id == item.id).unwrap_or(0);
            let title = if item.custom_title.is_some() {
                TabPresentation::primary_title(item, home)
            } else {
                truncate_keep_tail(&TabPresentation::primary_title(item, home), title_cells)
            };
            list = list.child(self.render_row(
                item,
                index,
                title,
                active_session == Some(item.id),
                pane == active_pane,
                pane_current == Some(item.id),
                Some(width),
                th,
                cx,
            ));
        }

        div()
            .id(("tab-strip-bar", pane.0 as usize))
            .absolute()
            .left(px(r.x as f32))
            .top(px(r.y as f32))
            .w(px(r.width as f32))
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .bg(th.c("bg.sidebar"))
            .border_b_1()
            .border_color(th.c("border.subtle"))
            .child(list)
            .child(
                div()
                    .id(("tab-strip-new", pane.0 as usize))
                    .flex_none()
                    .w(px(24.0))
                    .h(px(24.0))
                    .mx(px(2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(kanso_gpui::widgets::style::number("radius", "sm")))
                    .cursor_pointer()
                    .hover(|s| s.bg(th.c("bg.rowHover")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.activate_pane(pane, window, cx);
                            this.new_tab_clicked(window, cx)
                        }),
                    )
                    .child(icon("plus", 16.0, th.c("text.secondary"))),
            )
            .into_any_element()
    }

    /// 复制当前标签（标签栏空白处双击，ui-shell.md §2.4 / §3.3）。
    pub(super) fn duplicate_active_tab_clicked(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match self
            .ctl
            .duplicate_active_tab(kanso_gpui::app::controller::FALLBACK_GRID)
        {
            Ok(id) => {
                self.attach_session(id, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => self.show_notice(kanso_gpui::app::launcher::core_error_text(&e), cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_share_one_width_clamped_between_min_and_max() {
        assert_eq!(chip_width(1200.0, 2), CHIP_MAX_WIDTH);
        assert_eq!(chip_width(300.0, 5), CHIP_MIN_WIDTH);
        let w = chip_width(700.0, 4);
        assert!(w > CHIP_MIN_WIDTH && w < CHIP_MAX_WIDTH);
        // 四个标签加间隔正好放进去，不需要滚动。
        assert!(INSET_X * 2.0 + w * 4.0 + CHIP_GAP * 3.0 + PLUS_WIDTH <= 700.0);
        assert_eq!(chip_width(500.0, 0), CHIP_MAX_WIDTH);
    }
}
