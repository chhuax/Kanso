//! 左侧竖向标签栏（ui-shell.md §2）：顶部工具条（新建按钮）、按分屏分组的标签行、底部快捷键提示。
//!
//! 标签行：左侧会话色图标、主标题（本地 shell 显示当前目录，过长保留末尾）、副标题
//! （状态或 Git 分支）、右侧未读 / 运行指示与关闭按钮。点击在鼠标按下时就切换。
//! 暂未实现：路径搜索框与过滤按钮、拖拽排序、右键菜单（菜单控件由控件组提供）、悬停提示。

use gpui::{Context, Hsla, MouseButton, MouseDownEvent, div, prelude::*, px, rgb};
use kanso_gpui::app::shortcuts::{AppCommand, shortcut_text};
use kanso_gpui::app::tab::{
    CloseNote, TabConnection, TabDescriptions, TabItem, TabKind, TabPresentation, TabStateLine,
    truncate_keep_tail,
};
use kanso_gpui::widgets::style::number;

use super::shell::{ShellView, Theme};

/// 没有配置颜色的会话从这组色里按 id 稳定取一个（同一个会话每次都是同一个颜色）。
const SESSION_COLORS: [u32; 8] = [
    0x4F8EF7, 0x35C48B, 0xE0A030, 0xE0607E, 0x9B6BDF, 0x2BB5C9, 0xE07B39, 0x7DB84A,
];

/// 平均一个字符约占的像素宽（13px 界面字体，中英混排的经验值），用来估算主标题能放几个字符。
const CELL_PX: f32 = 7.4;

fn session_color(item: &TabItem) -> Hsla {
    let rgb_hex = match item.color {
        Some(c) => (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
        None => SESSION_COLORS[TabPresentation::color_index(item, SESSION_COLORS.len())],
    };
    rgb(rgb_hex).into()
}

fn kind_glyph(kind: TabKind) -> &'static str {
    match kind {
        TabKind::Local => ">_",
        TabKind::Ssh => "SH",
        TabKind::Sftp => "FT",
    }
}

/// 状态行文字（侧栏副标题）。
fn state_text(line: &TabStateLine) -> String {
    match line {
        TabStateLine::Connecting => "连接中…".to_owned(),
        TabStateLine::Closed(Some(CloseNote::Exited { code: Some(c) })) => {
            format!("会话已结束（退出码 {c}）")
        }
        TabStateLine::Closed(Some(CloseNote::Exited { code: None })) => "会话已结束".to_owned(),
        // TODO(l10n)：用 kanso-l10n 渲染错误消息。
        TabStateLine::Closed(Some(CloseNote::Error(_))) => "会话已断开".to_owned(),
        TabStateLine::Closed(None) => "会话已结束".to_owned(),
        TabStateLine::CommandRunning => "命令运行中".to_owned(),
        TabStateLine::AgentRunning => "AI 运行中".to_owned(),
        TabStateLine::AgentFinished => "AI 已完成".to_owned(),
        TabStateLine::UnreadDone => "命令已完成，点击查看".to_owned(),
    }
}

impl ShellView {
    pub(super) fn render_sidebar(&self, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let width = number("spacing", "sidebar.defaultWidth");
        let ws = self.ctl.workspace();
        let home = self.ctl.home_dir();
        let sections = ws.sidebar_sections();
        let show_headers = ws.shows_section_headers();
        let active_pane = ws.active_pane();
        let active_session = ws.active_session();
        let title_cells = (((width - 12.0 - 20.0 - 28.0 - 24.0 - 16.0) / CELL_PX) as usize).max(8);

        let mut list = div()
            .id("tab-list")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(6.0))
            .py(px(4.0));

