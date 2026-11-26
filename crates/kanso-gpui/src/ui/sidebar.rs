//! 左侧竖向标签栏（ui-shell.md §2）：顶部工具条（新建按钮）、按分屏分组的标签行。
//!
//! 标签行：左侧会话色图标、主标题（本地 shell 显示当前目录，过长保留末尾）、副标题
//! （状态或 Git 分支）、右侧未读 / 运行指示与关闭按钮。点击在鼠标按下时就切换。
//! 右键标签行弹出标签菜单（关闭 / 关闭其他 / 左侧 / 右侧 / 全部），标签栏空白处右键打开会话启动器。
//! 组内拖动先预览插入位置，松开时才改变顺序。

use gpui::{Context, Hsla, MouseButton, MouseDownEvent, div, prelude::*, px, rgb};
use kanso_gpui::app::i18n::t;
use kanso_gpui::app::status_bar::state_text;
use kanso_gpui::app::tab::{
    TabConnection, TabDescriptions, TabItem, TabKind, TabPresentation, truncate_keep_tail,
};
use kanso_gpui::widgets::style::number;
use kanso_types::message::Message;

use super::shell::{ShellView, Theme};
use super::widgets::icons::icon;

/// 没有配置颜色的会话从这组色里按 id 稳定取一个（同一个会话每次都是同一个颜色）。
const SESSION_COLORS: [u32; 8] = [
    0x4F8EF7, 0x35C48B, 0xE0A030, 0xE0607E, 0x9B6BDF, 0x2BB5C9, 0xE07B39, 0x7DB84A,
];

/// 平均一个字符约占的像素宽（13px 界面字体，中英混排的经验值），用来估算主标题能放几个字符。
pub(super) const CELL_PX: f32 = 7.4;

/// 左侧栏当前显示的页面：已打开的实时标签，或已保存的会话管理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarPage {
    Tabs,
    Sessions,
}

