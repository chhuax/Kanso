//! `ShellView` 与右侧服务器监控面板的接线（host-monitor.md、ui-shell.md §1）：
//!
//! 与 Git 改动面板共用右侧栏宽度（`self.git.layout`）与左边缘拖动条；
//! 同一时刻只显示一种内容，标题栏「改动」「监控」按钮互斥，再点当前项收起整栏。

use gpui::{
    AnyElement, Context, CursorStyle, Entity, MouseButton, MouseDownEvent, Subscription, Window,
    div, prelude::*, px,
};
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::monitor_panel::target_for;
use kanso_gpui::widgets::style::number;
use kanso_types::message::Message;

use super::git_panel::with_tip;
use super::monitor_panel::{MonitorEvent, MonitorPanelView};
use super::shell::{ShellView, Theme, status_bar_h};
use super::widgets::icons::icon;

/// 外壳持有的监控面板状态。
pub(super) struct MonitorState {
    panel: Entity<MonitorPanelView>,
    _panel_sub: Subscription,
    /// 当前是否由监控占据右侧栏（与 `git.layout.visible` 互斥）。
    pub(super) visible: bool,
}

impl MonitorState {
    pub(super) fn visible(&self) -> bool {
        self.visible
    }
}

pub(super) fn new_state(
    core: std::sync::Arc<kanso_core::Core>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> MonitorState {
    let panel = cx.new(|_| MonitorPanelView::new(core));
    let sub = cx.subscribe_in(
        &panel,
        window,
        |this, _panel, ev: &MonitorEvent, window, cx| match ev {
            MonitorEvent::Close => this.set_monitor_visible(false, window, cx),
        },
    );
    MonitorState {
        panel,
        _panel_sub: sub,
        visible: false,
    }
}

impl ShellView {
    /// 让监控面板跟随当前活动标签。
    pub(super) fn sync_monitor(&mut self, cx: &mut Context<Self>) {
        let ws = self.ctl.workspace();
        let item = ws.active_session().and_then(|s| ws.item(s));
        let target = target_for(item);
        self.monitor
            .panel
            .update(cx, |panel, cx| panel.set_target(target, cx));
    }

    /// 切换监控面板显隐；打开时互斥收起 Git 改动面板。
    pub(super) fn toggle_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = !self.monitor.visible;
        self.set_monitor_visible(next, window, cx);
    }

    pub(super) fn set_monitor_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if visible {
            self.git.layout_set_visible(false);
        }
        self.monitor.visible = visible;
        self.monitor
            .panel
            .update(cx, |panel, cx| panel.set_visible(visible, cx));
        if visible {
            self.sync_monitor(cx);
        } else {
            self.sync_focus(window, cx);
        }
        cx.notify();
    }

    /// 右侧栏实际占用宽度（Git 与监控共用同一宽度记忆）。
    pub(super) fn right_panel_width(&self, window: &Window) -> f32 {
        if self.git.visible() {
            return self.git_width(window);
        }
        if !self.monitor.visible {
            return 0.0;
        }
        let total = f32::from(window.viewport_size().width);
        let sidebar = self.sidebar_width(window);
        let mut layout = kanso_gpui::app::git_panel::GitLayout::with_saved_width(self.git.width());
        layout.visible = true;
        layout.effective_width(total - sidebar - self.filer_width(window))
    }

    /// 绘制右侧监控面板与左边缘拖动条。
    pub(super) fn render_monitor(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        if !self.monitor.visible {
            return Vec::new();
        }
        let w = self.right_panel_width(window);
        if w <= 0.0 {
            return Vec::new();
        }
        let total = f32::from(window.viewport_size().width);
        let x = total - w;
        let top = super::toolbar::HEIGHT;
        let h = (f32::from(window.viewport_size().height) - top - status_bar_h()).max(0.0);
        let panel = div()
            .id("monitor-host")
            .absolute()
            .left(px(x))
            .top(px(top))
            .w(px(w))
            .h(px(h))
            .border_l_1()
            .border_color(th.c("border.subtle"))
            .child(self.monitor.panel.clone());
        let handle = div()
            .id("monitor-resize")
            .absolute()
            .left(px(x - 3.0))
            .top(px(top))
            .w(px(6.0))
            .h(px(h))
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(move |s| s.bg(th.ca("accent", 0x55)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    this.start_right_drag(f32::from(ev.position.x), cx);
                    cx.stop_propagation();
                }),
            );
        vec![panel.into_any_element(), handle.into_any_element()]
    }

    /// 标题栏右侧的「监控」按钮（位于「改动」按钮右侧）。
    pub(super) fn monitor_status_item(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let on = self.monitor.visible;
        let color = if on {
            th.c("accent")
        } else {
            th.c("text.secondary")
        };
        let el = div()
            .id("status-monitor")
            .occlude()
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.0))
            .py(px(2.0))
            .rounded(px(number("radius", "md")))
            .cursor_pointer()
            .hover(move |s| s.bg(th.c("bg.rowHover")))
            .text_color(color)
            .child(icon("server", 13.0, color))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_monitor(window, cx)));
        with_tip(el, msg(Message::UiRightMonitor {}), th).into_any_element()
    }
}