        let mut global_index = 0usize;
        for section in sections {
            let pane = section.pane;
            if show_headers {
                let is_active = pane == active_pane;
                list = list.child(
                    div()
                        .id(("section", pane.0 as usize))
                        .px(px(8.0))
                        .pt(px(8.0))
                        .pb(px(2.0))
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(if is_active {
                            th.c("accent")
                        } else {
                            th.c("text.tertiary")
                        })
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.activate_pane(pane, window, cx)
                            }),
                        )
                        .child(format!("分屏 {}", section.title_index)),
                );
            }
            for item in &section.tabs {
                let selected = active_session == Some(item.id);
                // 选中且在活动分屏：完整高亮；选中但分屏不活动：淡一点，表示“该分屏当前显示它”。
                let in_active_pane = pane == active_pane;
                let title =
                    truncate_keep_tail(&TabPresentation::primary_title(item, home), title_cells);
                list = list.child(self.render_row(
                    item,
                    global_index,
                    title,
                    selected,
                    in_active_pane,
                    ws.active_tab_in(pane) == Some(item.id),
                    th,
                    cx,
                ));
                global_index += 1;
            }
        }

        let new_hint = shortcut_text(AppCommand::NewLocalTab).unwrap_or_default();
        div()
            .absolute()
            .left_0()
            .top_0()
            .h_full()
            .w(px(width))
            .flex()
            .flex_col()
            .bg(th.c("bg.sidebar"))
            .border_r_1()
            .border_color(th.c("border.subtle"))
            // 顶部工具条：标题 + 新建按钮。
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .child(
                        div()
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("text.secondary"))
                            .child(format!("会话 · {}", ws.tab_count())),
                    )
                    .child(
                        div()
                            .id("new-tab")
                            .w(px(24.0))
                            .h(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(number("radius", "sm")))
                            .text_size(px(16.0))
                            .text_color(th.c("text.secondary"))
                            .cursor_pointer()
                            .hover(|s| s.bg(th.c("bg.rowHover")).text_color(th.c("text.primary")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                    this.new_tab_clicked(window, cx)
                                }),
                            )
                            .child("+"),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex_none()
                    .px(px(14.0))
                    .py(px(8.0))
                    .border_t_1()
                    .border_color(th.c("border.subtle"))
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("text.tertiary"))
                    .child(format!("{new_hint} 新建标签")),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        item: &TabItem,
        index: usize,
        title: String,
        selected: bool,
        in_active_pane: bool,
        pane_current: bool,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let sid = item.id;
        let accent = session_color(item);
        let subtitle = TabDescriptions::state_line(item)
            .as_ref()
            .map(state_text)
            .or_else(|| TabPresentation::subtitle(item).map(str::to_owned));
        let closed = matches!(item.connection, TabConnection::Closed(_));
        let busy = item.activity.is_busy();
        let (bg, show_bar) = if selected && in_active_pane {
            (Some(th.c("bg.tabSelected")), true)
        } else if pane_current {
            (Some(th.ca("bg.tabSelected", 0x80)), false)
        } else {
            (None, false)
        };
        let title_color = if closed {
            th.c("text.tertiary")
        } else {
            th.c("text.primary")
        };
        let sub_color = match &item.connection {
            TabConnection::Closed(Some(_)) => th.c("status.warning"),
            _ if item.unread_done => th.c("status.unreadDone"),
            _ if busy => th.c("status.running"),
            _ => th.c("text.secondary"),
        };
        let (_, has_digit) = TabPresentation::shortcut_digit(index);

        let mut row = div()
            .id(("tab", sid.raw() as usize))
            .relative()
            .h(px(number("spacing", "tabRow.height")))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(number("spacing", "tabRow.paddingX") - 2.0))
            .rounded(px(number("radius", "tabRow")))
            .cursor_pointer()
            .hover(|s| s.bg(th.c("bg.tabHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.activate_session(sid, window, cx)
                }),
            );
        if let Some(bg) = bg {
            row = row.bg(bg);
        }
        if show_bar {
            row = row.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(10.0))
                    .bottom(px(10.0))
                    .w(px(3.0))
                    .rounded(px(2.0))
                    .bg(th.c("accent")),
            );
        }

        row.child(
            // 会话色图标。
            div()
                .flex_none()
                .w(px(28.0))
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(number("radius", "md")))
                .bg(Hsla { a: 0.2, ..accent })
                .text_size(px(11.0))
                .text_color(accent)
                .opacity(if closed { 0.5 } else { 1.0 })
                .child(kind_glyph(item.kind)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(number("font", "ui.tabTitle")))
                        .text_color(title_color)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(title),
                )
                .children(subtitle.map(|sub| {
                    div()
                        .text_size(px(number("font", "ui.tabSubtitle")))
                        .text_color(sub_color)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(sub)
                })),
        )
        // 前 9 个标签的快捷键序号（Alt+N）。
        .children(has_digit.then(|| {
            div()
                .flex_none()
                .text_size(px(number("font", "ui.caption")))
                .text_color(th.c("text.tertiary"))
                .child((index + 1).to_string())
        }))
        // 未读完成 / 运行中指示点。
        .children((item.unread_done || busy).then(|| {
            div()
                .flex_none()
                .w(px(6.0))
                .h(px(6.0))
                .rounded_full()
                .bg(if item.unread_done {
                    th.c("status.unreadDone")
                } else {
                    th.c("status.running")
                })
        }))
        .child(
            // 关闭按钮：鼠标按下即关闭，不让按下事件冒泡成“切换标签”。
            div()
                .id(("close", sid.raw() as usize))
                .flex_none()
                .w(px(20.0))
                .h(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(number("radius", "sm")))
                .text_size(px(14.0))
                .text_color(th.c("text.tertiary"))
                .hover(|s| s.bg(th.c("bg.rowHover")).text_color(th.c("text.primary")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.close_tab_clicked(sid, window, cx)
                    }),
                )
                .child("×"),
        )
        .into_any_element()
    }
}
