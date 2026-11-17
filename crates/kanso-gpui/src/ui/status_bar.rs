//! 底部状态栏（ui-shell.md §9）：内容与布局规则在 `kanso_gpui::app::status_bar`，这里只画。
//!
//! 左侧：Filer 开关、时间戳栏开关（两者的功能还没有接进外壳，先置灰显示，位置与 mac 版一致）、
//! 当前目录、状态文字；右侧：Legacy SSH 标记（悬停显示每台服务器用的旧算法）与时钟。
//! 空间不足时状态文字先让位（被裁掉），再压缩目录（保留末尾）。

use gpui::{Context, Render, Window, div, prelude::*, px};
use kanso_gpui::app::status_bar::{StatusBarModel, StatusTone, fit_directory};

use super::shell::{ShellView, Theme, status_bar_h};

/// 悬停提示：每行一台服务器及它用的旧算法。
struct LinesTooltip {
    lines: Vec<String>,
    th: Theme,
}

impl Render for LinesTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(kanso_gpui::widgets::style::number("radius", "md")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("border.strong"))
            .text_size(px(12.0))
            .text_color(th.c("text.primary"))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(self.lines.iter().map(|l| div().child(l.clone())))
    }
}

/// 左侧的“开关”条目（`on` 为开时用强调色）。
fn toggle_item(
    id: &'static str,
    label: &'static str,
    th: Theme,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(kanso_gpui::widgets::style::number("radius", "md")))
        .text_color(if on {
            th.c("accent")
        } else {
            th.c("text.secondary")
        })
        .child(label)
}

impl ShellView {
    pub(super) fn render_status_bar(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ws = self.ctl.workspace();
        let active = ws.active_session().and_then(|s| ws.item(s));
        let model = StatusBarModel::build(
            active,
            self.ctl.home_dir(),
            self.ctl.active_legacy_summary(),
            self.clock.clone(),
        );
        // AI 会话里开关只作用于当前会话，所以按活动会话的实际模式判断。
        let timeline_on = self.gutter_policy.is_on(ws.active_session());
        let width = f32::from(window.viewport_size().width);
        let legacy_visible = model.legacy.is_some();

        let status = model.status.map(|(text, tone)| {
            let color = match tone {
                StatusTone::Normal => th.c("text.secondary"),
                StatusTone::Warning => th.c("status.warning"),
                StatusTone::Error => th.c("status.error"),
            };
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(color)
                .child(text)
        });
        let directory = model.directory.map(|d| {
            div()
                .id("status-directory")
                .flex_none()
                .whitespace_nowrap()
                .text_color(th.c("text.secondary"))
                .child(fit_directory(&d, width, legacy_visible))
        });
        let legacy = model.legacy.map(|lines| {
            div()
                .id("status-legacy")
                .flex_none()
                .px(px(6.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(th.c("status.legacySsh"))
                .text_color(th.c("status.legacySsh"))
                .child("Legacy SSH")
                .tooltip(move |_window, cx| {
                    let lines = lines.clone();
                    cx.new(|_| LinesTooltip { lines, th }).into()
                })
        });

        // 重复发送在运行而面板收起时，给一个可点击的提示（sender.md D7）。
        let sender_chip = self
            .sender
            .chip
            .clone()
            .filter(|_| !self.sender.visible)
            .map(|text| {
                div()
                    .id("status-sender")
                    .flex_none()
                    .px(px(6.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(th.c("accent"))
                    .text_color(th.c("accent"))
                    .cursor_pointer()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                            this.toggle_sender(window, cx)
                        }),
                    )
                    .child(text)
            });

        div()
            .id("status-bar")
            .absolute()
            .left_0()
            .bottom_0()
            .w_full()
            .h(px(status_bar_h()))
            .flex()
            .items_center()
            .gap(px(16.0))
            .px(px(12.0))
            .bg(th.c("bg.statusBar"))
            .border_t_1()
            .border_color(th.c("border.subtle"))
            .text_size(px(kanso_gpui::widgets::style::number("font", "ui.caption")))
            // Filer 开关的功能还没有接进外壳，先置灰显示。
            .child(toggle_item("status-filer", "文件", th, false).opacity(0.5))
            .child(
                toggle_item("status-timeline", "时间戳", th, timeline_on)
                    .cursor_pointer()
                    .hover(|d| d.bg(th.c("bg.rowHover")))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_timeline(cx))),
            )
            .children(directory)
            // 状态文字没有内容时也要占住中间的弹性空间，把右侧元素推到最右。
            .child(status.unwrap_or_else(|| div().flex_1()))
            .children(sender_chip)
            .children(legacy)
            .child(
                div()
                    .flex_none()
                    .text_color(th.c("text.secondary"))
                    .child(model.clock),
            )
            .into_any_element()
    }
}