fn session_color(item: &TabItem) -> Hsla {
    let rgb_hex = match item.color {
        Some(c) => (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
        None => SESSION_COLORS[TabPresentation::color_index(item, SESSION_COLORS.len())],
    };
    rgb(rgb_hex).into()
}

impl ShellView {
    pub(super) fn cancel_tab_reorder(&mut self, cx: &mut Context<Self>) {
        if self.tab_reorder.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_sidebar(
        &self,
        width: f32,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ws = self.ctl.workspace();
        let home = self.ctl.home_dir();
        let sections = ws.sidebar_sections();
        let show_headers = ws.shows_section_headers();
        let active_pane = ws.active_pane();
        let active_session = ws.active_session();
        let tab_count: usize = sections.iter().map(|s| s.tabs.len()).sum();
        let has_unread = sections
            .iter()
            .flat_map(|s| s.tabs.iter())
            .any(|item| item.unread_done);
        let on_tabs = self.sidebar_page == SidebarPage::Tabs;
        let radius_sm = number("radius", "sm");

        // 顶部「标签 / 会话」双段切换栏：4px 外圆角 / 3px 选中块，与 macOS 侧保持一致。
        let mode_switch = div().px(px(8.0)).pt(px(8.0)).flex_none().child(
            div()
                .h(px(26.0))
                .p(px(2.0))
                .rounded(px(radius_sm))
                .bg(th.c("bg.field"))
                .border_1()
                .border_color(th.c("border.field"))
                .flex()
                .items_center()
                .child(
                    div()
                        .id("sidebar-tab-tabs")
                        .flex_1()
                        .h_full()
                        .rounded(px((radius_sm - 1.0).max(2.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(if on_tabs {
                            th.c("text.primary")
                        } else {
                            th.c("text.secondary")
                        })
                        .when(on_tabs, |d| {
                            d.bg(th.c("bg.tabSelected"))
                                .border_1()
                                .border_color(th.c("border.subtle"))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                if this.sidebar_page != SidebarPage::Tabs {
                                    this.show_sidebar_tabs(window, cx);
                                }
                            }),
                        )
                        .child(t(&Message::UiSidebarTabs {}))
                        .when(tab_count > 0, |d| {
                            d.child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(th.c("text.tertiary"))
                                    .child(tab_count.to_string()),
                            )
                        })
                        .when(!on_tabs && has_unread, |d| {
                            d.child(
                                div()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .bg(th.c("status.unreadDone")),
                            )
                        }),
                )
                .child(
                    div()
                        .id("sidebar-tab-sessions")
                        .flex_1()
                        .h_full()
                        .rounded(px((radius_sm - 1.0).max(2.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(if !on_tabs {
                            th.c("text.primary")
                        } else {
                            th.c("text.secondary")
                        })
                        .when(!on_tabs, |d| {
                            d.bg(th.c("bg.tabSelected"))
                                .border_1()
                                .border_color(th.c("border.subtle"))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                if this.sidebar_page != SidebarPage::Sessions {
                                    this.show_sidebar_sessions(false, window, cx);
                                }
                            }),
                        )
                        .child(t(&Message::UiSidebarSessions {})),
                ),
        );

        let resize_handle = div()
            .id("sidebar-resize")
            .absolute()
            .right_0()
            .top_0()
            .bottom_0()
            .w(px(5.))
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.sidebar_resizing = true;
                    cx.stop_propagation();
                }),
            );

        if !on_tabs {
            return div()
                .absolute()
                .left_0()
                .top(px(super::toolbar::HEIGHT))
                .bottom_0()
                .w(px(width))
                .flex()
                .flex_col()
                .bg(th.c("bg.sidebar"))
                .border_r_1()
                .border_color(th.c("border.subtle"))
                .child(mode_switch)
                .children(self.sidebar_sessions.clone())
                .child(resize_handle)
                .into_any_element();
        }

        let title_cells = (((width - 12.0 - 20.0 - 28.0 - 24.0 - 16.0) / CELL_PX) as usize).max(8);

        let mut list = div()
            .id("tab-list")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(th.c("border.subtle"))
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.cancel_tab_reorder(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.cancel_tab_reorder(cx);
                    if ev.click_count == 2 {
                        this.duplicate_active_tab_clicked(window, cx);
                    }
                }),
            );

        let query = self.path_filter.read(cx).text().to_owned();
        let mut visible_count = 0;
        let mut global_index = 0usize;
        for mut section in sections {
            if let Some(drag) = &self.tab_reorder
                && let Some(index) = drag.insertion
                && let Some(from) = section.tabs.iter().position(|item| item.id == drag.session)
            {
                let row = section.tabs.remove(from);
                section.tabs.insert(index.min(section.tabs.len()), row);
            }
            let pane = section.pane;
            if show_headers
                && section
                    .tabs
                    .iter()
                    .any(|item| TabPresentation::matches_path_filter(item, &query))
            {
                let is_active = pane == active_pane;
                list = list.child(
                    div()
                        .id(("section", pane.0 as usize))
                        .px(px(number("spacing", "tabRow.paddingX") + 8.0))
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
                                cx.stop_propagation();
                                this.activate_pane(pane, window, cx)
                            }),
                        )
                        .child(t(&Message::UiPane {
                            index: section.title_index as i64,
                        })),
                );
            }
            for item in &section.tabs {
                let index = global_index;
                global_index += 1;
                if !TabPresentation::matches_path_filter(item, &query) {
                    continue;
                }
                visible_count += 1;
                let selected = active_session == Some(item.id);
                // 选中且在活动分屏：完整高亮；选中但分屏不活动：淡一点，表示“该分屏当前显示它”。
                let in_active_pane = pane == active_pane;
                let title = if item.custom_title.is_some() {
                    TabPresentation::primary_title(item, home)
                } else {
                    truncate_keep_tail(&TabPresentation::primary_title(item, home), title_cells)
                };
                list = list.child(
                    div()
                        .px(px(number("spacing", "tabRow.paddingX")))
                        .py(px(number("spacing", "tabRow.gap") / 2.0))
                        .border_b_1()
                        .border_color(th.c("border.subtle"))
                        .child(self.render_row(
                            item,
                            index,
                            title,
                            selected,
                            in_active_pane,
                            ws.active_tab_in(pane) == Some(item.id),
                            None,
                            th,
                            cx,
                        )),
                );
            }
        }

        if visible_count == 0 && !query.trim().is_empty() {
            list = list.child(div().p(px(14.0)).text_color(th.c("text.secondary")).child(
                kanso_gpui::app::launcher::render_message(
                    &kanso_types::message::Message::UiNoMatchingPaths {},
                ),
            ));
        }
        div()
            .absolute()
            .left_0()
            .top(px(super::toolbar::HEIGHT))
            .bottom_0()
            .w(px(width))
            .flex()
            .flex_col()
            .bg(th.c("bg.sidebar"))
            .border_r_1()
            .border_color(th.c("border.subtle"))
            .child(mode_switch)
            // 顶部工具条：路径筛选 + 新建按钮。
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mr(px(4.0))
                            .child(self.path_filter.clone()),
                    )
                    .child(
                        div().flex().items_center().gap(px(2.0)).child(
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
                                .hover(|s| {
                                    s.bg(th.c("bg.rowHover")).text_color(th.c("text.primary"))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                        this.new_tab_clicked(window, cx)
                                    }),
                                )
                                .child(icon("plus", 16.0, th.c("text.secondary"))),
                        ),
                    ),
            )
            .child(list)
            .child(resize_handle)
            .into_any_element()
    }

    /// 一个标签。竖向侧栏与顶部标签条共用，保证两种位置的内容与状态样式一致。
    ///
    /// `chip` 为 `Some(宽度)` 时画成顶部标签条里的一枚标签（ui-shell.md §2.5）：定宽、单行，
    /// 副标题（状态或 Git 分支）放不下第二行，改进悬停提示；拖拽排序按横向计算。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_row(
        &self,
        item: &TabItem,
        index: usize,
        title: String,
        selected: bool,
        in_active_pane: bool,
        pane_current: bool,
        chip: Option<f32>,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let sid = item.id;
        let closed = matches!(item.connection, TabConnection::Closed(_));
        let accent = if closed {
            th.c("border.strong")
        } else {
            session_color(item)
        };
        let subtitle = TabDescriptions::state_line(item)
            .as_ref()
            .map(state_text)
            .or_else(|| closed.then(|| t(&Message::UiSessionDisconnected {})))
            .or_else(|| TabPresentation::subtitle(item, self.ctl.home_dir()));
        let busy = item.activity.is_busy();
        let mcp_mark = self.mcp.read(cx).tab_mark(sid).map(|(active, lines)| {
            let color = th.c(if active { "accent" } else { "text.tertiary" });
            div()
                .id(("tab-mcp", sid.raw() as usize))
                .flex_none()
                .child(icon("plug", 13.0, color))
                .tooltip(move |_, cx| {
                    let lines = lines.clone();
                    cx.new(|_| super::status_bar::LinesTooltip { lines, th })
                        .into()
                })
        });
        let (bg, show_bar) = if selected && in_active_pane {
            (Some(th.c("bg.tabSelected")), true)
        } else if pane_current {
            (Some(th.ca("bg.tabSelected", 0x80)), false)
        } else {
            (None, false)
        };
        // 标题始终用主文字色保持清晰，断开状态由左侧暗灰条与副标题表达。
        let title_color = th.c("text.primary");
        let sub_color = match &item.connection {
            TabConnection::Closed(Some(_)) => th.c("status.warning"),
            _ if item.unread_done => th.c("status.unreadDone"),
            _ if busy => th.c("status.running"),
            _ => th.c("text.secondary"),
        };
        let (_, has_digit) = TabPresentation::shortcut_digit(index);
        let mcp_addressable = item
            .profile
            .is_some_and(|profile| self.mcp.read(cx).can_address_tab(profile));
        let row_tooltip = (chip.is_some() || mcp_addressable || has_digit).then(|| {
            let mut lines = vec![title.clone()];
            if chip.is_some() {
                lines.extend(subtitle.clone());
            }
            if mcp_addressable {
                lines.push(t(&Message::WinMcpTabId {
                    id: format!("tab={}", sid.raw()),
                }));
            }
            if has_digit {
                lines.push(format!("Alt+{}", index + 1));
            }
            lines
        });
        let subtitle = if chip.is_some() { None } else { subtitle };

        let rect = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::<gpui::Pixels>::default()));
        let measured = rect.clone();
        let mut row = div()
            .id(("tab", sid.raw() as usize))
            .child(
                gpui::canvas(move |b, _, _| rect.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .when(
                self.tab_reorder
                    .as_ref()
                    .is_some_and(|drag| drag.session == sid && drag.insertion.is_some()),
                |row| {
                    // 插入指示线：竖向画在行的上沿，横向画在标签的左沿。
                    let line = div().absolute().top_0().left_0().bg(th.c("accent"));
                    row.child(if chip.is_some() {
                        line.bottom_0().w(px(2.))
                    } else {
                        line.right_0().h(px(2.))
                    })
                },
            )
            .group(gpui::SharedString::from(format!("tab-{}", sid.raw())))
            .relative()
            .h(px(if chip.is_some() {
                super::tab_strip::CHIP_HEIGHT
            } else {
                number("spacing", "tabRow.height")
            }))
            .when_some(chip, |row, w| row.w(px(w)))
            .when_some(row_tooltip, |row, lines| {
                row.tooltip(move |_, cx| {
                    let lines = lines.clone();
                    cx.new(|_| super::status_bar::LinesTooltip { lines, th })
                        .into()
                })
            })
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(number("spacing", "tabRow.paddingX")))
            .rounded(px(number("radius", "tabRow")))
            .cursor_pointer()
            .hover(|s| s.bg(th.c("bg.tabHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.cancel_tab_reorder(cx);
                    if ev.click_count == 2 {
                        this.open_rename_tab_dialog(sid, window, cx);
                        return;
                    }
                    this.activate_session(sid, window, cx);
                    // 竖栏筛选中只显示子集，插入位置对不上完整列表，不允许排序；标签条没有筛选。
                    if (chip.is_some() || this.path_filter.read(cx).text().trim().is_empty())
                        && let Some(pane) = this.ctl.workspace().pane_of(sid)
                    {
                        let order: Vec<_> = this
                            .ctl
                            .workspace()
                            .tabs_in(pane)
                            .into_iter()
                            .map(|t| t.id)
                            .collect();
                        if order.len() > 1 {
                            let row = measured.get();
                            use kanso_gpui::app::tab::TabReorder;
                            this.tab_reorder = Some(if chip.is_some() {
                                // 标签条里的标签等宽，相邻中点的间距就是宽度加间隔。
                                TabReorder::start(
                                    sid,
                                    order,
                                    ev.position.x.into(),
                                    (row.origin.x + row.size.width / 2.).into(),
                                    f32::from(row.size.width) + super::tab_strip::CHIP_GAP,
                                )
                                .horizontal()
                            } else {
                                TabReorder::start(
                                    sid,
                                    order,
                                    ev.position.y.into(),
                                    (row.origin.y + row.size.height / 2.).into(),
                                    f32::from(row.size.height) + number("spacing", "tabRow.gap"),
                                )
                            });
                        }
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_tab_menu(sid, ev.position, window, cx)
                }),
            );
        if let Some(bg) = bg {
            row = row.bg(bg);
        }
        if selected && in_active_pane {
            row = row.border_1().border_color(th.c("border.strong"));
        } else if pane_current {
            row = row.border_1().border_color(th.ca("border.strong", 0x80));
        }
        if show_bar {
            // 标签条里的标签比侧栏行矮，色条上下留白相应收窄。
            let bar_inset = if chip.is_some() { 6.0 } else { 9.0 };
            row = row.child(
                div()
                    .absolute()
                    .left(px(4.0))
                    .top(px(bar_inset))
                    .bottom(px(bar_inset))
                    .w(px(3.0))
                    .rounded(px(2.0))
                    .bg(accent),
            );
        }

        row.child(
            div()
                .flex_none()
                .w(px(24.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .opacity(if closed { 0.5 } else { 1.0 })
                .child(icon(
                    match item.kind {
                        TabKind::Local => "square-terminal",
                        TabKind::Ssh => "server",
                        TabKind::Sftp => "folder",
                    },
                    16.0,
                    th.c("text.secondary"),
                )),
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
                .opacity(0.0)
                .group_hover(
                    gpui::SharedString::from(format!("tab-{}", sid.raw())),
                    |s| s.opacity(1.0),
                )
                .child((index + 1).to_string())
        }))
        // 未读完成 / 运行中指示点。
        .children(mcp_mark)
        .children((!closed && (item.unread_done || busy)).then(|| {
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
                .opacity(0.0)
                .group_hover(
                    gpui::SharedString::from(format!("tab-{}", sid.raw())),
                    |s| s.opacity(1.0),
                )
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
                .child(icon("x", 12.0, th.c("text.secondary"))),
        )
        .into_any_element()
    }
}
